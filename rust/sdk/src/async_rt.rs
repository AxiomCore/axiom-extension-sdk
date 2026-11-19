//! ABI 1.1 async exports (X8c): a single-threaded, replaying guest executor.
//!
//! An `#[export] async fn name(cx: &Cx, input: In) -> Result<Out>` inside an
//! `#[extension]` impl is driven by [`run_round`]. Every ABI 1.1 round is a
//! fresh run of the export from the top, with the same invocation (round-1
//! snapshots) and the host-held tape of earlier rounds. No guest memory is
//! carried from one round to the next.
//!
//! # Model
//!
//! * A host operation is a [`Call`] future returned by [`Cx::call`]. It
//!   registers itself in the current *segment* the first time it is polled
//!   (an unpolled future does nothing), in poll order.
//! * The executor polls the export's future with a no-op waker until it is
//!   ready or *stalls* (returns `Pending`). At a stall, every operation
//!   registered since the previous stall forms one round plan, in
//!   registration order: one operation is a [`RoundPlan::Plan`] with one
//!   effect, several are a [`RoundPlan::Group`] in `Collect` mode with
//!   branches `b0, b1, ...` and `limit = min(n, 8)`.
//! * Replay: while earlier rounds remain on the tape, the k-th stall's plan is
//!   hashed ([`abi::round_plan_hash`]), compared with `tape[k].plan_hash`
//!   (mismatch: `Failed { Protocol }`, "non-deterministic guest"), pushed to
//!   `replayed`, and its operations resolve from `tape[k].outcomes` (by
//!   outcome index = position in the plan). The first stall beyond the tape
//!   is yielded as the next plan.
//! * An operation that the host failed resolves its future with `Err`; the
//!   export continues (so `?` and explicit handling both work).
//!
//! # Determinism rules
//!
//! * Host results, time and randomness come only from host operations
//!   (`Clock`/`Random` bindings are ordinary operations and are on the tape).
//! * Pending operations are ordered by registration (first poll) order;
//!   [`join!`](crate::join) and [`join_all`] poll their children in argument
//!   order on every pass.
//! * No racing combinator (`select!`) is offered. If guest code drops a
//!   registered operation before the stall (a cancelled branch of a custom
//!   combinator), the operation **stays in its round's plan**: it is executed
//!   and its result is discarded. Registered operations are never withdrawn,
//!   so tape positions never shift.
//! * Operations registered after the last stall when the export returns are
//!   never planned and never executed.
//! * [`Cx::round`] differs between rounds; deciding *which* operations to call
//!   from it (or from any state that survives in a `static`) makes the guest
//!   non-deterministic, which replay detects (`Protocol`, nothing commits).
//! * Awaiting anything that is not a host operation (a channel, a timer, a
//!   future from another executor) stalls with no new operation: the
//!   invocation fails with `InvalidInput`.

