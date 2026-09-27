#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const ABI_NAME: &str = "axiom-extension-abi";
pub const ABI_VERSION: AbiVersion = AbiVersion { major: 1, minor: 0 };
pub const WIRE_MAGIC: [u8; 4] = *b"AXE1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    New,
    Negotiated,
    Ready,
    Running(RequestId),
    Yielded(RequestId),
    Draining,
    Stopping,
    Stopped,
}

/// Protocol-only lifecycle checker. Engines call this before accepting a guest
/// response, so duplicate terminal outcomes and post-shutdown use fail closed.
pub struct Lifecycle {
    state: SessionState,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            state: SessionState::New,
        }
    }
}

impl Lifecycle {
    pub fn host(&mut self, message: &HostMessage) -> Result<(), ProtocolError> {
        if self.state == SessionState::Stopped {
            return Err(ProtocolError::UseAfterShutdown);
        }
        self.state = match (self.state, message) {
            (SessionState::New, HostMessage::Negotiate { .. }) => SessionState::New,
            (SessionState::Negotiated, HostMessage::Initialize { .. }) => SessionState::Negotiated,
            (SessionState::Ready, HostMessage::Invoke { request_id, .. }) => {
                SessionState::Running(*request_id)
            }
            (SessionState::Yielded(active), HostMessage::Resume { request_id, .. })
                if active == *request_id =>
            {
                SessionState::Running(active)
            }
            (
                SessionState::Running(active) | SessionState::Yielded(active),
                HostMessage::Cancel { request_id },
            ) if active == *request_id => SessionState::Running(active),
            (SessionState::Ready, HostMessage::Drain) => SessionState::Draining,
            (SessionState::Ready | SessionState::Draining, HostMessage::Shutdown) => {
                SessionState::Stopping
            }
            _ => return Err(ProtocolError::InvalidTransition),
        };
        Ok(())
    }

    pub fn guest(&mut self, message: &GuestMessage) -> Result<(), ProtocolError> {
        if self.state == SessionState::Stopped {
            return Err(ProtocolError::UseAfterShutdown);
        }
        self.state = match (self.state, message) {
            (SessionState::New, GuestMessage::Negotiated { selected })
                if *selected == ABI_VERSION =>
            {
                SessionState::Negotiated
            }
            (SessionState::Negotiated, GuestMessage::Initialized) => SessionState::Ready,
            (SessionState::Running(active), GuestMessage::Yielded { request_id, .. })
                if active == *request_id =>
            {
                SessionState::Yielded(active)
            }
            (
                SessionState::Running(active),
                GuestMessage::Completed { request_id, .. }
                | GuestMessage::Failed { request_id, .. }
                | GuestMessage::Cancelled { request_id },
            ) if active == *request_id => SessionState::Ready,
            (SessionState::Draining, GuestMessage::Drained) => SessionState::Ready,
            (SessionState::Stopping, GuestMessage::Shutdown) => SessionState::Stopped,
            (
                SessionState::Ready,
                GuestMessage::Completed { .. }
                | GuestMessage::Failed { .. }
                | GuestMessage::Cancelled { .. },
            ) => return Err(ProtocolError::DuplicateTerminal),
            _ => return Err(ProtocolError::InvalidTransition),
        };
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
            "../../../../axiom-lib/fixtures/sdk-interface-conformance-v1.json"
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
            .chunks_exact(2)
            .map(|pair| {
                let text = core::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
