//! X9c (W2B-2a): the SDK emits `YieldedRoundSited` when generated bindings
//! supplied sites and the session is ABI 1.1, and the plain message
//! otherwise. Driven natively through `GuestMachine` with real frames.
//!
//! - async exports: each `Cx::call` keeps the site of the operation it was
//!   built from, in plan or group-branch order;
//! - sync exports: typed tickets (`Effects::push`) and group tickets
//!   (`EffectGroup::push`) carry the site to the round yield;
//! - a raw `Extension` may yield `abi::yielded_round(..)` with sites
//!   (`GuestMachine` used to refuse it as an invalid transition);
//! - golden: a guest without sites sends exactly the bytes of a directly
//!   built `YieldedRound`, and an ABI 1.0 session never carries labels.
use axiom_extension_sdk::{
    abi::{
        self, AbiVersion, CodecLimits, EffectOutcome, EffectPlan, Frame, GuestMessage, HostMessage,
        Invocation, RequestId, RoundPlan, RoundRecord, SiteId, Value, ABI_VERSION, ABI_VERSION_1_1,
    },
    async_rt::RoundInvocation,
    join,
    prelude::*,
    raw::GuestMachine,
    Binding, EffectGroup, Extension, SiteKey, SiteTable,
};
use std::sync::{Mutex, OnceLock};

const LIMITS: CodecLimits = CodecLimits { max_bytes: 1024 * 1024 };

const LOAD: Binding = Binding { namespace: "catalog", operation: "load", input: "Value", output: "Unsigned" };
const PRICE: Binding = Binding { namespace: "catalog", operation: "price", input: "Value", output: "Unsigned" };

/// The table the "build" embeds, filled after a first pass learned where the
/// calls are (as the X9a analyser would, from the sources).
static TABLE: OnceLock<SiteTable> = OnceLock::new();
/// Locations seen by the generated-like items, for the first pass.
static SEEN: Mutex<Vec<(&'static str, u32, u32, &'static str)>> = Mutex::new(Vec::new());

/// Stands in for a generated `operations::<name>` item: `#[track_caller]`,
/// interned through the embedded table.
#[track_caller]
fn generated(binding: Binding, name: &'static str) -> Operation<Value, u64> {
    let here = core::panic::Location::caller();
    SEEN.lock().unwrap().push((here.file(), here.line(), here.column(), name));
    match TABLE.get() {
        Some(table) => Operation::generated_sited(binding, table, name),
        None => Operation::generated_sited(binding, &SiteTable::EMPTY, name),
    }
}

#[derive(Default)]
struct Guest;

#[extension]
impl Guest {
    /// Round 1: one sited operation (a plan). Round 2: a group of a sited and
    /// an unsited operation.
    #[export]
    async fn flow(cx: &Cx, _input: Value) -> Result<u64> {
        let a = cx.call(generated(LOAD, "operations::load").call(Value::Null)).await?;
        let (b, c) = join!(
            cx.call(generated(PRICE, "operations::price").call(Value::Null)),
            cx.call(Operation::<Value, u64>::generated(PRICE).call(Value::Null))
        )
        .await;
        Ok(a + b? + c?)
    }

    /// A sync export: typed tickets, a sited and an unsited effect.
    #[export]
    fn plan_tickets(&mut self, _invocation: TypedInvocation) -> Result<ExtensionResponse> {
        let mut effects = Effects::new();
        let _ = effects.push(generated(LOAD, "operations::load").call(Value::Null));
        let _ = effects.push(Operation::<Value, u64>::generated(PRICE).call(Value::Null));
        Ok(ExtensionResponse::yield_for(effects))
    }

    /// A sync export: group tickets, an unsited and a sited branch.
    #[export]
    fn group_tickets(&mut self, _invocation: TypedInvocation) -> Result<ExtensionResponse> {
        let mut group = EffectGroup::new(abi::EffectGroupMode::Collect, 2);
        let _ = group.push("plain", Operation::<Value, u64>::generated(PRICE).call(Value::Null));
        let _ = group.push("sited", generated(LOAD, "operations::load").call(Value::Null));
        Ok(ExtensionResponse::yield_effect_group(group))
    }
}

struct Session {
    machine: GuestMachine<Box<dyn Extension>>,
    abi: AbiVersion,
}

impl Session {
    fn new(extension: Box<dyn Extension>, offer: Vec<AbiVersion>) -> Self {
        let mut session = Self { machine: GuestMachine::new(extension, LIMITS), abi: ABI_VERSION };
        let (_, negotiated) = session.send(HostMessage::Negotiate { supported: offer });
        let GuestMessage::Negotiated { selected } = negotiated else { panic!("{negotiated:?}") };
        session.abi = selected;
        let (_, initialized) = session.send(HostMessage::Initialize {
            instance_id: 1,
            application: "x9c".into(),
            extension: "sites".into(),
        });
        assert_eq!(initialized, GuestMessage::Initialized);
        session
    }