use crate::{
    abi,
    typed::{AxiomDecode, AxiomEncode, EffectCall, Result, SdkError, TypedExtension, TypedInvocation},
    Extension,
};
use abi::{
    Effect, EffectGroup, EffectGroupMode, EffectOutcome, EffectPlan, ErrorCode, Event, EventBatch,
    ExtensionError, GuestMessage, Invocation, InvocationResult, NamedEffect, Patch, PlanHash,
    RequestId, RoundPlan, RoundRecord, TransactionProposal, Value,
};
use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::{
    any::Any,
    cell::{Cell, RefCell},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// Most branches the broker accepts in one effect group (`execute_group`).
pub const MAX_GROUP_BRANCHES: usize = 32;
/// Highest concurrency `limit` the broker accepts for an effect group.
pub const MAX_GROUP_LIMIT: u16 = 8;

/// A pinned, boxed, single-threaded future (no `Send` bound).
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// One `InvokeRound` as seen by an [`Extension`]: identical invocation bytes
/// every round, plus the host-owned tape of earlier rounds.
#[derive(Clone, Debug, PartialEq)]
pub struct RoundInvocation {
    pub request_id: RequestId,
    pub invocation: Invocation,
    /// 1-based invoke number of this invocation.
    pub round: u16,
    /// The `maxRounds` budget: how many times the guest may yield.
    pub max_rounds: u16,
    /// `tape.len() == round - 1`.
    pub tape: Vec<RoundRecord>,
}

enum Slot {
    Waiting(Option<Waker>),
    Ready(core::result::Result<Value, ExtensionError>),
    Taken,
}

struct RunState {
    tape: Vec<RoundRecord>,
    /// Tape records consumed by replay so far.
    cursor: usize,
    slots: Vec<Slot>,
    /// Operations registered since the last stall: (slot, effect).
    segment: Vec<(usize, Effect)>,
    replayed: Vec<PlanHash>,
    patches: Vec<Patch>,
    transactions: Vec<TransactionProposal>,
    events: Vec<Event>,
}

/// The round context of an async export: host operations, the invocation
/// (input and round-1 snapshots) and the final patches.
pub struct Cx {
    invocation: TypedInvocation,
    /// The export input, held outside `invocation` when the export's input
    /// parameter is `Value` ([`AsyncExports::input_moves`]): it is moved into
    /// the argument instead of being cloned, once per round.
    moved_input: RefCell<Option<Value>>,
    /// The export input was moved into the export's `Value` argument.
    input_taken: Cell<bool>,
    round: u16,
    max_rounds: u16,
    state: RefCell<RunState>,
}

impl Cx {
    fn new(request: RoundInvocation, move_input: bool) -> Self {
        let mut invocation = request.invocation;
        let moved_input = move_input.then(|| core::mem::replace(&mut invocation.input, Value::Null));
        Self {
            invocation: TypedInvocation::new(request.request_id, invocation),
            moved_input: RefCell::new(moved_input),
            input_taken: Cell::new(false),
            round: request.round,
            max_rounds: request.max_rounds,
            state: RefCell::new(RunState {
                tape: request.tape,
                cursor: 0,
                slots: Vec::new(),
                segment: Vec::new(),
                replayed: Vec::new(),
                patches: Vec::new(),
                transactions: Vec::new(),
                events: Vec::new(),
            }),
        }
    }

    /// Start a host operation built by a generated typed binding
    /// (`OPERATION.call(input)`). Awaiting the returned future yields the
    /// outcome decoded as the operation's declared output type `O`, or the
    /// host's error. The operation joins the round plan when first polled.
    pub fn call<O: AxiomDecode>(&self, call: EffectCall<O>) -> Call<'_, O> {
        Call {
            cx: self,
            state: CallState::Unpolled(call.into_effect()),
            marker: PhantomData,
        }
    }

    /// The invocation: export name, input and the round-1 snapshots (use it
    /// with `UiState::generated` / `Store::generated` as in sync exports).
    ///
    /// In an export whose input parameter is `Value`, the input is moved into
    /// that argument (no per-round copy), so `invocation().input()` sees
    /// `Null` there; read the argument instead.
    pub fn invocation(&self) -> &TypedInvocation {
        &self.invocation
    }

    /// The export input decoded as `T`.
    ///
    /// In an export whose input parameter is `Value`, the input was moved
    /// into that argument and this returns `InvalidInput`; read the argument.
    pub fn input<T: AxiomDecode>(&self) -> Result<T> {
        if let Some(value) = self.moved_input.borrow().as_ref() {
            return T::decode(value);
        }
        if self.input_taken.get() {
            return Err(SdkError::invalid_input(
                "the export input was moved into the export's `Value` argument; read it from the argument",
            ));
        }
        self.invocation.input()
    }

    /// The export's input argument (used by `#[extension]` for exports whose
    /// input parameter is `Value`): moved, not cloned, when it was held aside
    /// for this export; otherwise decoded as usual.
    #[doc(hidden)]
    pub fn __export_input<T: AxiomDecode + 'static>(&self) -> Result<T> {
        let Some(value) = self.moved_input.borrow_mut().take() else {
            return self.input();
        };
        self.input_taken.set(true);
        let mut slot = Some(value);
        if let Some(moved) = (&mut slot as &mut dyn Any).downcast_mut::<Option<T>>() {
            if let Some(moved) = moved.take() {
                return Ok(moved);
            }
        }
        match slot {
            Some(value) => T::decode(&value),
            None => Err(SdkError::invalid_input("the export input is missing")),
        }
    }

    /// The 1-based invoke number of this run. It changes from round to round:
    /// never use it to decide which host operations to call (see the module
    /// determinism rules). Reporting it in the output after the last await is
    /// fine.
    pub fn round(&self) -> u16 {
        self.round
    }

    /// The host's `maxRounds`: how many times this invocation may yield.
    pub fn max_rounds(&self) -> u16 {
        self.max_rounds
    }

    /// Add a final patch. Only the patches of the completing run are sent; the
    /// host revision-checks them against the round-1 snapshots.
    pub fn patch(&self, patch: impl Into<Patch>) {
        self.state.borrow_mut().patches.push(patch.into());
    }

    /// Add a final store transaction proposal (committed with the result).
    pub fn transaction(&self, transaction: impl Into<TransactionProposal>) {
        self.state.borrow_mut().transactions.push(transaction.into());
    }

    /// Emit an event with the result.
    pub fn emit<T: AxiomEncode>(&self, sequence: u64, value: T) {
        self.state.borrow_mut().events.push(Event {
            sequence,
            value: value.encode(),
        });
    }

    fn register(&self, effect: Effect, waker: &Waker) -> usize {
        let mut state = self.state.borrow_mut();
        let slot = state.slots.len();
        state.slots.push(Slot::Waiting(Some(waker.clone())));
        state.segment.push((slot, effect));
        slot
    }

    fn take(
        &self,
        slot: usize,
        waker: &Waker,
    ) -> Option<core::result::Result<Value, ExtensionError>> {
        let mut state = self.state.borrow_mut();
        let entry = state.slots.get_mut(slot)?;
        match core::mem::replace(entry, Slot::Taken) {
            Slot::Ready(result) => Some(result),
            Slot::Waiting(_) => {
                *entry = Slot::Waiting(Some(waker.clone()));
                None
            }
            Slot::Taken => Some(Err(ExtensionError {
                code: ErrorCode::InvalidInput,
                message: "host operation result was already taken".into(),
                retryable: false,
            })),
        }
    }
}

