#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
mod sha256;
mod value_decode;

pub const ABI_NAME: &str = "axiom-extension-abi";
pub const ABI_VERSION: AbiVersion = AbiVersion { major: 1, minor: 0 };
pub const WIRE_MAGIC: [u8; 4] = *b"AXE1";
/// ABI 1.1: multi-round invocations by deterministic replay (ADR 0001).
/// Negotiated; [`ABI_VERSION`] stays 1.0 so every 1.0 frame keeps its bytes.
pub const ABI_VERSION_1_1: AbiVersion = AbiVersion { major: 1, minor: 1 };
/// Every version this crate speaks, newest first: the offer a 1.1 host makes.
pub const SUPPORTED_ABI_VERSIONS: [AbiVersion; 2] = [ABI_VERSION_1_1, ABI_VERSION];
/// Hard ceiling for `budgets.maxRounds`; larger values are rejected, never clamped.
pub const MAX_ROUNDS_CEILING: u16 = 16;
/// Domain separator of [`round_plan_hash`].
pub const ROUND_PLAN_HASH_DOMAIN: &[u8] = b"axiom-extension-round-plan/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
}

impl AbiVersion {
    /// The `axiom_extension_abi_version` export value: `major << 16 | minor`.
    pub const fn packed(self) -> i32 {
        ((self.major as i32) << 16) | self.minor as i32
    }