    /// The reply bytes and the decoded reply.
    fn send(&mut self, message: HostMessage) -> (Vec<u8>, GuestMessage) {
        let negotiating = matches!(message, HostMessage::Negotiate { .. });
        let frame = if negotiating || self.abi == ABI_VERSION { Frame::new(message) } else { Frame::with_abi(self.abi, message) };
        let bytes = abi::encode(&frame, LIMITS).unwrap();
        let reply = self.machine.dispatch(&bytes).unwrap_or_else(|error| panic!("{error:?}: {:?}", self.machine.last_error()));
        let accepted = if negotiating { ABI_VERSION } else { self.abi };
        let frame: Frame<GuestMessage> = abi::decode_frame_versioned(&reply, LIMITS, &[accepted]).unwrap();
        (reply, frame.message)
    }

    fn round(&mut self, request_id: u64, export: &str, round: u16, tape: Vec<RoundRecord>) -> (Vec<u8>, GuestMessage) {
        self.send(HostMessage::InvokeRound {
            request_id: RequestId(request_id),
            invocation: invocation(export),
            round,
            max_rounds: 4,
            tape,
        })
    }
}

fn invocation(export: &str) -> Invocation {
    Invocation { export: export.into(), input: Value::Null, snapshots: vec![], deadline_unix_ms: 1 }
}

fn outcomes(plan: &RoundPlan) -> Vec<EffectOutcome> {
    (0..plan.len()).map(|index| EffectOutcome { index: index as u32, result: Ok(Value::Unsigned(10)) }).collect()
}

/// (plan, sites) of a yield; sites empty for a plain `YieldedRound`.
fn yielded(message: GuestMessage) -> (Vec<abi::PlanHash>, RoundPlan, Vec<Option<SiteId>>, bool) {
    match message {
        GuestMessage::YieldedRound { replayed, plan, .. } => (replayed, plan, Vec::new(), false),
        GuestMessage::YieldedRoundSited { replayed, plan, sites, .. } => (replayed, plan, sites, true),
        other => panic!("not a yield: {other:?}"),
    }
}

/// The 1.1 frame bytes of a directly built plain `YieldedRound`.
fn plain_bytes(request_id: u64, round: u16, replayed: Vec<abi::PlanHash>, plan: RoundPlan) -> Vec<u8> {
    let message = GuestMessage::YieldedRound { request_id: RequestId(request_id), round, replayed, plan };
    abi::encode(&Frame::with_abi(ABI_VERSION_1_1, message), LIMITS).unwrap()
}

/// Runs `flow` to completion; returns per round (reply bytes, replayed, plan,
/// sites, sited?).
#[allow(clippy::type_complexity)]
fn run_flow(session: &mut Session, request_id: u64) -> Vec<(Vec<u8>, Vec<abi::PlanHash>, RoundPlan, Vec<Option<SiteId>>, bool)> {
    let mut tape = Vec::new();
    let mut rounds = Vec::new();
    for round in 1..=3 {
        let (bytes, reply) = session.round(request_id, "flow", round, tape.clone());
        if let GuestMessage::CompletedRound { result, .. } = reply {
            assert_eq!(result.output, Value::Unsigned(30));
            return rounds;
        }
        let (replayed, plan, sites, sited) = yielded(reply);
        tape.push(RoundRecord { plan_hash: abi::round_plan_hash(&plan), outcomes: outcomes(&plan), events: vec![] });
        rounds.push((bytes, replayed, plan, sites, sited));
    }
    panic!("flow did not complete")
}