enum CallState {
    Unpolled(Effect),
    Registered(usize),
    Done,
}

/// A host operation in an async export. Resolves to the decoded outcome.
/// Created by [`Cx::call`]; it registers in the current round when first
/// polled. Polling it again after it completed yields an `InvalidInput`
/// error instead of panicking.
#[must_use = "a host operation does nothing unless it is awaited"]
pub struct Call<'a, O> {
    cx: &'a Cx,
    state: CallState,
    marker: PhantomData<fn() -> O>,
}

impl<O: AxiomDecode> Future for Call<'_, O> {
    type Output = Result<O>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<O>> {
        let this = self.get_mut();
        match core::mem::replace(&mut this.state, CallState::Done) {
            CallState::Unpolled(effect) => {
                this.state = CallState::Registered(this.cx.register(effect, context.waker()));
                Poll::Pending
            }
            CallState::Registered(slot) => match this.cx.take(slot, context.waker()) {
                Some(Ok(value)) => Poll::Ready(O::decode(&value)),
                Some(Err(error)) => Poll::Ready(Err(SdkError {
                    code: error.code,
                    message: error.message,
                    retryable: error.retryable,
                })),
                None => {
                    this.state = CallState::Registered(slot);
                    Poll::Pending
                }
            },
            CallState::Done => Poll::Ready(Err(SdkError::invalid_input(
                "host operation future was polled after it completed",
            ))),
        }
    }
}

// ------------------------------------------------------------------ joining

/// A child of [`join!`](crate::join) / [`join_all`]: boxed so the join stays
/// `Unpin` without unsafe pin projection.
#[doc(hidden)]
pub enum MaybeDone<F: Future> {
    Pending(Pin<Box<F>>),
    Done(F::Output),
    Gone,
}

// Outputs are never pinned (children are boxed), so the wrapper is Unpin.
impl<F: Future> Unpin for MaybeDone<F> {}

impl<F: Future> MaybeDone<F> {
    pub fn new(future: F) -> Self {
        Self::Pending(Box::pin(future))
    }

    /// Poll if still pending; true when the output is available.
    pub fn poll_step(&mut self, context: &mut Context<'_>) -> bool {
        if let Self::Pending(future) = self {
            match future.as_mut().poll(context) {
                Poll::Ready(output) => *self = Self::Done(output),
                Poll::Pending => return false,
            }
        }
        true
    }

    pub fn take(&mut self) -> F::Output {
        match core::mem::replace(self, Self::Gone) {
            Self::Done(output) => output,
            _ => unreachable!("join output taken before completion"),
        }
    }
}