    pub fn is_supported(self) -> bool {
        SUPPORTED_ABI_VERSIONS.contains(&self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RequestId(pub u64);

impl RequestId {
    pub fn new(value: u64) -> Result<Self, ProtocolError> {
        (value != 0)
            .then_some(Self(value))
            .ok_or(ProtocolError::InvalidRequestId)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Handle {
    pub id: u64,
    pub generation: u32,
    pub kind: HandleKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HandleKind {
    Blob,
    Stream,
    StoreCursor,
    File,
    Secret,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Value {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Record(Vec<Field>),
    Variant {
        case: String,
        value: Option<alloc::boxed::Box<Value>>,
    },
    Handle(Handle),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub resource: String,
    pub revision: u64,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PatchOperation {
    Set {
        path: Vec<String>,
        value: Value,
    },
    Unset {
        path: Vec<String>,
    },
    Increment {
        path: Vec<String>,
        amount: i64,
    },
    Append {
        path: Vec<String>,
        value: Value,
    },
    CompareAndSet {
        path: Vec<String>,
        expected: Value,
        replacement: Value,
    },
    Dispatch {
        action: String,
        payload: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub resource: String,
    pub expected_revision: u64,
    pub operations: Vec<PatchOperation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransactionProposal {
    pub transaction_id: u64,
    pub patches: Vec<Patch>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub namespace: String,
    pub operation: String,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPlan {
    pub effects: Vec<Effect>,
}

pub const EFFECT_GROUP_FORMAT:&str="axiom-extension-effect-group/v1";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectGroupMode {FailFast,Collect}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedEffect {pub name:String,pub effect:Effect}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectGroup {pub format:String,pub limit:u16,pub mode:EffectGroupMode,pub branches:Vec<NamedEffect>}
/// ABI 1.1: the plan a guest yields in one round. A sequential plan keeps the
/// legacy ordered meaning (D9); a group is the explicit concurrent form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RoundPlan {
    Plan(EffectPlan),
    Group(EffectGroup),
}

/// SHA-256 of a round plan: `SHA-256(ROUND_PLAN_HASH_DOMAIN || postcard(plan))`.
pub type PlanHash = [u8; 32];

/// ABI 1.1: one completed round on the host-owned tape. The host builds it
/// from the plan it executed and the outcomes it produced; a guest never
/// supplies or edits a record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundRecord {
    pub plan_hash: PlanHash,
    pub outcomes: Vec<EffectOutcome>,
    pub events: Vec<EventBatch>,
}

/// The ADR 0001 plan hash, over the canonical postcard encoding. Guests
/// return the hashes of the plans they re-derive during replay; the host
/// compares them with the tape. This is a determinism check, not a security
/// boundary: every effect is still authorised when it executes.
pub fn round_plan_hash(plan: &RoundPlan) -> PlanHash {
    let mut hasher = sha256::Sha256::new();
    hasher.update(ROUND_PLAN_HASH_DOMAIN);
    // Serialising an in-memory value to a Vec cannot fail for these types.
    let bytes = postcard::to_allocvec(plan).unwrap_or_default();
    hasher.update(&bytes);
    hasher.finish()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectOutcome {
    pub index: u32,
    pub result: Result<Value, ExtensionError>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventBatch {
    pub subscription: Handle,
    pub events: Vec<Event>,
    pub dropped: u32,
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidInput,
    Denied,
    Conflict,
    Exhausted,
    Deadline,
    Cancelled,
    Host,
    Guest,
    Protocol,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub export: String,
    pub input: Value,
    pub snapshots: Vec<Snapshot>,
    pub deadline_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvocationResult {
    pub output: Value,
    pub patches: Vec<Patch>,
    pub transactions: Vec<TransactionProposal>,
    pub emitted_events: Vec<Event>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostMessage {
    Negotiate {
        supported: Vec<AbiVersion>,
    },
    Initialize {
        instance_id: u64,
        application: String,
        extension: String,
    },
    Invoke {
        request_id: RequestId,
        invocation: Invocation,
    },
    Resume {
        request_id: RequestId,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    },
    Cancel {
        request_id: RequestId,
    },
    Drain,
    Shutdown,
    /// ABI 1.1 only (appended; 1.0 peers never see it). Round `round` of the
    /// invocation: the same `invocation` bytes and request ID in every round,
    /// plus the tape of the `round - 1` earlier rounds.
    InvokeRound {
        request_id: RequestId,
        invocation: Invocation,
        round: u16,
        max_rounds: u16,
        tape: Vec<RoundRecord>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GuestMessage {
    Negotiated {
        selected: AbiVersion,
    },
    Initialized,
    Yielded {
        request_id: RequestId,
        plan: EffectPlan,
    },
    Completed {
        request_id: RequestId,
        result: InvocationResult,
    },
    Failed {
        request_id: RequestId,
        error: ExtensionError,
    },
    Cancelled {
        request_id: RequestId,
    },
    Drained,
    Shutdown,
    YieldedGroup {request_id:RequestId,group:EffectGroup},
    /// ABI 1.1 only: the next plan. `replayed` holds the hash of every plan
    /// the guest re-derived from the tape, in order (one per tape record).
    YieldedRound {
        request_id: RequestId,
        round: u16,
        replayed: Vec<PlanHash>,
        plan: RoundPlan,
    },
    /// ABI 1.1 only: the terminal result after replaying the whole tape.
    CompletedRound {
        request_id: RequestId,
        round: u16,
        replayed: Vec<PlanHash>,
        result: InvocationResult,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame<T> {
    pub magic: [u8; 4],
    pub abi: AbiVersion,
    pub message: T,
}

impl<T> Frame<T> {
    pub fn new(message: T) -> Self {
        Self {
            magic: WIRE_MAGIC,
            abi: ABI_VERSION,
            message,
        }
    }

    /// A frame stamped with the negotiated version (ABI 1.1 sessions).
    pub fn with_abi(abi: AbiVersion, message: T) -> Self {
        Self {
            magic: WIRE_MAGIC,
            abi,
            message,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecLimits {
    pub max_bytes: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    Oversize,
    Malformed,
    NonCanonical,
    WrongMagic,
    AbiMismatch,
    InvalidRequestId,
    InvalidTransition,
    DuplicateTerminal,
    UseAfterShutdown,
}

pub fn encode<T: Serialize>(message: &T, limits: CodecLimits) -> Result<Vec<u8>, ProtocolError> {
    let bytes = postcard::to_allocvec(message).map_err(|_| ProtocolError::Malformed)?;
    if bytes.len() > limits.max_bytes {
        return Err(ProtocolError::Oversize);
    }
    Ok(bytes)
}

pub fn decode<T>(bytes: &[u8], limits: CodecLimits) -> Result<T, ProtocolError>
where
    T: DeserializeOwned + Serialize,
{
    if bytes.len() > limits.max_bytes {
        return Err(ProtocolError::Oversize);
    }
    let (value, remainder) =
        postcard::take_from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    if !remainder.is_empty() {
        return Err(ProtocolError::Malformed);
    }
    if encode(&value, limits)? != bytes {
        return Err(ProtocolError::NonCanonical);
    }
    Ok(value)
}

pub fn decode_frame<T>(bytes: &[u8], limits: CodecLimits) -> Result<Frame<T>, ProtocolError>
where
    T: DeserializeOwned + Serialize,
{
    let frame: Frame<T> = decode(bytes, limits)?;
    if frame.magic != WIRE_MAGIC {
        return Err(ProtocolError::WrongMagic);
    }
    if frame.abi.major != ABI_VERSION.major || frame.abi.minor > ABI_VERSION.minor {
        return Err(ProtocolError::AbiMismatch);
    }
    Ok(frame)
}

/// [`decode_frame`] for a negotiated session: the frame must carry one of the
/// `accepted` versions exactly (1.0 sessions pass `[ABI_VERSION]`, which is
/// what [`decode_frame`] accepts). An empty list accepts nothing.
pub fn decode_frame_versioned<T>(
    bytes: &[u8],
    limits: CodecLimits,
    accepted: &[AbiVersion],
) -> Result<Frame<T>, ProtocolError>
where
    T: DeserializeOwned + Serialize,
{
    let frame: Frame<T> = decode(bytes, limits)?;
    if frame.magic != WIRE_MAGIC {
        return Err(ProtocolError::WrongMagic);
    }
    if !frame.abi.is_supported() || !accepted.contains(&frame.abi) {
        return Err(ProtocolError::AbiMismatch);
    }
    Ok(frame)
}

/// [`decode_frame_trusted`] for a negotiated session (see
/// [`decode_frame_versioned`]).
pub fn decode_frame_trusted_versioned<T>(
    bytes: &[u8],
    limits: CodecLimits,
    accepted: &[AbiVersion],
) -> Result<Frame<T>, ProtocolError>
where
    T: DeserializeOwned,
{
    if bytes.len() > limits.max_bytes {
        return Err(ProtocolError::Oversize);
    }
    let (frame, remainder): (Frame<T>, _) =
        postcard::take_from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    if !remainder.is_empty() {
        return Err(ProtocolError::Malformed);
    }
    if frame.magic != WIRE_MAGIC {
        return Err(ProtocolError::WrongMagic);
    }
    if !frame.abi.is_supported() || !accepted.contains(&frame.abi) {
        return Err(ProtocolError::AbiMismatch);
    }
    Ok(frame)
}

/// Decode a frame from a trusted peer: same size, trailing-data, magic and
/// version checks as [`decode_frame`], without the canonical re-encoding.
/// Guests use this for host frames (the host is trusted and canonical by
/// construction), saving metered guest work on every message. Hosts must keep
/// using [`decode_frame`] for guest output.
pub fn decode_frame_trusted<T>(bytes: &[u8], limits: CodecLimits) -> Result<Frame<T>, ProtocolError>
where
    T: DeserializeOwned,
{
    if bytes.len() > limits.max_bytes {
        return Err(ProtocolError::Oversize);
    }
    let (frame, remainder): (Frame<T>, _) =
        postcard::take_from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    if !remainder.is_empty() {
        return Err(ProtocolError::Malformed);
    }
    if frame.magic != WIRE_MAGIC {
        return Err(ProtocolError::WrongMagic);
    }
    if frame.abi.major != ABI_VERSION.major || frame.abi.minor > ABI_VERSION.minor {
        return Err(ProtocolError::AbiMismatch);
    }
    Ok(frame)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    New,
    Negotiated,
    Ready,
    Running(RequestId),
    Yielded(RequestId),
    /// ABI 1.1: the guest yielded a round plan; only `InvokeRound` for the
    /// next round of the same request, or `Cancel`, may follow.
    Suspended(RequestId),
    Draining,
    /// The guest acknowledged `Drain`; only `Shutdown` may follow. This is the
    /// guest SDK's rule too, and the only host sequence in use (drain, then
    /// shut down), so the protocol checker and the guest cannot disagree.
    Drained,
    Stopping,
    Stopped,
}

/// Protocol-only lifecycle checker. Engines call this before accepting a guest
/// response, so duplicate terminal outcomes and post-shutdown use fail closed.
///
/// The checker records the host's `Negotiate` offer and the guest's selection.
/// A 1.0 session accepts exactly the 1.0 messages (`Invoke`/`Resume`,
/// `Yielded`/`YieldedGroup`/`Completed`); a 1.1 session accepts exactly the
/// round messages (`InvokeRound`, `YieldedRound`/`CompletedRound`). `Failed`,
/// `Cancelled`, `Cancel`, `Drain` and `Shutdown` are shared.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    state: SessionState,
    offered: Vec<AbiVersion>,
    negotiated: Option<AbiVersion>,
    round: u16,
    max_rounds: u16,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            state: SessionState::New,
            offered: Vec::new(),
            negotiated: None,
            round: 0,
            max_rounds: 0,
        }
    }
}

impl Lifecycle {
    /// The version the guest selected, once negotiation has completed.
    pub fn negotiated(&self) -> Option<AbiVersion> {
        self.negotiated
    }

    fn rounds(&self) -> bool {
        self.negotiated == Some(ABI_VERSION_1_1)
    }

    fn legacy(&self) -> bool {
        self.negotiated == Some(ABI_VERSION)
    }

    pub fn host(&mut self, message: &HostMessage) -> Result<(), ProtocolError> {
        if self.state == SessionState::Stopped {
            return Err(ProtocolError::UseAfterShutdown);
        }
        let next = match (self.state, message) {
            (SessionState::New, HostMessage::Negotiate { supported }) => {
                self.offered = supported.clone();
                SessionState::New
            }
            (SessionState::Negotiated, HostMessage::Initialize { .. }) => SessionState::Negotiated,
            (SessionState::Ready, HostMessage::Invoke { request_id, .. }) if self.legacy() => {
                SessionState::Running(*request_id)
            }
            (
                SessionState::Ready,
                HostMessage::InvokeRound {
                    request_id,
                    round: 1,
                    max_rounds,
                    tape,
                    ..
                },
            ) if self.rounds()
                && tape.is_empty()
                && (1..=MAX_ROUNDS_CEILING).contains(max_rounds) =>
            {
                self.round = 1;
                self.max_rounds = *max_rounds;
                SessionState::Running(*request_id)
            }
            (
                SessionState::Suspended(active),
                HostMessage::InvokeRound {
                    request_id,
                    round,
                    max_rounds,
                    tape,
                    ..
                },
            ) if self.rounds()
                && active == *request_id
                && *max_rounds == self.max_rounds
                && *round == self.round.saturating_add(1)
                && *round <= self.max_rounds
                && tape.len() == usize::from(*round - 1) =>
            {
                self.round = *round;
                SessionState::Running(active)
            }
            (SessionState::Yielded(active), HostMessage::Resume { request_id, .. })
                if active == *request_id && self.legacy() =>
            {
                SessionState::Running(active)
            }
            (
                SessionState::Running(active)
                | SessionState::Yielded(active)
                | SessionState::Suspended(active),
                HostMessage::Cancel { request_id },
            ) if active == *request_id => SessionState::Running(active),
            (SessionState::Ready, HostMessage::Drain) => SessionState::Draining,
            (
                SessionState::Ready | SessionState::Draining | SessionState::Drained,
                HostMessage::Shutdown,
            ) => {
                SessionState::Stopping
            }
            _ => return Err(ProtocolError::InvalidTransition),
        };
        self.state = next;
        Ok(())
    }

    pub fn guest(&mut self, message: &GuestMessage) -> Result<(), ProtocolError> {
        if self.state == SessionState::Stopped {
            return Err(ProtocolError::UseAfterShutdown);
        }
        let expected_replays = usize::from(self.round.saturating_sub(1));
        let next = match (self.state, message) {
            (SessionState::New, GuestMessage::Negotiated { selected })
                if selected.is_supported() && self.offered.contains(selected) =>
            {
                self.negotiated = Some(*selected);
                SessionState::Negotiated
            }
            (SessionState::Negotiated, GuestMessage::Initialized) => SessionState::Ready,
            (SessionState::Running(active), GuestMessage::Yielded { request_id, .. } | GuestMessage::YieldedGroup {request_id,..})
                if active == *request_id && self.legacy() =>
            {
                SessionState::Yielded(active)
            }
            (
                SessionState::Running(active),
                GuestMessage::YieldedRound {
                    request_id,
                    round,
                    replayed,
                    ..
                },
            ) if active == *request_id
                && self.rounds()
                && *round == self.round
                && replayed.len() == expected_replays =>
            {
                SessionState::Suspended(active)
            }
            (SessionState::Running(active), GuestMessage::Completed { request_id, .. })
                if active == *request_id && self.legacy() =>
            {
                SessionState::Ready
            }
            (
                SessionState::Running(active),
                GuestMessage::CompletedRound {
                    request_id,
                    round,
                    replayed,
                    ..
                },
            ) if active == *request_id
                && self.rounds()
                && *round == self.round
                && replayed.len() == expected_replays =>
            {
                SessionState::Ready
            }
            (
                SessionState::Running(active),
                GuestMessage::Failed { request_id, .. }
                | GuestMessage::Cancelled { request_id },
            ) if active == *request_id => SessionState::Ready,
            (SessionState::Draining, GuestMessage::Drained) => SessionState::Drained,
            (SessionState::Stopping, GuestMessage::Shutdown) => SessionState::Stopped,
            (
                SessionState::Ready,
                GuestMessage::Completed { .. }
                | GuestMessage::CompletedRound { .. }
                | GuestMessage::Failed { .. }
                | GuestMessage::Cancelled { .. },
            ) => return Err(ProtocolError::DuplicateTerminal),
            _ => return Err(ProtocolError::InvalidTransition),
        };
        if next == SessionState::Ready {
            self.round = 0;
            self.max_rounds = 0;
        }
        self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct ConformanceFixture {
        format: String,
        abi: ConformanceAbi,
        frames: Vec<ConformanceFrame>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ConformanceAbi {
        name: String,
        version: String,
        wire_encoding: String,
        wire_magic_hex: String,
    }

    #[derive(Deserialize)]
    struct ConformanceFrame {
        name: String,
        direction: String,
        hex: String,
    }

    fn invocation() -> Invocation {
        Invocation {
            export: "validate".into(),
            input: Value::String("ok".into()),
            snapshots: Vec::new(),
            deadline_unix_ms: 42,
        }
    }

    #[test]
    fn golden_frame_is_stable_and_round_trips() {
        let frame = Frame::new(HostMessage::Invoke {
            request_id: RequestId::new(7).unwrap(),
            invocation: invocation(),
        });
        let bytes = encode(&frame, CodecLimits::default()).unwrap();
        assert_eq!(
            hex(&bytes),
            "41584531010002070876616c696461746504026f6b002a"
        );
        assert_eq!(
            decode_frame::<HostMessage>(&bytes, CodecLimits::default()).unwrap(),
            frame
        );
    }

    #[test]
    fn language_neutral_conformance_frames_preserve_the_existing_abi() {
        let fixture: ConformanceFixture = serde_json::from_str(include_str!(
            "../fixtures/sdk-interface-conformance-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture.format, "axiom-sdk-conformance/v1");
        assert_eq!(fixture.abi.name, ABI_NAME);
        assert_eq!(fixture.abi.version, "1.0.0");
        assert_eq!(fixture.abi.wire_encoding, "postcard");
        assert_eq!(fixture.abi.wire_magic_hex, hex(&WIRE_MAGIC));
        assert!(!fixture.frames.is_empty());

        for vector in fixture.frames {
            let bytes = from_hex(&vector.hex);
            match vector.direction.as_str() {
                "host" => {
                    let frame = decode_frame::<HostMessage>(&bytes, CodecLimits::default())
                        .unwrap_or_else(|error| panic!("{}: {error:?}", vector.name));
                    assert_eq!(encode(&frame, CodecLimits::default()).unwrap(), bytes);
                }
                "guest" => {
                    let frame = decode_frame::<GuestMessage>(&bytes, CodecLimits::default())
                        .unwrap_or_else(|error| panic!("{}: {error:?}", vector.name));
                    assert_eq!(encode(&frame, CodecLimits::default()).unwrap(), bytes);
                }
                direction => panic!("{}: unsupported direction {direction}", vector.name),
            }
        }
    }

    #[test]
    fn malformed_oversize_and_trailing_data_fail_closed() {
        assert_eq!(
            decode::<Value>(&[255], CodecLimits::default()),
            Err(ProtocolError::Malformed)
        );
        assert_eq!(
            decode::<Value>(&[0, 0], CodecLimits::default()),
            Err(ProtocolError::Malformed)
        );
        assert_eq!(
            decode::<Value>(&[0], CodecLimits { max_bytes: 0 }),
            Err(ProtocolError::Oversize)
        );
    }

    #[test]
    fn deterministic_binary_mutation_corpus_never_panics_or_accepts_noncanonical_frames() {
        let frame = Frame::new(HostMessage::Invoke {
            request_id: RequestId::new(7).unwrap(),
            invocation: invocation(),
        });
        let canonical = encode(&frame, CodecLimits::default()).unwrap();
        let mut corpus = Vec::new();
        for end in 0..canonical.len() {
            corpus.push(canonical[..end].to_vec());
        }
        for index in 0..canonical.len() {
            for mask in [0x01, 0x80, 0xff] {
                let mut mutated = canonical.clone();
                mutated[index] ^= mask;
                corpus.push(mutated);
            }
        }
        corpus.push(vec![0xff; 4_097]);

        for bytes in corpus {
            let decoded = std::panic::catch_unwind(|| {
                decode_frame::<HostMessage>(&bytes, CodecLimits { max_bytes: 4_096 })
            })
            .expect("hostile ABI bytes must never panic the decoder");
            if let Ok(value) = decoded {
                assert_eq!(
                    encode(&value, CodecLimits { max_bytes: 4_096 }).unwrap(),
                    bytes,
                    "accepted ABI frames must remain canonical"
                );
            }
        }
    }

    #[test]
    fn lifecycle_rejects_duplicate_terminal_and_use_after_shutdown() {
        let id = RequestId::new(1).unwrap();
        let mut life = Lifecycle::default();
        life.host(&HostMessage::Negotiate {
            supported: vec![ABI_VERSION],
        })
        .unwrap();
        life.guest(&GuestMessage::Negotiated {
            selected: ABI_VERSION,
        })
        .unwrap();
        life.host(&HostMessage::Initialize {
            instance_id: 1,
            application: "app".into(),
            extension: "ext".into(),
        })
        .unwrap();
        life.guest(&GuestMessage::Initialized).unwrap();
        life.host(&HostMessage::Invoke {
            request_id: id,
            invocation: invocation(),
        })
        .unwrap();
        life.guest(&GuestMessage::Completed {
            request_id: id,
            result: InvocationResult {
                output: Value::Null,
                patches: vec![],
                transactions: vec![],
                emitted_events: vec![],
            },
        })
        .unwrap();
        assert_eq!(
            life.guest(&GuestMessage::Cancelled { request_id: id }),
            Err(ProtocolError::DuplicateTerminal)
        );
        life.host(&HostMessage::Shutdown).unwrap();
        life.guest(&GuestMessage::Shutdown).unwrap();
        assert_eq!(
            life.host(&HostMessage::Drain),
            Err(ProtocolError::UseAfterShutdown)
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|byte| alloc::format!("{byte:02x}"))
            .collect()
    }

    fn from_hex(value: &str) -> Vec<u8> {
        assert_eq!(value.len() % 2, 0);
        value
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let text = core::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}

#[cfg(test)]
mod trusted_decode_tests {
    use super::*;
    use alloc::vec;

    fn frames() -> Vec<HostMessage> {
        let id = RequestId(7);
        vec![
            HostMessage::Negotiate {
                supported: vec![ABI_VERSION],
            },
            HostMessage::Initialize {
                instance_id: 300,
                application: "app".into(),
                extension: "ext".into(),
            },
            HostMessage::Invoke {
                request_id: id,
                invocation: Invocation {
                    export: "run".into(),
                    input: Value::Record(vec![Field {
                        name: "n".into(),
                        value: Value::Unsigned(1 << 40),
                    }]),
                    snapshots: vec![Snapshot {
                        resource: "ui:c".into(),
                        revision: 70_000,
                        value: Value::Signed(-5),
                    }],
                    deadline_unix_ms: 1_700_000_000_000,
                },
            },
            HostMessage::Resume {
                request_id: id,
                outcomes: vec![EffectOutcome {
                    index: 0,
                    result: Ok(Value::Bytes(vec![1, 2, 3])),
                }],
                events: vec![],
            },
            HostMessage::Cancel { request_id: id },
            HostMessage::Drain,
            HostMessage::Shutdown,
        ]
    }

    #[test]
    fn trusted_decode_agrees_with_checked_decode_on_a_mutation_corpus() {
        let limits = CodecLimits { max_bytes: 4096 };
        let mut checked_accepts = 0;
        for message in frames() {
            let canonical = encode(&Frame::new(message), limits).unwrap();
            let mut corpus = vec![canonical.clone()];
            for end in 0..canonical.len() {
                corpus.push(canonical[..end].to_vec());
            }
            for index in 0..canonical.len() {
                for mask in [0x01_u8, 0x80, 0xff] {
                    let mut mutated = canonical.clone();
                    mutated[index] ^= mask;
                    corpus.push(mutated);
                }
            }
            let mut trailing = canonical.clone();
            trailing.push(0);
            corpus.push(trailing);
            for bytes in corpus {
                let checked = decode_frame::<HostMessage>(&bytes, limits);
                let trusted = decode_frame_trusted::<HostMessage>(&bytes, limits);
                match (&checked, &trusted) {
                    (Ok(left), Ok(right)) => {
                        assert_eq!(left, right);
                        checked_accepts += 1;
                    }
                    (Ok(_), Err(error)) => {
                        panic!("trusted decoder rejected a valid frame: {error:?}")
                    }
                    // Only canonical-form violations may be accepted by the trusted decoder.
                    (Err(ProtocolError::NonCanonical), _) | (Err(_), Err(_)) => {}
                    (Err(error), Ok(_)) => {
                        panic!("trusted decoder accepted a frame rejected with {error:?}")
                    }
                }
            }
        }
        assert!(
            checked_accepts >= frames().len(),
            "the corpus must contain accepted frames"
        );
    }

    #[test]
    fn trusted_decode_keeps_size_trailing_magic_and_version_checks() {
        let limits = CodecLimits::default();
        let bytes = encode(&Frame::new(HostMessage::Drain), limits).unwrap();
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode_frame_trusted::<HostMessage>(&trailing, limits),
            Err(ProtocolError::Malformed)
        );
        let mut magic = bytes.clone();
        magic[0] ^= 1;
        assert_eq!(
            decode_frame_trusted::<HostMessage>(&magic, limits),
            Err(ProtocolError::WrongMagic)
        );
        assert_eq!(
            decode_frame_trusted::<HostMessage>(&bytes, CodecLimits { max_bytes: 1 }),
            Err(ProtocolError::Oversize)
        );
        for abi in [
            AbiVersion {
                major: ABI_VERSION.major,
                minor: ABI_VERSION.minor + 1,
            },
            AbiVersion {
                major: ABI_VERSION.major + 1,
                minor: 0,
            },
            AbiVersion { major: 0, minor: 0 },
        ] {
            let frame = Frame {
                magic: WIRE_MAGIC,
                abi,
                message: HostMessage::Drain,
            };
            let bytes = encode(&frame, limits).unwrap();
            assert_eq!(
                decode_frame_trusted::<HostMessage>(&bytes, limits),
                Err(ProtocolError::AbiMismatch)
            );
            assert_eq!(
                decode_frame::<HostMessage>(&bytes, limits),
                Err(ProtocolError::AbiMismatch)
            );
        }
    }
}

#[cfg(test)]
mod drained_tests {
    use super::*;

    #[test]
    fn after_drained_only_shutdown_is_accepted() {
        let ready = || {
            let mut life = Lifecycle::default();
            life.host(&HostMessage::Negotiate {
                supported: alloc::vec![ABI_VERSION],
            })
            .unwrap();
            life.guest(&GuestMessage::Negotiated {
                selected: ABI_VERSION,
            })
            .unwrap();
            life.host(&HostMessage::Initialize {
                instance_id: 1,
                application: "a".into(),
                extension: "e".into(),
            })
            .unwrap();
            life.guest(&GuestMessage::Initialized).unwrap();
            life.host(&HostMessage::Drain).unwrap();
            life.guest(&GuestMessage::Drained).unwrap();
            life
        };
        for rejected in [
            HostMessage::Drain,
            HostMessage::Cancel {
                request_id: RequestId(1),
            },
        ] {
            assert_eq!(
                ready().host(&rejected),
                Err(ProtocolError::InvalidTransition)
            );
        }
        let mut life = ready();
        life.host(&HostMessage::Shutdown).unwrap();
        life.guest(&GuestMessage::Shutdown).unwrap();
    }
}
