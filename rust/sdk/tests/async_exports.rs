//! X8c: async exports on ABI 1.1 rounds, driven natively through
//! `GuestMachine` with real frames and a small host simulator that follows
//! the X8b broker rules (tape of plan hashes and outcomes, `replayed` must
//! equal the tape hashes, a yield in round `max_rounds + 1` is `Exhausted`).
//! The same guest behaviours run through the real Wasmi kernel and broker in
//! `axiom-runtime/extensions/fixtures/async-conformance`.

use axiom_extension_sdk::{
    abi::{
        self, AbiVersion, CodecLimits, Effect, EffectOutcome, EffectPlan, ErrorCode,
        ExtensionError, Frame, GuestMessage, HostMessage, Invocation, InvocationResult,
        PatchOperation, RequestId, RoundPlan, RoundRecord, Snapshot, Value, ABI_VERSION,
        ABI_VERSION_1_1,
    },
    async_rt::{factory_abi_version, segment_plan, FactoryOutput, MAX_GROUP_BRANCHES},
    async_rt::try_join_all,
    join, join_all,
    prelude::*,
    try_join,
    raw::GuestMachine,
    AsyncExtension, Binding, Extension,
};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};

const LIMITS: CodecLimits = CodecLimits {
    max_bytes: 1024 * 1024,
};

const fn op(operation: &'static str) -> Operation<Value, u64> {
    Operation::generated(Binding {
        namespace: "catalog",
        operation,
        input: "Value",
        output: "Unsigned",
    })
}

const LOAD: Operation<Value, u64> = op("load");
const PRICE: Operation<Value, u64> = op("price");
const RESERVE: Operation<Value, u64> = op("reserve");
const FAIL: Operation<Value, u64> = op("fail");
const NAME: Operation<Value, String> = Operation::generated(Binding {
    namespace: "catalog",
    operation: "load",
    input: "Value",
    output: "String",
});

fn prev(value: Option<u64>) -> Value {
    Value::Record(vec![abi::Field {
        name: "prev".into(),
        value: value.map(Value::Unsigned).unwrap_or(Value::Null),
    }])
}

fn n(i: u64) -> Value {
    Value::Unsigned(i)
}

static LATE: AtomicU64 = AtomicU64::new(0);
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A future that is never ready and is not a host operation.
struct Forever;
impl Future for Forever {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

/// Poll a future exactly once (a hand-written "cancelled branch").
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    let mut future = Some(future);
    std::future::poll_fn(move |context| {
        let polled = Pin::new(future.take().unwrap()).poll(context);
        Poll::Ready(match polled {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        })
    })
    .await
}

fn depth<'a>(cx: &'a Cx, left: u64) -> Pin<Box<dyn Future<Output = Result<u64>> + 'a>> {
    Box::pin(async move {
        if left == 0 {
            return Ok(0);
        }
        let here = cx.call(LOAD.call(n(left))).await?;
        Ok(here + depth(cx, left - 1).await?)
    })
}

#[derive(Default)]
struct Guest {
    held: Option<Ticket<u64>>,
}

#[extension]
impl Guest {
    /// The ADR scenario: load, price(load), reserve(price), patch the sum.
    #[export]
    async fn three(cx: &Cx, _input: Value) -> Result<Value> {
        let a = cx.call(LOAD.call(prev(None))).await?;
        let b = cx.call(PRICE.call(prev(Some(a)))).await?;
        let c = cx.call(RESERVE.call(prev(Some(b)))).await?;
        // Final patch against the round-1 snapshot, as in a sync export.
        let checkout = UiState::<Checkout>::generated(cx.invocation())?;
        cx.patch(checkout.patch().generated_set(&COUNT, a + b + c));
        Ok(Value::Record(vec![
            abi::Field {
                name: "outcomes".into(),
                value: Value::List(vec![n(a), n(b), n(c)]),
            },
            abi::Field {
                name: "round".into(),
                value: n(u64::from(cx.round())),
            },
        ]))
    }

    /// w2-c3: fail-fast `try_join!` (TS `Promise.all`, Python `gather`).
    #[export]
    async fn try_join_ok(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let (a, b) = try_join!(cx.call(LOAD.call(n(1))), cx.call(PRICE.call(n(2)))).await?;
        Ok(vec![a, b])
    }

    #[export]
    async fn try_join_fails_fast(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let chain = async {
            let x = cx.call(LOAD.call(n(1))).await?;
            let y = cx.call(LOAD.call(n(x))).await?;
            cx.call(LOAD.call(n(y))).await
        };
        let (a, b) = try_join!(cx.call(FAIL.call(n(1))), chain).await?;
        Ok(vec![a, b])
    }

    /// The error is caught: the dropped branch's operation registered in the
    /// failing pass still runs with the next round; its third never registers.
    #[export]
    async fn try_join_caught(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let chain = async {
            let x = cx.call(LOAD.call(n(1))).await?;
            let y = cx.call(LOAD.call(n(x))).await?;
            cx.call(LOAD.call(n(y))).await
        };
        let failed = try_join!(cx.call(FAIL.call(n(1))), chain).await.is_err();
        let p = cx.call(PRICE.call(n(7))).await?;
        Ok(vec![u64::from(failed), p])
    }

    #[export]
    async fn try_join_all_fails_fast(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let ok = try_join_all((1..=3).map(|i| cx.call(LOAD.call(n(i))))).await?;
        let failed = try_join_all([cx.call(PRICE.call(n(4))), cx.call(FAIL.call(n(5)))]).await;
        Ok(ok.into_iter().chain([u64::from(failed.is_err())]).collect())
    }

    #[export]
    async fn join2(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let (a, b) = join!(cx.call(PRICE.call(n(1))), cx.call(RESERVE.call(n(2)))).await;
        Ok(vec![a?, b?])
    }