/// Await several futures together. Every pass polls the children in argument
/// order, so their host operations register in that order and the ones that
/// are pending at the same stall go to the host as **one** `Collect` group.
/// Returns a future of the tuple of outputs (all results, no fail-fast):
///
/// ```ignore
/// let (price, stock) = join!(cx.call(PRICE.call(id)), cx.call(STOCK.call(id))).await;
/// ```
#[macro_export]
macro_rules! join {
    ($($future:expr),+ $(,)?) => {
        $crate::__join_impl!([] $($future,)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __join_impl {
    // Each recursion step introduces a fresh hygienic `child` binding.
    ([$($done:tt)*] $head:expr, $($rest:expr,)*) => {
        $crate::__join_impl!([$($done)* ($head, child)] $($rest,)*)
    };
    ([$(($future:expr, $child:ident))+]) => {{
        $(let mut $child = $crate::async_rt::MaybeDone::new($future);)+
        ::core::future::poll_fn(move |context| {
            let mut ready = true;
            $(ready &= $child.poll_step(context);)+
            if ready {
                ::core::task::Poll::Ready(($($child.take(),)+))
            } else {
                ::core::task::Poll::Pending
            }
        })
    }};
}

/// Await every future of an iterator together (a dynamic [`join!`]):
/// operations pending at the same stall form one group, in iterator order.
pub fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> JoinAll<F> {
    JoinAll {
        children: futures.into_iter().map(MaybeDone::new).collect(),
    }
}

/// Future returned by [`join_all`].
pub struct JoinAll<F: Future> {
    children: Vec<MaybeDone<F>>,
}

impl<F: Future> Future for JoinAll<F> {
    type Output = Vec<F::Output>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut ready = true;
        for child in &mut this.children {
            ready &= child.poll_step(context);
        }
        if ready {
            Poll::Ready(this.children.iter_mut().map(MaybeDone::take).collect())
        } else {
            Poll::Pending
        }
    }
}

// ------------------------------------------------------- fail-fast joining

impl<T, E, F: Future<Output = core::result::Result<T, E>>> MaybeDone<F> {
    /// True when the child finished with an error ([`try_join!`](crate::try_join)).
    pub fn is_err(&self) -> bool {
        matches!(self, Self::Done(Err(_)))
    }

    /// The error of a child that finished with one.
    pub fn take_err(&mut self) -> E {
        match core::mem::replace(self, Self::Gone) {
            Self::Done(Err(error)) => error,
            _ => unreachable!("try_join error taken from a child without one"),
        }
    }

    /// The value of a child that finished successfully.
    pub fn take_ok(&mut self) -> T {
        match core::mem::replace(self, Self::Gone) {
            Self::Done(Ok(value)) => value,
            _ => unreachable!("try_join value taken before completion"),
        }
    }
}

/// Fail-fast [`join!`]: the counterpart of TypeScript `Promise.all` and Python
/// `gather(...)` without `return_exceptions` (contract x8c §3.6). Every pass
/// polls the children that are still pending, in argument order (so their
/// host operations register exactly as with [`join!`]); after a pass in which
/// a child finished with `Err`, it returns the error of the first such child
/// in argument order and drops the others. Operations a dropped child already
/// registered stay registered and run with their round (never withdrawn); it
/// registers nothing more. Returns a future of `Result<(T1, T2, ...), E>`:
///
/// ```ignore
/// let (price, stock) = try_join!(cx.call(PRICE.call(id)), cx.call(STOCK.call(id))).await?;
/// ```
#[macro_export]
macro_rules! try_join {
    ($($future:expr),+ $(,)?) => {
        $crate::__try_join_impl!([] $($future,)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __try_join_impl {
    // Each recursion step introduces a fresh hygienic `child` binding.
    ([$($done:tt)*] $head:expr, $($rest:expr,)*) => {
        $crate::__try_join_impl!([$($done)* ($head, child)] $($rest,)*)
    };
    ([$(($future:expr, $child:ident))+]) => {{
        $(let mut $child = $crate::async_rt::MaybeDone::new($future);)+
        ::core::future::poll_fn(move |context| {
            let mut ready = true;
            $(ready &= $child.poll_step(context);)+
            $(if $child.is_err() {
                return ::core::task::Poll::Ready(::core::result::Result::Err($child.take_err()));
            })+
            if ready {
                ::core::task::Poll::Ready(::core::result::Result::Ok(($($child.take_ok(),)+)))
            } else {
                ::core::task::Poll::Pending
            }
        })
    }};
}

/// Fail-fast [`join_all`] (TypeScript `Promise.all` over an array, Python
/// `gather(*items)`): every pass polls the pending children in iterator
/// order; after a pass in which one finished with `Err`, the first such error
/// in iterator order is returned and the others are dropped.
pub fn try_join_all<T, E, F: Future<Output = core::result::Result<T, E>>>(futures: impl IntoIterator<Item = F>) -> TryJoinAll<F> {
    TryJoinAll {
        children: futures.into_iter().map(MaybeDone::new).collect(),
    }
}

/// Future returned by [`try_join_all`].
pub struct TryJoinAll<F: Future> {
    children: Vec<MaybeDone<F>>,
}

impl<T, E, F: Future<Output = core::result::Result<T, E>>> Future for TryJoinAll<F> {
    type Output = core::result::Result<Vec<T>, E>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut ready = true;
        for child in &mut this.children {
            ready &= child.poll_step(context);
        }
        if let Some(failed) = this.children.iter_mut().find(|child| child.is_err()) {
            let error = failed.take_err();
            this.children.clear();
            return Poll::Ready(Err(error));
        }
        if ready {
            Poll::Ready(Ok(this.children.iter_mut().map(MaybeDone::take_ok).collect()))
        } else {
            Poll::Pending
        }
    }
}

// ----------------------------------------------------------------- executor

/// Result of one run of an async export.
#[derive(Debug, PartialEq)]
pub enum RoundStep {
    Completed {
        replayed: Vec<PlanHash>,
        result: InvocationResult,
    },
    Yielded {
        replayed: Vec<PlanHash>,
        plan: RoundPlan,
    },
    Failed(ExtensionError),
}

fn error(code: ErrorCode, message: impl Into<String>) -> ExtensionError {
    ExtensionError {
        code,
        message: message.into(),
        retryable: false,
    }
}

/// The plan for the operations of one segment, in registration order.
pub fn segment_plan(effects: Vec<Effect>) -> RoundPlan {
    if effects.len() == 1 {
        return RoundPlan::Plan(EffectPlan { effects });
    }
    let limit = u16::try_from(effects.len())
        .unwrap_or(u16::MAX)
        .min(MAX_GROUP_LIMIT);
    RoundPlan::Group(EffectGroup {
        format: abi::EFFECT_GROUP_FORMAT.into(),
        limit,
        mode: EffectGroupMode::Collect,
        branches: effects
            .into_iter()
            .enumerate()
            .map(|(index, effect)| NamedEffect {
                name: format!("b{index}"),
                effect,
            })
            .collect(),
    })
}

/// Run an async export once for `request`: replay the tape, then complete or
/// yield the next plan. `start` builds the export's future from the context.
pub fn run_round<S>(request: RoundInvocation, start: S) -> RoundStep
where
    S: for<'cx> FnOnce(&'cx Cx) -> Option<LocalBoxFuture<'cx, Result<Value>>>,
{
    run_round_with(request, false, &mut PlanMemo::default(), start)
}

/// Plan hashes of one invocation, kept by the warm instance between rounds
/// (H-1). Entry `k` is the plan of effect round `k + 1` as first replayed,
/// with `round_plan_hash` of it (the plan is moved in, not copied). A later
/// replayed plan that is equal to entry `k` reuses its hash instead of
/// hashing it again, so each plan is hashed once per instance instead of
/// once per later round (N hashes for N yields instead of N(N+1)/2).
///
/// This is memoisation of a pure function: the hash used is always the hash
/// of the plan the guest re-derived, so every replay check (a different
/// plan, an altered tape hash, a reordered tape) gives exactly the result it
/// gives without the memo. It is not continuation state: an empty memo (a
/// fresh instance, another invocation) just hashes.
#[derive(Default)]
pub(crate) struct PlanMemo {
    request_id: Option<RequestId>,
    plans: Vec<(RoundPlan, PlanHash)>,
}

impl PlanMemo {
    /// Keep the entries only for `request_id` and only for the tape the host
    /// sent (a fresh invocation or a shorter tape starts over).
    fn begin(&mut self, request_id: RequestId, tape_len: usize) {
        if self.request_id != Some(request_id) {
            self.request_id = Some(request_id);
            self.plans.clear();
        }
        self.plans.truncate(tape_len);
    }

    /// `round_plan_hash(plan)` for tape position `k`.
    fn hash(&mut self, k: usize, plan: RoundPlan) -> PlanHash {
        if let Some((known, hash)) = self.plans.get(k) {
            if *known == plan {
                return *hash;
            }
        }
        let hash = abi::round_plan_hash(&plan);
        // Only a contiguous prefix is kept, so entry `k` follows entry `k - 1`.
        if self.plans.len() >= k {
            self.plans.truncate(k);
            self.plans.push((plan, hash));
        }
        hash
    }

    fn clear(&mut self) {
        self.request_id = None;
        self.plans.clear();
    }
}

/// [`run_round`], holding the input aside when `move_input` is set so the
/// export's `Value` argument takes it without a copy
/// ([`AsyncExports::input_moves`]), and reusing plan hashes from `memo`.
fn run_round_with<S>(
    request: RoundInvocation,
    move_input: bool,
    memo: &mut PlanMemo,
    start: S,
) -> RoundStep
where
    S: for<'cx> FnOnce(&'cx Cx) -> Option<LocalBoxFuture<'cx, Result<Value>>>,
{
    memo.begin(request.request_id, request.tape.len());
    let step = run_round_memo(request, move_input, memo, start);
    if !matches!(step, RoundStep::Yielded { .. }) {
        memo.clear();
    }
    step
}

fn run_round_memo<S>(
    request: RoundInvocation,
    move_input: bool,
    memo: &mut PlanMemo,
    start: S,
) -> RoundStep
where
    S: for<'cx> FnOnce(&'cx Cx) -> Option<LocalBoxFuture<'cx, Result<Value>>>,
{
    let cx = Cx::new(request, move_input);
    let Some(mut future) = start(&cx) else {
        return RoundStep::Failed(error(
            ErrorCode::InvalidInput,
            format!("unknown async export `{}`", cx.invocation.export()),
        ));
    };
    let mut context = Context::from_waker(Waker::noop());
    let output = loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            break output;
        }
        let mut state = cx.state.borrow_mut();
        let segment = core::mem::take(&mut state.segment);
        if segment.is_empty() {
            return RoundStep::Failed(error(
                ErrorCode::InvalidInput,
                "async export is waiting on something that is not a host operation; only `Cx::call` futures can be awaited (no timers, channels or other executors)",
            ));
        }
        if segment.len() > MAX_GROUP_BRANCHES {
            return RoundStep::Failed(error(
                ErrorCode::Exhausted,
                format!(
                    "{} host operations are pending together; one round group allows at most {MAX_GROUP_BRANCHES}",
                    segment.len()
                ),
            ));
        }
        let (slots, effects): (Vec<usize>, Vec<Effect>) = segment.into_iter().unzip();
        let plan = segment_plan(effects);
        if state.cursor >= state.tape.len() {
            let replayed = core::mem::take(&mut state.replayed);
            return RoundStep::Yielded { replayed, plan };
        }
        let k = state.cursor;
        let hash = memo.hash(k, plan);
        if state.tape[k].plan_hash != hash {
            return RoundStep::Failed(error(
                ErrorCode::Protocol,
                format!(
                    "non-deterministic guest: the plan re-derived for effect round {} differs from the tape",
                    k + 1
                ),
            ));
        }
        state.replayed.push(hash);
        state.cursor += 1;
        // Outcomes move out of the tape: no copy of the values.
        let mut outcomes = core::mem::take(&mut state.tape[k].outcomes);
        let mut wakers = Vec::new();
        for (index, slot) in slots.into_iter().enumerate() {
            let result = match outcomes
                .iter()
                .position(|outcome| outcome.index as usize == index)
            {
                Some(position) => outcomes.swap_remove(position).result,
                None => Err(error(
                    ErrorCode::Protocol,
                    "the tape has no outcome for this host operation",
                )),
            };
            if let Slot::Waiting(Some(waker)) =
                core::mem::replace(&mut state.slots[slot], Slot::Ready(result))
            {
                wakers.push(waker);
            }
        }
        drop(state);
        wakers.into_iter().for_each(Waker::wake);
    };
    drop(future);
    let state = cx.state.into_inner();
    if state.cursor != state.tape.len() {
        return RoundStep::Failed(error(
            ErrorCode::Protocol,
            format!(
                "non-deterministic guest: the export completed after replaying {} of {} effect rounds",
                state.cursor,
                state.tape.len()
            ),
        ));
    }
    match output {
        Ok(output) => RoundStep::Completed {
            replayed: state.replayed,
            result: InvocationResult {
                output,
                patches: state.patches,
                transactions: state.transactions,
                emitted_events: state.events,
            },
        },
        Err(error) => RoundStep::Failed(error.into()),
    }
}