/// One test, because the table is process-global: first without a table
/// (golden bytes), then with the table the first pass learned.
#[test]
fn sites_ride_beside_sdk_plans_only_when_known_and_only_on_1_1() {
    // ---- Pass 1: no table. Every yield is the plain `YieldedRound`, byte for byte.
    let mut session = Session::new(axiom_extension().into(), vec![ABI_VERSION_1_1, ABI_VERSION]);
    assert_eq!(session.abi, ABI_VERSION_1_1);
    let unsited = run_flow(&mut session, 1);
    assert_eq!(unsited.len(), 2);
    for (index, (bytes, replayed, plan, sites, sited)) in unsited.iter().enumerate() {
        assert!(!sited && sites.is_empty());
        assert_eq!(bytes, &plain_bytes(1, index as u16 + 1, replayed.clone(), plan.clone()), "round {}", index + 1);
    }
    for export in ["plan_tickets", "group_tickets"] {
        let (bytes, reply) = session.round(2, export, 1, vec![]);
        let (replayed, plan, sites, sited) = yielded(reply);
        assert!(!sited && sites.is_empty());
        assert_eq!(bytes, plain_bytes(2, 1, replayed, plan));
        let _ = session.send(HostMessage::Cancel { request_id: RequestId(2) });
    }

    // ---- The "build": intern the locations the generated items reported.
    let mut seen = SEEN.lock().unwrap().clone();
    seen.sort();
    seen.dedup();
    let sites: Vec<SiteKey> =
        seen.iter().map(|(file, line, column, binding)| SiteKey { file, line: *line, column: *column, binding }).collect();
    assert_eq!(sites.len(), 4, "{sites:?}");
    let leaked: &'static [SiteKey] = Box::leak(sites.into_boxed_slice());
    TABLE.set(SiteTable { root: "", sites: leaked }).unwrap();
    let id = |line_of: usize| Some(line_of as SiteId);

    // ---- Pass 2: the same program with the table.
    let mut session = Session::new(axiom_extension().into(), vec![ABI_VERSION_1_1, ABI_VERSION]);
    let sited = run_flow(&mut session, 1);
    // Plans, plan hashes and replays are unchanged; labels ride beside them.
    for ((_, replayed, plan, sites, is_sited), (_, unsited_replayed, unsited_plan, ..)) in sited.iter().zip(&unsited) {
        assert!(*is_sited, "{sites:?}");
        assert_eq!((replayed, plan), (unsited_replayed, unsited_plan));
        assert_eq!(sites.len(), plan.len());
    }
    // Round 1: the sited load. Round 2: the sited price, then the plain one.
    // `flow` comes first in this file, so its load call is the first load entry.
    let flow_load = leaked.iter().position(|s| s.binding == "operations::load").unwrap();
    assert_eq!(sited[0].3, vec![id(flow_load)]);
    assert!(matches!(sited[1].2, RoundPlan::Group(_)));
    assert_eq!(sited[1].3[1], None);
    let price = sited[1].3[0].expect("the price call is sited");
    assert_eq!(leaked[price as usize].binding, "operations::price");
    // The sync exports' tickets carry the site of their call.
    let (_, reply) = session.round(2, "plan_tickets", 1, vec![]);
    let (_, plan, sites, is_sited) = yielded(reply);
    assert!(is_sited && matches!(plan, RoundPlan::Plan(EffectPlan { .. })));
    assert!(matches!(sites.as_slice(), [Some(_), None]), "{sites:?}");
    let _ = session.send(HostMessage::Cancel { request_id: RequestId(2) });
    let (_, reply) = session.round(3, "group_tickets", 1, vec![]);
    let (_, plan, sites, is_sited) = yielded(reply);
    assert!(is_sited && matches!(plan, RoundPlan::Group(_)));
    assert!(matches!(sites.as_slice(), [None, Some(_)]), "{sites:?}");
    let _ = session.send(HostMessage::Cancel { request_id: RequestId(3) });
    assert_eq!(leaked[sites[1].unwrap() as usize].binding, "operations::load");
    assert_ne!(sites[1], id(flow_load), "each call has its own site");

    // ---- ABI 1.0 session: never a label (the 1.0 messages have no room).
    let mut session = Session::new(axiom_extension().into(), vec![ABI_VERSION]);
    assert_eq!(session.abi, ABI_VERSION);
    let (_, reply) = session.send(HostMessage::Invoke { request_id: RequestId(1), invocation: invocation("plan_tickets") });
    assert!(matches!(reply, GuestMessage::Yielded { .. }), "{reply:?}");
}

/// A raw extension may yield a sited round itself (`abi::yielded_round`);
/// `GuestMachine` accepts it like `YieldedRound` (it used to refuse it).
#[test]
fn a_raw_extension_may_yield_a_sited_round() {
    struct Raw;
    impl Extension for Raw {
        fn invoke(&mut self, request_id: RequestId, _: Invocation) -> GuestMessage {
            axiom_extension_sdk::raw::completed(request_id, Value::Null)
        }
        fn supports_rounds(&self) -> bool {
            true
        }
        fn invoke_round(&mut self, request: RoundInvocation) -> GuestMessage {
            let effect = LOAD.effect(Value::Null);
            let plan = RoundPlan::Plan(EffectPlan { effects: vec![effect.clone(), effect] });
            abi::yielded_round(request.request_id, request.round, vec![], plan, vec![Some(3), None])
        }
    }
    let mut session = Session::new(Box::new(Raw), vec![ABI_VERSION_1_1]);
    let (_, reply) = session.round(1, "x", 1, vec![]);
    let (_, plan, sites, sited) = yielded(reply);
    assert!(sited);
    assert_eq!(sites, vec![Some(3), None]);
    assert_eq!(plan.len(), 2);
    // The machine is in the yielded state: a cancel is accepted.
    let (_, cancelled) = session.send(HostMessage::Cancel { request_id: RequestId(1) });
    assert_eq!(cancelled, GuestMessage::Cancelled { request_id: RequestId(1) });
}