    #[export]
    async fn join5(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let (a, b, c, d, e) = join!(
            cx.call(LOAD.call(n(1))),
            cx.call(PRICE.call(n(2))),
            cx.call(RESERVE.call(n(3))),
            cx.call(LOAD.call(n(4))),
            cx.call(PRICE.call(n(5))),
        )
        .await;
        Ok(vec![a?, b?, c?, d?, e?])
    }

    /// join!(join!(a, b), c) is one group of three, in order a, b, c.
    #[export]
    async fn nested(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let ((a, b), c) = join!(
            join!(cx.call(LOAD.call(n(1))), cx.call(PRICE.call(n(2)))),
            cx.call(RESERVE.call(n(3))),
        )
        .await;
        Ok(vec![a?, b?, c?])
    }

    /// join!(seq(load, price), reserve): round 1 = [load, reserve], round 2 = [price].
    #[export]
    async fn mixed(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let chain = async {
            let a = cx.call(LOAD.call(n(1))).await?;
            cx.call(PRICE.call(n(a))).await
        };
        let (b, c) = join!(chain, cx.call(RESERVE.call(n(3)))).await;
        let d = cx.call(LOAD.call(n(9))).await?;
        Ok(vec![b?, c?, d])
    }

    /// N sequential awaits, N from the input.
    #[export]
    async fn sequential(cx: &Cx, count: u64) -> Result<u64> {
        let mut total = 0;
        for i in 0..count {
            total += cx.call(LOAD.call(n(i))).await?;
        }
        Ok(total)
    }

    /// N awaits at once (one group), N from the input.
    #[export]
    async fn fanout(cx: &Cx, count: u64) -> Result<u64> {
        let results = join_all((0..count).map(|i| cx.call(LOAD.call(n(i))))).await;
        results.into_iter().sum()
    }

    /// Branches on state that survives between runs in this instance.
    #[export]
    async fn nondeterministic(cx: &Cx, _input: Value) -> Result<u64> {
        let first = cx.call(LOAD.call(n(0))).await?;
        let seen = COUNTER.fetch_add(1, Ordering::SeqCst);
        let next = if seen.is_multiple_of(2) { &PRICE } else { &RESERVE };
        Ok(first + cx.call(next.call(n(1))).await?)
    }

    /// Diverges only on its fourth run (round 4 of a three-yield
    /// invocation), after the instance has memoised the earlier plan hashes.
    #[export]
    async fn late_divergence(cx: &Cx, _input: Value) -> Result<u64> {
        let seen = LATE.fetch_add(1, Ordering::SeqCst);
        let first = if seen == 3 { &PRICE } else { &LOAD };
        let a = cx.call(first.call(n(0))).await?;
        let b = cx.call(LOAD.call(n(1))).await?;
        let c = cx.call(LOAD.call(n(2))).await?;
        Ok(a + b + c)
    }

    /// A `Value` input with `cx.input()` and `cx.invocation().input()` also
    /// used (W2A-1): the input is still moved into the argument, and the
    /// context decodes it again from the round's frame, so all three see it.
    #[export]
    async fn value_input(cx: &Cx, input: Value) -> Result<Value> {
        let again = cx.input::<Value>()?;
        let a = cx.call(LOAD.call(input.clone())).await?;
        Ok(Value::List(vec![
            input,
            n(a),
            again,
            cx.invocation().input::<Value>()?,
            Value::Bool(cx.__input_moved()),
        ]))
    }

    /// A `Value` input whose context is used only for host operations: the
    /// input is moved into the argument (H-1, the cost win) in every round.
    #[export]
    async fn moved_value_input(cx: &Cx, input: Value) -> Result<Value> {
        let a = cx.call(LOAD.call(input.clone())).await?;
        Ok(Value::List(vec![input, n(a), Value::Bool(cx.__input_moved())]))
    }

    /// The context handed to a helper that reads the input (W2A-1): the
    /// helper sees it.
    #[export]
    async fn value_input_in_helper(cx: &Cx, input: Value) -> Result<Value> {
        fn read(cx: &Cx) -> Result<Value> {
            cx.input()
        }
        let a = cx.call(LOAD.call(input.clone())).await?;
        Ok(Value::List(vec![input, n(a), read(cx)?]))
    }

    /// The same through a qualified path.
    #[export]
    async fn qualified_value_input(_cx: &Cx, input: abi::Value) -> Result<Value> {
        Ok(input)
    }

    /// A typed input is decoded by reference, as before: `cx.input()` and
    /// `cx.invocation().input()` see it too.
    #[export]
    async fn typed_input(cx: &Cx, input: u64) -> Result<Vec<u64>> {
        let again: u64 = cx.input()?;
        let via_invocation: u64 = cx.invocation().input()?;
        let a = cx.call(LOAD.call(n(input))).await?;
        Ok(vec![input, again, via_invocation, a])
    }

    /// Branches on the round number before an await.
    #[export]
    async fn round_dependent(cx: &Cx, _input: Value) -> Result<u64> {
        let next = if cx.round() == 1 { &PRICE } else { &RESERVE };
        let a = cx.call(next.call(n(1))).await?;
        Ok(a + cx.call(LOAD.call(n(2))).await?)
    }

    /// A registered operation dropped before the stall stays in the plan.
    #[export]
    async fn dropped(cx: &Cx, _input: Value) -> Result<u64> {
        let mut abandoned = cx.call(PRICE.call(n(1)));
        assert!(poll_once(&mut abandoned).await.is_none());
        drop(abandoned);
        cx.call(LOAD.call(n(2))).await
    }

    /// Returns while an operation is registered: it is never executed.
    #[export]
    async fn early_return(cx: &Cx, _input: Value) -> Result<u64> {
        let mut pending = cx.call(PRICE.call(n(1)));
        assert!(poll_once(&mut pending).await.is_none());
        Ok(7)
    }