// ------------------------------------------------------------------ adapter

/// Implemented by `#[extension]` for an impl with `#[export] async fn`
/// methods. Not intended to be implemented by hand.
pub trait AsyncExports {
    /// Export names served by the async executor, sorted.
    const ASYNC_EXPORTS: &'static [&'static str];

    /// The future of async export `export`, or `None` if it is not async.
    fn dispatch_async<'cx>(
        cx: &'cx Cx,
        export: &str,
    ) -> Option<LocalBoxFuture<'cx, Result<Value>>>;

    /// True when async export `export` takes its input as `Value`: the
    /// executor then moves the input into the argument instead of cloning it
    /// every round (H-1). Defaults to false (the input is decoded by
    /// reference, as before).
    fn input_moves(export: &str) -> bool {
        let _ = export;
        false
    }
}

/// The factory value of an extension with async exports. A module whose
/// factory returns it exports ABI 1.1 (`axiom_extension_abi_version`), so
/// the host offers rounds; sync exports of the same impl keep working.
pub struct AsyncExtension(Box<dyn Extension>);

impl AsyncExtension {
    pub fn new<T: TypedExtension + AsyncExports + 'static>(extension: T) -> Self {
        Self(Box::new(RoundsAdapter::<T> {
            sync: crate::boxed_extension(extension),
            sync_rounds: None,
            legacy: None,
            memo: PlanMemo::default(),
            marker: PhantomData,
        }))
    }

    /// The ABI-level extension object (for `GuestMachine` in tests).
    pub fn into_extension(self) -> Box<dyn Extension> {
        self.0
    }
}

impl From<AsyncExtension> for Box<dyn Extension> {
    fn from(value: AsyncExtension) -> Self {
        value.0
    }
}

/// What a source factory may return, and the ABI version its module exports.
/// Used by `export_extension_factory!`; not intended to be implemented by
/// hand.
pub trait FactoryOutput {
    const ABI: abi::AbiVersion;
    fn into_extension(self) -> Box<dyn Extension>;
}

impl FactoryOutput for Box<dyn Extension> {
    const ABI: abi::AbiVersion = abi::ABI_VERSION;
    fn into_extension(self) -> Box<dyn Extension> {
        self
    }
}

impl<E: Extension + 'static> FactoryOutput for Box<E> {
    const ABI: abi::AbiVersion = abi::ABI_VERSION;
    fn into_extension(self) -> Box<dyn Extension> {
        self
    }
}

impl FactoryOutput for AsyncExtension {
    const ABI: abi::AbiVersion = abi::ABI_VERSION_1_1;
    fn into_extension(self) -> Box<dyn Extension> {
        self.0
    }
}

/// `axiom_extension_abi_version` of a factory, evaluated at compile time so
/// the export body stays `i32.const N` (read statically at package time).
#[doc(hidden)]
pub const fn factory_abi_version<F, T>(_factory: &F) -> i32
where
    F: Fn() -> T,
    T: FactoryOutput,
{
    T::ABI.packed()
}