    /// Polling a completed operation again gives an error, not a panic.
    #[export]
    async fn twice(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let mut call = cx.call(LOAD.call(n(1)));
        let first = (&mut call).await?;
        let again = poll_once(&mut call).await;
        match again {
            Some(Err(error)) if error.code == ErrorCode::InvalidInput => Ok(vec![first, 1]),
            _ => Ok(vec![first, 0]),
        }
    }

    /// A future created in one task and awaited in another registers when
    /// first polled (in the other task), not where it was created.
    #[export]
    async fn moved(cx: &Cx, _input: Value) -> Result<Vec<u64>> {
        let created_here = cx.call(PRICE.call(n(1)));
        #[allow(clippy::redundant_async_block)] // the point: another task polls it
        let other_task = async move { created_here.await };
        let (a, b) = join!(cx.call(LOAD.call(n(2))), other_task).await;
        Ok(vec![a?, b?])
    }

    #[export]
    async fn recursive(cx: &Cx, levels: u64) -> Result<u64> {
        depth(cx, levels).await
    }

    /// An operation failure with `?` fails the invocation with its code.
    #[export]
    async fn op_error(cx: &Cx, _input: Value) -> Result<u64> {
        let a = cx.call(LOAD.call(n(1))).await?;
        Ok(a + cx.call(FAIL.call(n(2))).await?)
    }

    /// A handled operation failure: the export continues.
    #[export]
    async fn handled(cx: &Cx, _input: Value) -> Result<Value> {
        let outcome = match cx.call(FAIL.call(n(1))).await {
            Ok(_) => "ok".to_string(),
            Err(error) => format!("{:?}:{}", error.code, error.message),
        };
        let after = cx.call(LOAD.call(n(2))).await?;
        Ok(Value::List(vec![Value::String(outcome), n(after)]))
    }

    /// A failing branch in a group gets its error, the others their values.
    #[export]
    async fn group_error(cx: &Cx, _input: Value) -> Result<Value> {
        let (a, b, c) = join!(
            cx.call(LOAD.call(n(1))),
            cx.call(FAIL.call(n(2))),
            cx.call(PRICE.call(n(3))),
        )
        .await;
        Ok(Value::List(vec![
            n(a?),
            Value::Bool(b.is_err()),
            n(c?),
        ]))
    }

    /// The declared output type is enforced: `load` returns Unsigned here.
    #[export]
    async fn wrong_type(cx: &Cx, _input: Value) -> Result<String> {
        cx.call(NAME.call(n(1))).await
    }

    #[export]
    async fn foreign(cx: &Cx, _input: Value) -> Result<u64> {
        let a = cx.call(LOAD.call(n(1))).await?;
        Forever.await;
        Ok(a)
    }

    #[export]
    async fn panics(cx: &Cx, _input: Value) -> Result<u64> {
        let _ = cx.call(LOAD.call(n(1))).await?;
        panic!("pricing table is missing tier `gold`")
    }

    /// A sync export in the same module: 1.0 tickets, invoke then resume.
    #[export]
    fn sync_lookup(&mut self, _invocation: TypedInvocation) -> Result<ExtensionResponse> {
        let mut effects = Effects::new();
        self.held = Some(effects.push(LOAD.call(n(5))));
        Ok(ExtensionResponse::yield_for(effects))
    }

    #[export]
    fn sync_pure(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
        Ok(ExtensionResponse::new(invocation.input::<u64>()? + 1))
    }
}

impl TypedExtension for SyncOnly {
    fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
        Ok(ExtensionResponse::new(invocation.input::<u64>()? * 2))
    }
}

#[derive(Default)]
struct SyncOnly;

struct Checkout;
impl UiScope for Checkout {
    const SCOPE: &'static str = "checkout";
}
const COUNT: UiField<u64> = UiField::generated("count");

// ------------------------------------------------------------------- the host

type Tamper = Box<dyn FnMut(u16, &mut Vec<RoundRecord>)>;

struct Host {
    machine: GuestMachine<Box<dyn Extension>>,
    abi: AbiVersion,
    next_id: u64,
    replies: BTreeMap<&'static str, std::result::Result<Value, ExtensionError>>,
    calls: Vec<Effect>,
    /// Test hook: alter the tape before round `n` is sent.
    tamper: Option<Tamper>,
}

#[derive(Debug)]
struct Run {
    result: std::result::Result<InvocationResult, ExtensionError>,
    plans: Vec<RoundPlan>,
    invokes: u16,
}

impl Run {
    fn output(&self) -> &Value {
        &self.result.as_ref().expect("completed").output
    }
    fn error(&self) -> &ExtensionError {
        self.result.as_ref().expect_err("failed")
    }
}

fn host_error(code: ErrorCode, message: &str) -> ExtensionError {
    ExtensionError {
        code,
        message: message.into(),
        retryable: false,
    }
}

impl Host {
    fn new(extension: Box<dyn Extension>, offer: Vec<AbiVersion>) -> Self {
        let mut host = Self {
            machine: GuestMachine::new(extension, LIMITS),
            abi: ABI_VERSION,
            next_id: 1,
            replies: BTreeMap::from([
                ("load", Ok(n(10))),
                ("price", Ok(n(20))),
                ("reserve", Ok(n(30))),
                ("fail", Err(host_error(ErrorCode::Host, "catalog item is unavailable"))),
            ]),
            calls: Vec::new(),
            tamper: None,
        };
        let negotiated = host.send(HostMessage::Negotiate { supported: offer });
        let GuestMessage::Negotiated { selected } = negotiated else {
            panic!("{negotiated:?}")
        };
        host.abi = selected;
        let initialized = host.send(HostMessage::Initialize {
            instance_id: 1,
            application: "x8c".into(),
            extension: "async".into(),
        });
        assert_eq!(initialized, GuestMessage::Initialized);
        host
    }

    fn rounds(extension: impl Into<Box<dyn Extension>>) -> Self {
        Self::new(extension.into(), vec![ABI_VERSION_1_1, ABI_VERSION])
    }