#[doc(hidden)]
pub fn factory_extension<F, T>(factory: F) -> Box<dyn Extension>
where
    F: FnOnce() -> T,
    T: FactoryOutput,
{
    factory().into_extension()
}

struct Legacy {
    request_id: RequestId,
    invocation: Invocation,
    plan_hash: PlanHash,
}

struct RoundsAdapter<T> {
    /// Sync exports of the same impl, through the ordinary typed adapter.
    sync: Box<dyn Extension>,
    /// A sync export inside a 1.1 session: its request and the hashes of the
    /// plans it yielded. Sync exports keep their continuation in instance
    /// memory, exactly as on ABI 1.0.
    sync_rounds: Option<(RequestId, Vec<PlanHash>)>,
    /// An async export on a host that negotiated ABI 1.0 (one yield).
    legacy: Option<Legacy>,
    /// Plan hashes of the async invocation in progress ([`PlanMemo`]).
    memo: PlanMemo,
    marker: PhantomData<fn() -> T>,
}

impl<T: AsyncExports> RoundsAdapter<T> {
    fn is_async(export: &str) -> bool {
        T::ASYNC_EXPORTS.contains(&export)
    }

    fn run(&mut self, request: RoundInvocation) -> RoundStep {
        let move_input = T::input_moves(&request.invocation.export);
        run_round_with(request, move_input, &mut self.memo, |cx| {
            T::dispatch_async(cx, cx.invocation.export())
        })
    }

    fn sync_message(&mut self, request_id: RequestId, round: u16, message: GuestMessage) -> GuestMessage {
        let mut hashes = match self.sync_rounds.take() {
            Some((id, hashes)) if id == request_id => hashes,
            _ => Vec::new(),
        };
        let replayed = hashes.clone();
        let (plan, response) = match message {
            GuestMessage::Yielded { plan, .. } => {
                let plan = RoundPlan::Plan(plan);
                (Some(plan.clone()), plan)
            }
            GuestMessage::YieldedGroup { group, .. } => {
                let plan = RoundPlan::Group(group);
                (Some(plan.clone()), plan)
            }
            GuestMessage::Completed { result, .. } => {
                return GuestMessage::CompletedRound {
                    request_id,
                    round,
                    replayed,
                    result,
                }
            }
            other => return other,
        };
        if let Some(plan) = plan {
            hashes.push(abi::round_plan_hash(&plan));
        }
        self.sync_rounds = Some((request_id, hashes));
        GuestMessage::YieldedRound {
            request_id,
            round,
            replayed,
            plan: response,
        }
    }
}