    fn send(&mut self, message: HostMessage) -> GuestMessage {
        let negotiating = matches!(message, HostMessage::Negotiate { .. });
        let frame = if negotiating || self.abi == ABI_VERSION {
            Frame::new(message)
        } else {
            Frame::with_abi(self.abi, message)
        };
        let bytes = abi::encode(&frame, LIMITS).unwrap();
        let reply = self
            .machine
            .dispatch(&bytes)
            .unwrap_or_else(|error| panic!("{error:?}: {:?}", self.machine.last_error()));
        let accepted = if negotiating { ABI_VERSION } else { self.abi };
        let frame: Frame<GuestMessage> =
            abi::decode_frame_versioned(&reply, LIMITS, &[accepted]).expect("guest frame version");
        frame.message
    }

    fn execute(&mut self, plan: &RoundPlan) -> Vec<EffectOutcome> {
        let effects: Vec<Effect> = match plan {
            RoundPlan::Plan(plan) => plan.effects.clone(),
            RoundPlan::Group(group) => {
                assert_eq!(group.mode, abi::EffectGroupMode::Collect);
                assert!((1..=8).contains(&group.limit));
                group.branches.iter().map(|b| b.effect.clone()).collect()
            }
        };
        effects
            .into_iter()
            .enumerate()
            .map(|(index, effect)| {
                let result = self.replies[effect.operation.as_str()].clone();
                self.calls.push(effect);
                EffectOutcome {
                    index: index as u32,
                    result,
                }
            })
            .collect()
    }

    fn invocation(export: &str, input: Value) -> Invocation {
        Invocation {
            export: export.into(),
            input,
            snapshots: vec![Snapshot {
                resource: "ui:checkout".into(),
                revision: 1,
                value: Value::Null,
            }],
            deadline_unix_ms: 1,
        }
    }

    /// One invocation through ABI 1.1 rounds, like the X8b broker.
    fn invoke(&mut self, export: &str, input: Value, max_rounds: u16) -> Run {
        assert_eq!(self.abi, ABI_VERSION_1_1, "rounds need a 1.1 session");
        let request_id = RequestId(self.next_id);
        self.next_id += 1;
        let invocation = Self::invocation(export, input);
        let mut tape: Vec<RoundRecord> = Vec::new();
        let mut plans = Vec::new();
        for round in 1..=abi::last_round(max_rounds) {
            let mut sent = tape.clone();
            if let Some(tamper) = self.tamper.as_mut() {
                tamper(round, &mut sent);
            }
            let reply = self.send(HostMessage::InvokeRound {
                request_id,
                invocation: invocation.clone(),
                round,
                max_rounds,
                tape: sent.clone(),
            });
            let expected: Vec<_> = sent.iter().map(|record| record.plan_hash).collect();
            match reply {
                GuestMessage::CompletedRound {
                    request_id: id,
                    round: r,
                    replayed,
                    result,
                } => {
                    assert_eq!((id, r), (request_id, round));
                    if replayed != expected {
                        return Run {
                            result: Err(host_error(ErrorCode::Protocol, "non-deterministic guest")),
                            plans,
                            invokes: round,
                        };
                    }
                    return Run {
                        result: Ok(result),
                        plans,
                        invokes: round,
                    };
                }
                GuestMessage::YieldedRound {
                    request_id: id,
                    round: r,
                    replayed,
                    plan,
                } => {
                    assert_eq!((id, r), (request_id, round));
                    assert_eq!(replayed, expected, "replayed must equal the tape hashes");
                    if round == abi::last_round(max_rounds) {
                        let cancelled = self.send(HostMessage::Cancel { request_id });
                        assert_eq!(cancelled, GuestMessage::Cancelled { request_id });
                        return Run {
                            result: Err(host_error(ErrorCode::Exhausted, "maxRounds exceeded")),
                            plans,
                            invokes: round,
                        };
                    }
                    let outcomes = self.execute(&plan);
                    tape.push(RoundRecord {
                        plan_hash: abi::round_plan_hash(&plan),
                        outcomes,
                        events: Vec::new(),
                    });
                    plans.push(plan);
                }
                GuestMessage::Failed { request_id: id, error } => {
                    assert_eq!(id, request_id);
                    return Run {
                        result: Err(error),
                        plans,
                        invokes: round,
                    };
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        unreachable!("the loop returns in the last round")
    }
}

fn guest() -> Host {
    Host::rounds(Guest::axiom_extension_for_tests())
}

impl Guest {
    fn axiom_extension_for_tests() -> AsyncExtension {
        axiom_extension()
    }
}

fn effect(operation: &str, input: Value) -> Effect {
    Effect {
        namespace: "catalog".into(),
        operation: operation.into(),
        input,
    }
}

fn plan_of(operations: &[(&str, u64)]) -> RoundPlan {
    segment_plan(
        operations
            .iter()
            .map(|(operation, input)| effect(operation, n(*input)))
            .collect(),
    )
}

fn list(values: &[u64]) -> Value {
    Value::List(values.iter().copied().map(n).collect())
}

// ---------------------------------------------------------------------- tests

#[test]
fn factory_abi_version_is_1_1_only_for_async_extensions() {
    assert_eq!(factory_abi_version(&axiom_extension), ABI_VERSION_1_1.packed());
    assert_eq!(factory_abi_version(&|| boxed_extension(SyncOnly)), ABI_VERSION.packed());
    struct Raw;
    impl Extension for Raw {
        fn invoke(&mut self, request_id: RequestId, _: Invocation) -> GuestMessage {
            axiom_extension_sdk::raw::completed(request_id, Value::Null)
        }
    }
    assert_eq!(factory_abi_version(&|| Box::new(Raw)), ABI_VERSION.packed());
    assert_eq!(<AsyncExtension as FactoryOutput>::ABI, ABI_VERSION_1_1);
    assert_eq!(AXIOM_EXTENSION_EXPORTS.len(), 32);
    assert!(AXIOM_EXTENSION_EXPORTS.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn three_round_scenario_yields_three_plans_and_completes_in_round_four() {
    let mut host = guest();
    let run = host.invoke("three", Value::Null, 3);
    assert_eq!(run.invokes, 4);
    assert_eq!(
        run.plans,
        vec![
            segment_plan(vec![effect("load", prev(None))]),
            segment_plan(vec![effect("price", prev(Some(10)))]),
            segment_plan(vec![effect("reserve", prev(Some(20)))]),
        ]
    );
    assert!(run.plans.iter().all(|plan| matches!(plan, RoundPlan::Plan(p) if p.effects.len() == 1)));
    let result = run.result.unwrap();
    assert_eq!(
        result.output,
        Value::Record(vec![
            abi::Field { name: "outcomes".into(), value: list(&[10, 20, 30]) },
            abi::Field { name: "round".into(), value: n(4) },
        ])
    );
    assert_eq!(result.patches.len(), 1);
    assert_eq!(result.patches[0].expected_revision, 1);
    assert_eq!(
        result.patches[0].operations,
        vec![PatchOperation::Set { path: vec!["count".into()], value: n(60) }]
    );
    // maxRounds = 2 allows two yields: the third is Exhausted, nothing completes.
    let run = host.invoke("three", Value::Null, 2);
    assert_eq!(run.error().code, ErrorCode::Exhausted);
    assert_eq!(run.invokes, 3);
}

#[test]
fn join_of_two_and_five_is_exactly_one_collect_group_round() {
    let mut host = guest();
    let run = host.invoke("join2", Value::Null, 1);
    assert_eq!(run.invokes, 2);
    assert_eq!(run.plans, vec![plan_of(&[("price", 1), ("reserve", 2)])]);
    let RoundPlan::Group(group) = &run.plans[0] else { panic!() };
    assert_eq!(group.mode, abi::EffectGroupMode::Collect);
    assert_eq!(group.limit, 2);
    assert_eq!(group.format, abi::EFFECT_GROUP_FORMAT);
    assert_eq!(
        group.branches.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        ["b0", "b1"]
    );
    assert_eq!(run.output(), &list(&[20, 30]));

    let run = host.invoke("join5", Value::Null, 1);
    assert_eq!(run.invokes, 2);
    assert_eq!(
        run.plans,
        vec![plan_of(&[("load", 1), ("price", 2), ("reserve", 3), ("load", 4), ("price", 5)])]
    );
    assert_eq!(run.output(), &list(&[10, 20, 30, 10, 20]));
}

#[test]
fn nested_join_is_one_group_in_argument_order() {
    let mut host = guest();
    let run = host.invoke("nested", Value::Null, 1);
    assert_eq!(run.plans, vec![plan_of(&[("load", 1), ("price", 2), ("reserve", 3)])]);
    assert_eq!(run.output(), &list(&[10, 20, 30]));
}

#[test]
fn sequential_awaits_take_one_round_each_and_mixed_programs_batch() {
    let mut host = guest();
    let run = host.invoke("sequential", n(3), 3);
    assert_eq!(run.invokes, 4);
    assert_eq!(
        run.plans,
        vec![plan_of(&[("load", 0)]), plan_of(&[("load", 1)]), plan_of(&[("load", 2)])]
    );
    assert_eq!(run.output(), &n(30));

    let run = host.invoke("mixed", Value::Null, 3);
    assert_eq!(
        run.plans,
        vec![
            plan_of(&[("load", 1), ("reserve", 3)]),
            plan_of(&[("price", 10)]),
            plan_of(&[("load", 9)]),
        ]
    );
    assert_eq!(run.output(), &list(&[20, 30, 10]));
}

#[test]
fn loop_of_n_awaits_against_max_rounds_16() {
    let mut host = guest();
    for (count, invokes) in [(0, 1), (1, 2), (16, 17)] {
        let run = host.invoke("sequential", n(count), 16);
        assert_eq!(run.invokes, invokes, "count {count}");
        assert_eq!(run.output(), &n(10 * count));
    }
    let run = host.invoke("sequential", n(17), 16);
    assert_eq!(run.error().code, ErrorCode::Exhausted);
    assert_eq!(run.invokes, 17);
    assert_eq!(run.plans.len(), 16);
}

#[test]
fn fan_out_is_one_group_up_to_the_branch_limit() {
    let mut host = guest();
    let run = host.invoke("fanout", n(16), 1);
    assert_eq!(run.invokes, 2);
    let RoundPlan::Group(group) = &run.plans[0] else { panic!() };
    assert_eq!((group.branches.len(), group.limit), (16, 8));
    assert_eq!(run.output(), &n(160));
    let run = host.invoke("fanout", n(MAX_GROUP_BRANCHES as u64 + 1), 1);
    assert_eq!(run.error().code, ErrorCode::Exhausted);
    assert!(run.error().message.contains("at most 32"), "{}", run.error().message);
    let run = host.invoke("fanout", n(0), 1);
    assert_eq!((run.invokes, run.output()), (1, &n(0)));
}

#[test]
fn non_deterministic_guests_are_caught_on_replay() {
    let mut host = guest();
    let run = host.invoke("nondeterministic", Value::Null, 2);
    assert_eq!(run.error().code, ErrorCode::Protocol);
    assert!(run.error().message.contains("non-deterministic"), "{}", run.error().message);

    let run = host.invoke("round_dependent", Value::Null, 2);
    assert_eq!(run.error().code, ErrorCode::Protocol);
    assert!(run.error().message.contains("effect round 1"), "{}", run.error().message);
}

#[test]
fn an_altered_tape_hash_or_order_is_a_protocol_failure() {
    let mut host = guest();
    host.tamper = Some(Box::new(|round, tape| {
        if round == 3 {
            tape[1].plan_hash[0] ^= 1;
        }
    }));
    let run = host.invoke("three", Value::Null, 3);
    assert_eq!(run.error().code, ErrorCode::Protocol);
    assert!(run.error().message.contains("effect round 2"));

    let mut host = guest();
    host.tamper = Some(Box::new(|round, tape| {
        if round == 3 {
            tape.swap(0, 1);
        }
    }));
    assert_eq!(host.invoke("three", Value::Null, 3).error().code, ErrorCode::Protocol);
}

#[test]
fn a_dropped_registered_operation_stays_in_its_round() {
    let mut host = guest();
    let run = host.invoke("dropped", Value::Null, 1);
    // The abandoned `price` and `load` were pending at the same stall.
    assert_eq!(run.plans, vec![plan_of(&[("price", 1), ("load", 2)])]);
    assert_eq!(run.output(), &n(10));
    assert_eq!(host.calls.len(), 2, "the dropped operation is executed, its result discarded");
}

#[test]
fn early_return_never_plans_the_pending_operation() {
    let mut host = guest();
    let run = host.invoke("early_return", Value::Null, 1);
    assert_eq!((run.invokes, run.output()), (1, &n(7)));
    assert!(run.plans.is_empty());
    assert!(host.calls.is_empty());
}

#[test]
fn polling_a_completed_operation_again_is_an_error() {
    let mut host = guest();
    let run = host.invoke("twice", Value::Null, 1);
    assert_eq!(run.output(), &list(&[10, 1]));
    assert_eq!(run.plans.len(), 1);
}

#[test]
fn a_moved_future_registers_where_it_is_first_polled() {
    let mut host = guest();
    let run = host.invoke("moved", Value::Null, 1);
    assert_eq!(run.plans, vec![plan_of(&[("load", 2), ("price", 1)])]);
    assert_eq!(run.output(), &list(&[10, 20]));
}

#[test]
fn recursion_awaits_one_round_per_level() {
    let mut host = guest();
    let run = host.invoke("recursive", n(3), 3);
    assert_eq!(run.invokes, 4);
    assert_eq!(
        run.plans,
        vec![plan_of(&[("load", 3)]), plan_of(&[("load", 2)]), plan_of(&[("load", 1)])]
    );
    assert_eq!(run.output(), &n(30));
}

#[test]
fn operation_errors_propagate_with_question_mark_or_can_be_handled() {
    let mut host = guest();
    let run = host.invoke("op_error", Value::Null, 2);
    assert_eq!(run.error().code, ErrorCode::Host);
    assert_eq!(run.error().message, "catalog item is unavailable");
    assert_eq!(run.invokes, 3);

    let run = host.invoke("handled", Value::Null, 2);
    assert_eq!(
        run.output(),
        &Value::List(vec![Value::String("Host:catalog item is unavailable".into()), n(10)])
    );

    let run = host.invoke("group_error", Value::Null, 1);
    assert_eq!(run.plans.len(), 1);
    assert_eq!(run.output(), &Value::List(vec![n(10), Value::Bool(true), n(20)]));
}

#[test]
fn outputs_are_decoded_with_the_declared_type() {
    let mut host = guest();
    let run = host.invoke("wrong_type", Value::Null, 1);
    assert_eq!(run.error().code, ErrorCode::InvalidInput);
}

#[test]
fn awaiting_a_foreign_future_fails_with_invalid_input() {
    let mut host = guest();
    let run = host.invoke("foreign", Value::Null, 2);
    assert_eq!(run.error().code, ErrorCode::InvalidInput);
    assert!(run.error().message.contains("not a host operation"));
    assert_eq!(run.invokes, 2);
}

#[test]
fn cancellation_between_rounds_clears_the_request() {
    let mut host = guest();
    let request_id = RequestId(77);
    let invocation = Host::invocation("three", Value::Null);
    let first = host.send(HostMessage::InvokeRound {
        request_id,
        invocation,
        round: 1,
        max_rounds: 3,
        tape: vec![],
    });
    assert!(matches!(first, GuestMessage::YieldedRound { .. }));
    assert_eq!(
        host.send(HostMessage::Cancel { request_id }),
        GuestMessage::Cancelled { request_id }
    );
    // The machine is ready for a new invocation.
    assert_eq!(host.invoke("join2", Value::Null, 1).output(), &list(&[20, 30]));
}

#[test]
fn sync_exports_work_in_a_mixed_module_on_1_1() {
    let mut host = guest();
    let run = host.invoke("sync_pure", n(41), 1);
    assert_eq!((run.invokes, run.output()), (1, &n(42)));
    let run = host.invoke("sync_lookup", Value::Null, 1);
    // Typed resume path: the sync export yields once, then resumes from instance memory.
    assert_eq!(run.invokes, 2);
    assert_eq!(run.plans, vec![RoundPlan::Plan(EffectPlan { effects: vec![effect("load", n(5))] })]);
    // Round 2 reached the typed `resume` (an `#[extension]` impl keeps the
    // default one, which refuses, exactly as on ABI 1.0).
    assert_eq!(run.error().code, ErrorCode::InvalidInput);
    assert!(run.error().message.contains("resumable"), "{}", run.error().message);
}

#[test]
fn async_exports_on_a_1_0_host_may_yield_once() {
    let mut host = Host::new(axiom_extension().into(), vec![ABI_VERSION]);
    assert_eq!(host.abi, ABI_VERSION);
    let id = RequestId(5);
    let yielded = host.send(HostMessage::Invoke {
        request_id: id,
        invocation: Host::invocation("join2", Value::Null),
    });
    let GuestMessage::YieldedGroup { group, .. } = yielded else { panic!("{yielded:?}") };
    let outcomes = host.execute(&RoundPlan::Group(group));
    let done = host.send(HostMessage::Resume { request_id: id, outcomes, events: vec![] });
    let GuestMessage::Completed { result, .. } = done else { panic!("{done:?}") };
    assert_eq!(result.output, list(&[20, 30]));

    let id = RequestId(6);
    let yielded = host.send(HostMessage::Invoke {
        request_id: id,
        invocation: Host::invocation("three", Value::Null),
    });
    let GuestMessage::Yielded { plan, .. } = yielded else { panic!("{yielded:?}") };
    let outcomes = host.execute(&RoundPlan::Plan(plan));
    let second = host.send(HostMessage::Resume { request_id: id, outcomes, events: vec![] });
    let GuestMessage::Failed { error, .. } = second else { panic!("{second:?}") };
    assert_eq!(error.code, ErrorCode::Exhausted);
    assert!(error.message.contains("ABI 1.0"));
}

#[test]
fn sync_only_extensions_still_negotiate_1_0_with_identical_frames() {
    let offer = vec![ABI_VERSION_1_1, ABI_VERSION];
    let mut host = Host::new(boxed_extension(SyncOnly), offer);
    assert_eq!(host.abi, ABI_VERSION);
    let request_id = RequestId(3);
    let bytes = abi::encode(
        &Frame::new(HostMessage::Invoke {
            request_id,
            invocation: Host::invocation("double", n(21)),
        }),
        LIMITS,
    )
    .unwrap();
    let reply = host.machine.dispatch(&bytes).unwrap();
    let expected = abi::encode(
        &Frame::new(GuestMessage::Completed {
            request_id,
            result: InvocationResult {
                output: n(42),
                patches: vec![],
                transactions: vec![],
                emitted_events: vec![],
            },
        }),
        LIMITS,
    )
    .unwrap();
    assert_eq!(reply, expected, "1.0 frames are byte-identical");
    assert_eq!(&reply[4..6], &[1, 0], "frame stamped 1.0");
}

#[test]
fn a_1_1_session_stamps_every_frame_after_negotiation_with_1_1() {
    let mut host = guest();
    let bytes = abi::encode(
        &Frame::with_abi(
            ABI_VERSION_1_1,
            HostMessage::InvokeRound {
                request_id: RequestId(9),
                invocation: Host::invocation("sync_pure", n(1)),
                round: 1,
                max_rounds: 1,
                tape: vec![],
            },
        ),
        LIMITS,
    )
    .unwrap();
    let reply = host.machine.dispatch(&bytes).unwrap();
    assert_eq!(&reply[4..6], &[1, 1]);
    // A 1.0 frame in a 1.1 session is refused.
    let stale = abi::encode(&Frame::new(HostMessage::Drain), LIMITS).unwrap();
    assert_eq!(host.machine.dispatch(&stale), Err(abi::ProtocolError::AbiMismatch));
}

#[test]
fn guest_panics_unwind_natively_and_trap_in_wasm() {
    // Natively the panic unwinds out of dispatch (in wasm, panic = abort is
    // a trap, covered by the async-conformance suite).
    let mut host = guest();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.invoke("panics", Value::Null, 1)
    }));
    assert!(result.is_err());
}

/// H-1: plan hashes are memoised per invocation on the warm instance; the
/// checks are unchanged: an altered hash or a reordered tape in a late round
/// (after the memo holds every earlier plan) is still a protocol failure, and
/// so is a guest that diverges only in round 4.
#[test]
fn memoised_plan_hashes_keep_every_replay_check() {
    for (round, record) in [(3u16, 0usize), (4, 0), (4, 1), (4, 2)] {
        let mut host = guest();
        host.tamper = Some(Box::new(move |r, tape| {
            if r == round {
                tape[record].plan_hash[0] ^= 1;
            }
        }));
        let run = host.invoke("three", Value::Null, 3);
        assert_eq!(run.error().code, ErrorCode::Protocol, "round {round} record {record}");
        assert!(
            run.error().message.contains(&format!("effect round {}", record + 1)),
            "{}",
            run.error().message
        );
    }
    let mut host = guest();
    host.tamper = Some(Box::new(|round, tape| {
        if round == 4 {
            tape.swap(1, 2);
        }
    }));
    assert_eq!(host.invoke("three", Value::Null, 3).error().code, ErrorCode::Protocol);

    // The same warm instance runs the next invocation from a clean memo.
    let mut host = guest();
    for _ in 0..3 {
        let run = host.invoke("three", Value::Null, 3);
        assert_eq!(run.invokes, 4);
        run.output();
    }

    let mut host = guest();
    let run = host.invoke("late_divergence", Value::Null, 3);
    assert_eq!(run.invokes, 4);
    assert_eq!(run.error().code, ErrorCode::Protocol);
    assert!(run.error().message.contains("effect round 1"), "{}", run.error().message);
}

/// H-1: the memo-less executor (`run_round`, a fresh context per round, as
/// on an instance that kept nothing) gives exactly the replies of the warm
/// adapter, round by round.
#[test]
fn memo_less_rounds_give_the_same_replies_as_the_warm_adapter() {
    use axiom_extension_sdk::{
        __private::AsyncExports,
        async_rt::{run_round, RoundInvocation, RoundStep},
    };
    let mut warm = guest();
    let run = warm.invoke("three", Value::Null, 3);
    let warm_output = run.output().clone();
    let invocation = Host::invocation("three", Value::Null);
    let mut tape: Vec<RoundRecord> = Vec::new();
    for round in 1..=4u16 {
        let request = RoundInvocation {
            request_id: RequestId(1),
            invocation: invocation.clone(),
            round,
            max_rounds: 3,
            tape: tape.clone(),
        };
        let expected: Vec<_> = tape.iter().map(|record| record.plan_hash).collect();
        match run_round(request, |cx| Guest::dispatch_async(cx, "three")) {
            RoundStep::Yielded { replayed, plan } => {
                assert_eq!(replayed, expected);
                assert_eq!(plan, run.plans[usize::from(round - 1)]);
                let outcomes = warm.execute(&plan);
                tape.push(RoundRecord { plan_hash: abi::round_plan_hash(&plan), outcomes, events: Vec::new() });
            }
            RoundStep::Completed { replayed, result } => {
                assert_eq!(round, 4);
                assert_eq!(replayed, expected);
                assert_eq!(result.output, warm_output);
            }
            RoundStep::Failed(error) => panic!("{error:?}"),
        }
    }
}

/// H-1: an export whose input parameter is `Value` takes the input by move
/// (no per-round copy of the value). W2A-1: an export that also reads the
/// input through `cx.input()`, `cx.invocation().input()` or a helper given
/// `cx` gets it everywhere, as before H-1 (decoded again from the round's
/// frame when asked). Typed inputs are unchanged.
#[test]
fn a_value_input_is_moved_into_the_argument_and_typed_inputs_are_unchanged() {
    let mut host = guest();
    let input = Value::Record(vec![abi::Field { name: "steps".into(), value: n(3) }]);
    let run = host.invoke("value_input", input.clone(), 1);
    assert_eq!(run.invokes, 2);
    let Value::List(items) = run.output() else { panic!("{:?}", run.output()) };
    assert_eq!(items[0], input, "the argument holds the input in the completing round");
    assert_eq!(items[1], n(10));
    assert_eq!(items[2], input, "cx.input() sees the input (W2A-1)");
    assert_eq!(items[3], input, "cx.invocation().input() sees the input (W2A-1)");
    assert_eq!(items[4], Value::Bool(true), "the input is still moved into the argument");
    // The plan used the input in round 1 and replayed it in round 2.
    assert_eq!(run.plans, vec![RoundPlan::Plan(EffectPlan { effects: vec![effect("load", input.clone())] })]);

    // Context used only for host operations: moved in every round.
    let run = host.invoke("moved_value_input", input.clone(), 1);
    assert_eq!(run.invokes, 2);
    assert_eq!(run.output(), &Value::List(vec![input.clone(), n(10), Value::Bool(true)]));

    // A helper given the context reads the input.
    let run = host.invoke("value_input_in_helper", input.clone(), 1);
    assert_eq!(run.output(), &Value::List(vec![input.clone(), n(10), input.clone()]));

    assert_eq!(host.invoke("qualified_value_input", input.clone(), 1).output(), &input);

    let run = host.invoke("typed_input", n(7), 1);
    assert_eq!(run.output(), &list(&[7, 7, 7, 10]));
}

/// L-5: the 1.0-only machine (`GuestMachine<_, false>`, what the export
/// macros build for a module that exports ABI 1.0) never selects 1.1, even
/// for a rounds-capable extension offered 1.1, and refuses `InvokeRound`
/// without changing state; ordinary 1.0 invocations still work.
#[test]
fn a_machine_without_rounds_never_selects_1_1() {
    let extension: Box<dyn Extension> = Guest::axiom_extension_for_tests().into();
    let mut machine = GuestMachine::<Box<dyn Extension>, false>::without_rounds(extension, LIMITS);
    let mut send = |abi: AbiVersion, message: HostMessage| {
        let bytes = abi::encode(&Frame::with_abi(abi, message), LIMITS).unwrap();
        machine
            .dispatch(&bytes)
            .map(|reply| abi::decode_frame::<GuestMessage>(&reply, LIMITS).unwrap().message)
    };
    assert_eq!(
        send(ABI_VERSION, HostMessage::Negotiate { supported: vec![ABI_VERSION_1_1, ABI_VERSION] }),
        Ok(GuestMessage::Negotiated { selected: ABI_VERSION })
    );
    assert_eq!(
        send(ABI_VERSION, HostMessage::Initialize { instance_id: 1, application: "x8c".into(), extension: "async".into() }),
        Ok(GuestMessage::Initialized)
    );
    let round = HostMessage::InvokeRound {
        request_id: RequestId(1),
        invocation: Host::invocation("three", Value::Null),
        round: 1,
        max_rounds: 3,
        tape: Vec::new(),
    };
    assert!(send(ABI_VERSION, round.clone()).is_err());
    assert!(send(ABI_VERSION_1_1, round).is_err());
    // A sync export over 1.0 still completes.
    let reply = send(ABI_VERSION, HostMessage::Invoke { request_id: RequestId(2), invocation: Host::invocation("sync_pure", n(4)) });
    assert!(matches!(reply, Ok(GuestMessage::Completed { .. })), "{reply:?}");
}

#[test]
fn try_join_is_fail_fast_and_never_withdraws_registered_operations() {
    let mut host = guest();
    let run = host.invoke("try_join_ok", Value::Null, 1);
    assert_eq!(run.plans, vec![plan_of(&[("load", 1), ("price", 2)])]);
    assert_eq!(run.output(), &list(&[10, 20]));

    // Fails in the pass after round 1: no third round (join! would wait).
    let run = host.invoke("try_join_fails_fast", Value::Null, 4);
    assert_eq!(run.invokes, 2);
    assert_eq!(run.plans, vec![plan_of(&[("fail", 1), ("load", 1)])]);
    assert_eq!(run.error().code, ErrorCode::Host);

    // Caught: round 2 carries the dropped branch's operation registered in
    // the failing pass (never withdrawn) and the export's next one; the
    // branch's third operation is never registered.
    let calls_before = host.calls.len();
    let run = host.invoke("try_join_caught", Value::Null, 4);
    assert_eq!(run.invokes, 3);
    assert_eq!(
        run.plans,
        vec![plan_of(&[("fail", 1), ("load", 1)]), plan_of(&[("load", 10), ("price", 7)])]
    );
    assert_eq!(run.output(), &list(&[1, 20]));
    assert_eq!(host.calls.len() - calls_before, 4);

    let run = host.invoke("try_join_all_fails_fast", Value::Null, 4);
    assert_eq!(
        run.plans,
        vec![plan_of(&[("load", 1), ("load", 2), ("load", 3)]), plan_of(&[("price", 4), ("fail", 5)])]
    );
    assert_eq!(run.output(), &list(&[10, 10, 10, 1]));
}