fn failed(request_id: RequestId, error: ExtensionError) -> GuestMessage {
    GuestMessage::Failed { request_id, error }
}

impl<T: AsyncExports> Extension for RoundsAdapter<T> {
    fn initialize(&mut self) -> core::result::Result<(), ExtensionError> {
        self.sync.initialize()
    }

    fn supports_rounds(&self) -> bool {
        true
    }

    fn invoke_round(&mut self, request: RoundInvocation) -> GuestMessage {
        let request_id = request.request_id;
        let round = request.round;
        if Self::is_async(&request.invocation.export) {
            self.sync_rounds = None;
            return match self.run(request) {
                RoundStep::Completed { replayed, result } => GuestMessage::CompletedRound {
                    request_id,
                    round,
                    replayed,
                    result,
                },
                RoundStep::Yielded { replayed, plan } => GuestMessage::YieldedRound {
                    request_id,
                    round,
                    replayed,
                    plan,
                },
                RoundStep::Failed(error) => failed(request_id, error),
            };
        }
        // A sync export in a 1.1 session: round 1 is `invoke`, every later
        // round resumes with the newest tape record.
        let Some(last) = request.tape.last().cloned() else {
            self.sync_rounds = Some((request_id, Vec::new()));
            let message = self.sync.invoke(request_id, request.invocation);
            return self.sync_message(request_id, round, message);
        };
        let held = match &self.sync_rounds {
            Some((id, hashes)) if *id == request_id => {
                hashes.len() == request.tape.len()
                    && hashes
                        .iter()
                        .zip(&request.tape)
                        .all(|(hash, record)| *hash == record.plan_hash)
            }
            _ => false,
        };
        if !held {
            self.sync_rounds = None;
            return failed(
                request_id,
                error(
                    ErrorCode::Protocol,
                    "a synchronous export cannot continue this invocation: its yielded plan is not held by this instance (sync exports keep their continuation in instance memory; use an async export for replay)",
                ),
            );
        }
        let message = self.sync.resume(request_id, last.outcomes, last.events);
        self.sync_message(request_id, round, message)
    }

    fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage {
        self.legacy = None;
        if !Self::is_async(&invocation.export) {
            return self.sync.invoke(request_id, invocation);
        }
        // The host offered only ABI 1.0: an async export may yield once.
        let request = RoundInvocation {
            request_id,
            invocation: invocation.clone(),
            round: 1,
            max_rounds: 1,
            tape: Vec::new(),
        };
        match self.run(request) {
            RoundStep::Completed { result, .. } => GuestMessage::Completed { request_id, result },
            RoundStep::Yielded { plan, .. } => {
                self.legacy = Some(Legacy {
                    request_id,
                    invocation,
                    plan_hash: abi::round_plan_hash(&plan),
                });
                match plan {
                    RoundPlan::Plan(plan) => GuestMessage::Yielded { request_id, plan },
                    RoundPlan::Group(group) => GuestMessage::YieldedGroup { request_id, group },
                }
            }
            RoundStep::Failed(error) => failed(request_id, error),
        }
    }

    fn resume(
        &mut self,
        request_id: RequestId,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    ) -> GuestMessage {
        let Some(legacy) = self.legacy.take() else {
            return self.sync.resume(request_id, outcomes, events);
        };
        if legacy.request_id != request_id {
            return failed(
                request_id,
                error(ErrorCode::Protocol, "resume does not match the yielded async invocation"),
            );
        }
        let request = RoundInvocation {
            request_id,
            invocation: legacy.invocation,
            round: 2,
            max_rounds: 1,
            tape: alloc::vec![RoundRecord {
                plan_hash: legacy.plan_hash,
                outcomes,
                events,
            }],
        };
        match self.run(request) {
            RoundStep::Completed { result, .. } => GuestMessage::Completed { request_id, result },
            RoundStep::Yielded { .. } => failed(
                request_id,
                error(
                    ErrorCode::Exhausted,
                    "async export needs more than one effect round, but the host negotiated ABI 1.0, which allows one",
                ),
            ),
            RoundStep::Failed(error) => failed(request_id, error),
        }
    }

    fn cancel(&mut self, request_id: RequestId) -> core::result::Result<(), ExtensionError> {
        self.legacy = None;
        self.sync_rounds = None;
        self.memo.clear();
        self.sync.cancel(request_id)
    }

    fn shutdown(&mut self) {
        self.legacy = None;
        self.sync_rounds = None;
        self.memo.clear();
        self.sync.shutdown()
    }
}
