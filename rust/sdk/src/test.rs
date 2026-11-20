//! Deterministic in-memory host for extension unit tests.
//!
//! It deliberately operates on ABI proposals: an extension never receives a
//! mutable host reference, and every patch/effect is checked before it is
//! recorded or committed.

use crate::{
    abi::{
        EffectOutcome, ErrorCode, ExtensionError, GuestMessage, Invocation, InvocationResult,
        Patch, PatchOperation, RequestId, Snapshot, TransactionProposal, Value,
    },
    AxiomDecode, AxiomEncode, Extension, Result, SdkError,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditKind {
    Initialize,
    Invoke,
    Snapshot,
    Effect,
    Patch,
    Transaction,
    Event,
    Denied,
    Completed,
    Failed,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditRecord {
    pub kind: AuditKind,
    pub resource: Option<String>,
    pub operation: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Resource {
    revision: u64,
    value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TestRun {
    pub output: Value,
    pub emitted_events: usize,
    pub resumptions: usize,
}

#[derive(Default)]
pub struct TestHost {
    resources: BTreeMap<String, Resource>,
    writes: BTreeSet<(String, String)>,
    dispatches: BTreeSet<(String, String)>,
    effects: BTreeMap<(String, String), core::result::Result<Value, ExtensionError>>,
    audit: Vec<AuditRecord>,
    next_request: u64,
}

impl TestHost {
    pub fn new() -> Self {
        Self {
            next_request: 1,
            ..Self::default()
        }
    }

    pub fn ui_state<T: AxiomEncode>(mut self, scope: &str, revision: u64, value: T) -> Self {
        self.resources.insert(
            format!("ui:{scope}"),
            Resource {
                revision,
                value: value.encode(),
            },
        );
        self
    }

    pub fn store_object<T: AxiomEncode>(
        mut self,
        object: &str,
        id: &str,
        revision: u64,
        value: T,
    ) -> Self {
        self.resources.insert(
            format!("store:{object}:{id}"),
            Resource {
                revision,
                value: value.encode(),
            },
        );
        self
    }

    pub fn allow_ui_write(mut self, scope: &str, path: &str) -> Self {
        self.writes.insert((format!("ui:{scope}"), path.to_owned()));
        self
    }

    pub fn allow_store_write(mut self, object: &str, id: &str, field: &str) -> Self {
        self.writes
            .insert((format!("store:{object}:{id}"), field.to_owned()));
        self
    }

    pub fn allow_dispatch(mut self, resource: &str, action: &str) -> Self {
        self.dispatches
            .insert((resource.to_owned(), action.to_owned()));
        self
    }

    pub fn effect<T: AxiomEncode>(mut self, namespace: &str, operation: &str, output: T) -> Self {
        self.effects.insert(
            (namespace.to_owned(), operation.to_owned()),
            Ok(output.encode()),
        );
        self
    }

    pub fn effect_error(mut self, namespace: &str, operation: &str, error: SdkError) -> Self {
        self.effects.insert(
            (namespace.to_owned(), operation.to_owned()),
            Err(error.into()),
        );
        self
    }

    pub fn invoke<I: AxiomEncode>(
        &mut self,
        extension: &mut dyn Extension,
        export: &str,
        input: I,
    ) -> Result<TestRun> {
        extension.initialize().map_err(sdk_error)?;
        self.record(AuditKind::Initialize, None, None, "initialized");
        let request_id = RequestId::new(self.next_request)
            .map_err(|_| SdkError::invalid_input("test request ID overflowed"))?;
        self.next_request = self.next_request.saturating_add(1);
        let snapshots = self
            .resources
            .iter()
            .map(|(resource, value)| Snapshot {
                resource: resource.clone(),
                revision: value.revision,
                value: value.value.clone(),
            })
            .collect::<Vec<_>>();
        for snapshot in &snapshots {
            self.record(
                AuditKind::Snapshot,
                Some(snapshot.resource.clone()),
                None,
                &format!("revision {}", snapshot.revision),
            );
        }
        self.record(AuditKind::Invoke, None, Some(export.into()), "invoked");
        let mut message = extension.invoke(
            request_id,
            Invocation {
                export: export.into(),
                input: input.encode(),
                snapshots,
                deadline_unix_ms: 1,
            },
        );
        let mut resumptions = 0usize;
        loop {
            match message {
                GuestMessage::YieldedGroup{..} => return self.fail("effect groups require a qualified broker harness"),
                GuestMessage::Yielded { plan, .. } => {
                    if resumptions >= 32 {
                        return self.fail("test host continuation limit exceeded");
                    }
                    let mut outcomes = Vec::with_capacity(plan.effects.len());
                    for (index, effect) in plan.effects.into_iter().enumerate() {
                        let key = (effect.namespace.clone(), effect.operation.clone());
                        let result = self.effects.get(&key).cloned().unwrap_or_else(|| {
                            Err(ExtensionError {
                                code: ErrorCode::Denied,
                                message: format!(
                                    "test host has no granted response for {}.{}",
                                    effect.namespace, effect.operation
                                ),
                                retryable: false,
                            })
                        });
                        let kind = if result.is_ok() {
                            AuditKind::Effect
                        } else {
                            AuditKind::Denied
                        };
                        self.record(
                            kind,
                            Some(effect.namespace),
                            Some(effect.operation),
                            "host effect",
                        );
                        outcomes.push(EffectOutcome {
                            index: index as u32,
                            result,
                        });
                    }
                    resumptions += 1;
                    message = extension.resume(request_id, outcomes, Vec::new());
                }
                GuestMessage::Completed { result, .. } => {
                    if let Err(error) = self.apply_result(&result) {
                        self.record(AuditKind::Failed, None, None, &error.message);
                        extension.shutdown();
                        self.record(AuditKind::Shutdown, None, None, "shutdown");
                        return Err(error);
                    }
                    self.record(AuditKind::Completed, None, None, "completed");
                    extension.shutdown();
                    self.record(AuditKind::Shutdown, None, None, "shutdown");
                    return Ok(TestRun {
                        output: result.output,
                        emitted_events: result.emitted_events.len(),
                        resumptions,
                    });
                }
                GuestMessage::Failed { error, .. } => {
                    self.record(AuditKind::Failed, None, None, &error.message);
                    extension.shutdown();
                    self.record(AuditKind::Shutdown, None, None, "shutdown");
                    return Err(sdk_error(error));
                }
                _ => return self.fail("extension returned an invalid invocation outcome"),
            }
        }
    }

    pub fn ui<T: AxiomDecode>(&self, scope: &str) -> Result<T> {
        self.resource(&format!("ui:{scope}"))
    }

    pub fn store<T: AxiomDecode>(&self, object: &str, id: &str) -> Result<T> {
        self.resource(&format!("store:{object}:{id}"))
    }

    pub fn revision(&self, resource: &str) -> Option<u64> {
        self.resources.get(resource).map(|value| value.revision)
    }

    pub fn audit(&self) -> &[AuditRecord] {
        &self.audit
    }

    pub fn assert_called(&self, namespace: &str, operation: &str) {
        assert!(
            self.audit.iter().any(|record| {
                record.kind == AuditKind::Effect
                    && record.resource.as_deref() == Some(namespace)
                    && record.operation.as_deref() == Some(operation)
            }),
            "expected effect {namespace}.{operation}; audit: {:?}",
            self.audit
        );
    }

    pub fn assert_wrote(&self, resource: &str, path: &str) {
        assert!(
            self.audit.iter().any(|record| {
                record.kind == AuditKind::Patch
                    && record.resource.as_deref() == Some(resource)
                    && record.operation.as_deref() == Some(path)
            }),
            "expected write {resource}.{path}; audit: {:?}",
            self.audit
        );
    }

    pub fn assert_denied(&self) {
        assert!(
            self.audit
                .iter()
                .any(|record| record.kind == AuditKind::Denied),
            "expected a denial; audit: {:?}",
            self.audit
        );
    }

    pub fn assert_no_denials(&self) {
        assert!(
            self.audit
                .iter()
                .all(|record| record.kind != AuditKind::Denied),
            "unexpected denial; audit: {:?}",
            self.audit
        );
    }

    fn resource<T: AxiomDecode>(&self, resource: &str) -> Result<T> {
        let value = self.resources.get(resource).ok_or_else(|| {
            SdkError::invalid_input(format!("test resource `{resource}` is absent"))
        })?;
        T::decode(&value.value)
    }

    fn apply_result(&mut self, result: &InvocationResult) -> Result<()> {
        for patch in &result.patches {
            self.apply_patch(patch)?;
        }
        for transaction in &result.transactions {
            self.apply_transaction(transaction)?;
        }
        for event in &result.emitted_events {
            self.record(
                AuditKind::Event,
                None,
                Some(event.sequence.to_string()),
                "emitted event",
            );
        }
        Ok(())
    }

    fn apply_transaction(&mut self, transaction: &TransactionProposal) -> Result<()> {
        let resources = self.resources.clone();
        let audit_length = self.audit.len();
        for patch in &transaction.patches {
            if let Err(error) = self.apply_patch(patch) {
                self.resources = resources;
                self.audit.truncate(audit_length);
                self.record(
                    AuditKind::Denied,
                    None,
                    Some(transaction.transaction_id.to_string()),
                    &error.message,
                );
                return Err(error);
            }
        }
        self.record(
            AuditKind::Transaction,
            None,
            Some(transaction.transaction_id.to_string()),
            "atomic transaction committed",
        );
        Ok(())
    }

    fn apply_patch(&mut self, patch: &Patch) -> Result<()> {
        let audit_length = self.audit.len();
        if let Err(error) = self.apply_patch_atomically(patch) {
            let denial = self
                .audit
                .get(audit_length..)
                .and_then(|records| {
                    records
                        .iter()
                        .rev()
                        .find(|record| record.kind == AuditKind::Denied)
                })
                .cloned();
            self.audit.truncate(audit_length);
            if let Some(denial) = denial {
                self.audit.push(denial);
            } else {
                self.record(
                    AuditKind::Denied,
                    Some(patch.resource.clone()),
                    None,
                    &error.message,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn apply_patch_atomically(&mut self, patch: &Patch) -> Result<()> {
        let current = self
            .resources
            .get(&patch.resource)
            .ok_or_else(|| SdkError::invalid_input("patch resource is absent"))?;
        if current.revision != patch.expected_revision {
            return self.denied(
                &patch.resource,
                None,
                "patch expected revision does not match",
            );
        }
        let mut next = current.value.clone();
        for operation in &patch.operations {
            match operation {
                PatchOperation::Dispatch { action, .. } => {
                    if !self
                        .dispatches
                        .contains(&(patch.resource.clone(), action.clone()))
                    {
                        return self.denied(
                            &patch.resource,
                            Some(action),
                            "dispatch is not granted",
                        );
                    }
                    self.record(
                        AuditKind::Patch,
                        Some(patch.resource.clone()),
                        Some(action.clone()),
                        "dispatch",
                    );
                }
                operation => {
                    let path = operation_path(operation)?;
                    let joined = path.join(".");
                    if !self
                        .writes
                        .contains(&(patch.resource.clone(), joined.clone()))
                    {
                        return self.denied(
                            &patch.resource,
                            Some(&joined),
                            "state/store write is not granted",
                        );
                    }
                    apply_operation(&mut next, operation)?;
                    self.record(
                        AuditKind::Patch,
                        Some(patch.resource.clone()),
                        Some(joined),
                        "atomic write",
                    );
                }
            }
        }
        let current = self.resources.get_mut(&patch.resource).expect("checked");
        current.value = next;
        current.revision = current.revision.saturating_add(1);
        Ok(())
    }

    fn denied<T>(&mut self, resource: &str, operation: Option<&str>, detail: &str) -> Result<T> {
        self.record(
            AuditKind::Denied,
            Some(resource.into()),
            operation.map(str::to_owned),
            detail,
        );
        Err(SdkError::denied(detail))
    }

    fn fail<T>(&mut self, detail: &str) -> Result<T> {
        self.record(AuditKind::Failed, None, None, detail);
        Err(SdkError::new(ErrorCode::Protocol, detail))
    }

    fn record(
        &mut self,
        kind: AuditKind,
        resource: Option<String>,
        operation: Option<String>,
        detail: &str,
    ) {
        self.audit.push(AuditRecord {
            kind,
            resource,
            operation,
            detail: detail.into(),
        });
    }
}

fn sdk_error(error: ExtensionError) -> SdkError {
    SdkError {
        code: error.code,
        message: error.message,
        retryable: error.retryable,
    }
}

fn operation_path(operation: &PatchOperation) -> Result<&[String]> {
    match operation {
        PatchOperation::Set { path, .. }
        | PatchOperation::Unset { path }
        | PatchOperation::Increment { path, .. }
        | PatchOperation::Append { path, .. }
        | PatchOperation::CompareAndSet { path, .. } => Ok(path),
        PatchOperation::Dispatch { .. } => Err(SdkError::invalid_input(
            "dispatch does not contain a state path",
        )),
    }
}

fn apply_operation(root: &mut Value, operation: &PatchOperation) -> Result<()> {
    let path = operation_path(operation)?;
    if path.is_empty() {
        return Err(SdkError::invalid_input("patch path must not be empty"));
    }
    let (parent, field) = value_parent(root, path)?;
    let Value::Record(fields) = parent else {
        return Err(SdkError::invalid_input("patch parent must be a record"));
    };
    let position = fields.binary_search_by(|candidate| candidate.name.as_str().cmp(field));
    match operation {
        PatchOperation::Set { value, .. } => match position {
            Ok(index) => fields[index].value = value.clone(),
            Err(index) => fields.insert(
                index,
                crate::abi::Field {
                    name: field.into(),
                    value: value.clone(),
                },
            ),
        },
        PatchOperation::Unset { .. } => {
            if let Ok(index) = position {
                fields.remove(index);
            }
        }
        PatchOperation::Increment { amount, .. } => {
            let index = position.map_err(|_| SdkError::conflict("increment target is missing"))?;
            fields[index].value = match fields[index].value {
                Value::Signed(value) => Value::Signed(
                    value
                        .checked_add(*amount)
                        .ok_or_else(|| SdkError::conflict("increment overflowed"))?,
                ),
                Value::Unsigned(value) if *amount >= 0 => Value::Unsigned(
                    value
                        .checked_add(*amount as u64)
                        .ok_or_else(|| SdkError::conflict("increment overflowed"))?,
                ),
                _ => return Err(SdkError::conflict("increment target has incompatible type")),
            };
        }
        PatchOperation::Append { value, .. } => {
            let index = position.map_err(|_| SdkError::conflict("append target is missing"))?;
            let Value::List(values) = &mut fields[index].value else {
                return Err(SdkError::conflict("append target is not a list"));
            };
            values.push(value.clone());
        }
        PatchOperation::CompareAndSet {
            expected,
            replacement,
            ..
        } => {
            let index =
                position.map_err(|_| SdkError::conflict("compare-and-set target is missing"))?;
            if fields[index].value != *expected {
                return Err(SdkError::conflict("compare-and-set expectation failed"));
            }
            fields[index].value = replacement.clone();
        }
        PatchOperation::Dispatch { .. } => unreachable!(),
    }
    Ok(())
}

fn value_parent<'a, 'b>(
    value: &'a mut Value,
    path: &'b [String],
) -> Result<(&'a mut Value, &'b str)> {
    let (field, parents) = path
        .split_last()
        .ok_or_else(|| SdkError::invalid_input("patch path is empty"))?;
    let mut current = value;
    for segment in parents {
        let Value::Record(fields) = current else {
            return Err(SdkError::invalid_input("patch path traverses a non-record"));
        };
        let index = fields
            .binary_search_by(|candidate| candidate.name.as_str().cmp(segment))
            .map_err(|_| SdkError::invalid_input("patch path parent is missing"))?;
        current = &mut fields[index].value;
    }
    Ok((current, field))
}

// ------------------------------------------------------------ X9d replay tapes

/// X9d: replay a dev replay tape (`axiom-extension-replay-tape` version 1,
/// contract `x9d-replay-tape.md`) against an extension in process, through
/// [`crate::GuestMachine`] and real ABI frames, with no host.
///
/// The guest receives the recorded input and round-1 snapshots. Every plan it
/// yields must equal the recorded plan (plan hash and content), otherwise the
/// replay fails with a message naming the first differing call. Outcomes and
/// events come from the tape. The final result (or failure, or the host's
/// refusal or cancellation at the recorded point) must equal the recorded
/// one. Works for ABI 1.0 single-yield guests and ABI 1.1 multi-round guests
/// (async exports, sync exports in a 1.1 module, groups).
#[cfg(feature = "replay-tape")]
pub use replay::{ReplayReport, TapeReplay};

#[cfg(feature = "replay-tape")]
impl TestHost {
    /// Load and verify a tape (format, version, integrity digest, size).
    pub fn from_tape(bytes: &[u8]) -> Result<TapeReplay> {
        TapeReplay::from_bytes(bytes)
    }
}

#[cfg(feature = "replay-tape")]
mod replay {
    use crate::{
        abi::{
            self, AbiVersion, CodecLimits, EffectOutcome, ErrorCode, EventBatch, Frame,
            GuestMessage, HostMessage, Invocation, InvocationResult, PlanHash, RequestId,
            RoundPlan, RoundRecord, Snapshot, Value, ABI_VERSION, ABI_VERSION_1_1,
        },
        Extension, GuestMachine, Result, SdkError,
    };
    use serde_json::Value as Json;

    /// Same cap as the broker's `MAX_TAPE_FILE_BYTES`.
    pub const MAX_TAPE_FILE_BYTES: usize = 32 * 1024 * 1024;
    const LIMITS: CodecLimits = CodecLimits { max_bytes: 16 * 1024 * 1024 };

    #[derive(Clone, Debug)]
    struct Round {
        round: u16,
        plan_hash: PlanHash,
        plan: RoundPlan,
        executed: bool,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    }

    /// A verified tape, ready to replay.
    #[derive(Clone, Debug)]
    pub struct TapeReplay {
        abi: AbiVersion,
        max_rounds: u16,
        application: String,
        extension: String,
        module_sha256: Option<String>,
        invocation: Invocation,
        rounds: Vec<Round>,
        status: String,
        result: Option<InvocationResult>,
        error: Option<(ErrorCode, String)>,
    }

    /// What an identical replay did.
    #[derive(Clone, Debug, PartialEq)]
    pub struct ReplayReport {
        /// Plans the guest yielded (each equal to the recorded plan).
        pub yields: usize,
        /// Recorded terminal status (`completed`, `guestFailed`, `refused`,
        /// `cancelled`, `sandboxFailed`).
        pub status: String,
        /// The guest's final result, when it completed.
        pub result: Option<InvocationResult>,
    }

    fn bad(message: impl core::fmt::Display) -> SdkError {
        SdkError::invalid_input(format!("AXIOM_EXTENSION_REPLAY_TAPE: {message}"))
    }

    fn differs(message: impl core::fmt::Display) -> SdkError {
        SdkError::new(ErrorCode::Protocol, format!("AXIOM_EXTENSION_REPLAY_DIFFERS: {message}"))
    }

    /// Fail on a duplicate object key anywhere in `bytes` (already known to
    /// be valid JSON).
    fn no_duplicate_keys(bytes: &[u8]) -> core::result::Result<(), String> {
        let mut stack: Vec<Option<std::collections::BTreeSet<String>>> = Vec::new();
        let mut last = b' ';
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            match byte {
                b'{' => stack.push(Some(Default::default())),
                b'[' => stack.push(None),
                b'}' | b']' => {
                    stack.pop();
                }
                b'"' => {
                    let start = index;
                    index += 1;
                    while index < bytes.len() && bytes[index] != b'"' {
                        if bytes[index] == b'\\' {
                            index += 1;
                        }
                        index += 1;
                    }
                    let key_position = matches!(last, b'{' | b',');
                    if let Some(Some(keys)) = stack.last_mut() {
                        if key_position {
                            let key: String = serde_json::from_slice(&bytes[start..=index.min(bytes.len() - 1)])
                                .map_err(|error| format!("malformed key: {error}"))?;
                            if !keys.insert(key.clone()) {
                                return Err(format!("malformed tape: duplicate key `{key}`"));
                            }
                        }
                    }
                }
                _ => {}
            }
            if !byte.is_ascii_whitespace() {
                last = bytes[index.min(bytes.len() - 1)];
            }
            index += 1;
        }
        Ok(())
    }

    fn canonical(value: &Json, out: &mut String) {
        match value {
            Json::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    canonical(&map[key], out);
                }
                out.push('}');
            }
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn field<'a>(value: &'a Json, name: &str) -> Result<&'a Json> {
        value.get(name).ok_or_else(|| bad(format!("missing `{name}`")))
    }

    macro_rules! typed {
        ($value:expr, $what:expr) => {
            serde_json::from_value($value.clone()).map_err(|error| bad(format!("malformed {}: {error}", $what)))
        };
    }

    fn text(value: &Json, name: &str) -> Result<String> {
        field(value, name)?.as_str().map(str::to_owned).ok_or_else(|| bad(format!("`{name}` is not a string")))
    }

    fn plan_calls(plan: &RoundPlan) -> Vec<String> {
        match plan {
            RoundPlan::Plan(plan) => plan
                .effects
                .iter()
                .map(|e| format!("{}.{}({})", e.namespace, e.operation, serde_json::to_string(&e.input).unwrap_or_default()))
                .collect(),
            RoundPlan::Group(group) => group
                .branches
                .iter()
                .map(|b| format!("{}.{}({}) as branch `{}`", b.effect.namespace, b.effect.operation, serde_json::to_string(&b.effect.input).unwrap_or_default(), b.name))
                .collect(),
        }
    }

    fn kind(plan: &RoundPlan) -> &'static str {
        match plan {
            RoundPlan::Plan(_) => "plan",
            RoundPlan::Group(_) => "group",
        }
    }

    /// `None` when the plans are equal (hash and content).
    fn diff_plan(round: u16, recorded: &RoundPlan, replayed: &RoundPlan) -> Option<String> {
        if abi::round_plan_hash(recorded) == abi::round_plan_hash(replayed) && recorded == replayed {
            return None;
        }
        let (a, b) = (plan_calls(recorded), plan_calls(replayed));
        if kind(recorded) != kind(replayed) {
            return Some(format!(
                "round {round}: recorded a {} of {} call(s) but the guest yielded a {} of {} call(s)",
                kind(recorded),
                a.len(),
                kind(replayed),
                b.len()
            ));
        }
        for index in 0..a.len().max(b.len()) {
            if a.get(index) != b.get(index) {
                let show = |c: Option<&String>| c.cloned().unwrap_or_else(|| "no call".into());
                return Some(format!(
                    "round {round}, call {index}: recorded {} but the guest yielded {}",
                    show(a.get(index)),
                    show(b.get(index))
                ));
            }
        }
        Some(format!("round {round}: the plan's group settings differ (recorded {recorded:?}, replayed {replayed:?})"))
    }

    impl TapeReplay {
        /// Decode and verify a tape.
        pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
            if bytes.len() > MAX_TAPE_FILE_BYTES {
                return Err(bad(format!("tape is {} bytes, over the {MAX_TAPE_FILE_BYTES}-byte limit", bytes.len())));
            }
            let mut tape: Json = serde_json::from_slice(bytes).map_err(|error| bad(format!("not JSON: {error}")))?;
            // The digest covers the parsed content: refuse bytes that parse
            // to two meanings (a duplicate key; serde_json keeps the last).
            no_duplicate_keys(bytes).map_err(bad)?;
            if tape.get("format").and_then(Json::as_str) != Some("axiom-extension-replay-tape") {
                return Err(bad("not an axiom-extension-replay-tape document"));
            }
            if tape.get("version").and_then(Json::as_u64) != Some(1) {
                return Err(bad("unsupported tape version (this reader knows version 1)"));
            }
            let integrity = tape
                .as_object_mut()
                .and_then(|map| map.remove("integrity"))
                .ok_or_else(|| bad("tape has no integrity digest"))?;
            let mut content = String::new();
            canonical(&tape, &mut content);
            let digest = hex(&abi::sha256_digest(content.as_bytes()));
            if integrity.get("algorithm").and_then(Json::as_str) != Some("sha256")
                || integrity.get("digest").and_then(Json::as_str) != Some(digest.as_str())
            {
                return Err(bad("tape failed its integrity check (content changed after recording)"));
            }
            let header = field(&tape, "header")?;
            // Current recorders refuse instead of redacting (x9d §3, w3-bc3):
            // only a tape of an earlier dev recorder says `replayable: false`.
            if header.get("replayable").and_then(Json::as_bool) != Some(true) {
                return Err(bad("tape has redacted values and cannot be replayed byte-exactly"));
            }
            let abi = match text(&tape, "abi")?.as_str() {
                "1.0" => ABI_VERSION,
                "1.1" => ABI_VERSION_1_1,
                other => return Err(bad(format!("unknown abi `{other}`"))),
            };
            let max_rounds = field(&tape, "maxRounds")?.as_u64().ok_or_else(|| bad("`maxRounds` is not a number"))? as u16;
            let identity = field(&tape, "identity")?;
            let invocation = field(&tape, "invocation")?;
            let snapshots: Vec<Snapshot> = typed!(field(invocation, "snapshots")?, "snapshots")?;
            let input: Value = typed!(field(invocation, "input")?, "input")?;
            let invocation = Invocation { export: text(invocation, "export")?, input, snapshots, deadline_unix_ms: 0 };
            let mut rounds = Vec::new();
            for round in field(&tape, "rounds")?.as_array().ok_or_else(|| bad("`rounds` is not a list"))? {
                let plan: RoundPlan = typed!(field(round, "plan")?, "plan")?;
                let recorded_hash = text(round, "planHash")?;
                let plan_hash = abi::round_plan_hash(&plan);
                if hex(&plan_hash) != recorded_hash {
                    return Err(bad("a recorded plan does not match its recorded plan hash"));
                }
                let executed = text(round, "status")? == "executed";
                let outcomes = match round.get("outcomes") {
                    Some(outcomes) => typed!(outcomes, "outcomes")?,
                    None => Vec::new(),
                };
                let events = match round.get("events") {
                    Some(events) => typed!(events, "events")?,
                    None => Vec::new(),
                };
                rounds.push(Round {
                    round: field(round, "round")?.as_u64().unwrap_or(0) as u16,
                    plan_hash,
                    plan,
                    executed,
                    outcomes,
                    events,
                });
            }
            let terminal = field(&tape, "terminal")?;
            let result = match terminal.get("result") {
                Some(result) => Some({ let v: InvocationResult = typed!(result, "result")?; v }),
                None => None,
            };
            let error = match terminal.get("error") {
                Some(error) => {
                    let code: ErrorCode = typed!(field(error, "code")?, "error code")?;
                    Some((code, text(error, "message")?))
                }
                None => None,
            };
            Ok(Self {
                abi,
                max_rounds,
                application: text(identity, "application")?,
                extension: text(identity, "extension")?,
                module_sha256: identity.get("moduleSha256").and_then(Json::as_str).map(str::to_string),
                invocation,
                rounds,
                status: text(terminal, "status")?,
                result,
                error,
            })
        }

        /// The recorded invocation (deadline 0: wall-clock deadlines are not
        /// recorded).
        pub fn invocation(&self) -> &Invocation {
            &self.invocation
        }

        pub fn export(&self) -> &str {
            &self.invocation.export
        }

        /// The recording's identity: application, extension and module
        /// SHA-256 (when the recording host knew it).
        pub fn identity(&self) -> (&str, &str, Option<&str>) {
            (&self.application, &self.extension, self.module_sha256.as_deref())
        }

        /// Refuse a tape recorded for another extension (the in-process
        /// replay has no package identity of its own; the caller names the
        /// extension it replays). The negotiated ABI is checked by
        /// [`TapeReplay::replay`] itself.
        pub fn expect_extension(self, extension: &str) -> Result<Self> {
            if self.extension != extension {
                return Err(SdkError::invalid_input(format!(
                    "AXIOM_EXTENSION_REPLAY_IDENTITY: the tape was recorded for extension `{}`, not `{extension}`",
                    self.extension
                )));
            }
            Ok(self)
        }

        /// Replay against `extension`. `Ok` means identical; a difference is
        /// an error (`AXIOM_EXTENSION_REPLAY_DIFFERS`) naming where.
        pub fn replay(&self, extension: impl Into<Box<dyn Extension>>) -> Result<ReplayReport> {
            let mut host = Host { machine: GuestMachine::new(extension.into(), LIMITS), abi: ABI_VERSION };
            let offer = if self.abi == ABI_VERSION_1_1 { vec![ABI_VERSION_1_1, ABI_VERSION] } else { vec![ABI_VERSION] };
            match host.send(HostMessage::Negotiate { supported: offer })? {
                GuestMessage::Negotiated { selected } if selected == self.abi => host.abi = selected,
                GuestMessage::Negotiated { selected } => {
                    return Err(differs(format!(
                        "negotiation: the tape was recorded on ABI {}.{} but the guest selected {}.{}",
                        self.abi.major, self.abi.minor, selected.major, selected.minor
                    )))
                }
                other => return Err(differs(format!("negotiation: unexpected reply {other:?}"))),
            }
            host.send(HostMessage::Initialize { instance_id: 1, application: self.application.clone(), extension: self.extension.clone() })?;
            let request_id = RequestId(1);
            if self.abi == ABI_VERSION_1_1 {
                self.replay_rounds(&mut host, request_id)
            } else {
                self.replay_single(&mut host, request_id)
            }
        }

        fn replay_rounds(&self, host: &mut Host, request_id: RequestId) -> Result<ReplayReport> {
            let mut tape: Vec<RoundRecord> = Vec::new();
            for round in 1..=abi::last_round(self.max_rounds.max(1)) {
                let reply = host.send(HostMessage::InvokeRound {
                    request_id,
                    invocation: self.invocation.clone(),
                    round,
                    max_rounds: self.max_rounds,
                    tape: tape.clone(),
                })?;
                match reply {
                    GuestMessage::YieldedRound { replayed, plan, .. } => {
                        let expected: Vec<PlanHash> = tape.iter().map(|r| r.plan_hash).collect();
                        if replayed != expected {
                            return Err(differs(format!("round {round}: the guest's replayed plan hashes do not match the tape")));
                        }
                        if let Some(done) = self.yielded(host, request_id, tape.len(), &plan)? {
                            return Ok(done);
                        }
                        let recorded = &self.rounds[tape.len()];
                        tape.push(RoundRecord { plan_hash: recorded.plan_hash, outcomes: recorded.outcomes.clone(), events: recorded.events.clone() });
                    }
                    GuestMessage::CompletedRound { result, .. } => return self.completed(tape.len(), result),
                    other => return self.ended(tape.len(), other),
                }
            }
            Err(differs("the guest yielded more often than maxRounds allows"))
        }

        fn replay_single(&self, host: &mut Host, request_id: RequestId) -> Result<ReplayReport> {
            let reply = host.send(HostMessage::Invoke { request_id, invocation: self.invocation.clone() })?;
            let plan = match reply {
                GuestMessage::Yielded { plan, .. } => RoundPlan::Plan(plan),
                GuestMessage::YieldedGroup { group, .. } => RoundPlan::Group(group),
                GuestMessage::Completed { result, .. } => return self.completed(0, result),
                other => return self.ended(0, other),
            };
            if let Some(done) = self.yielded(host, request_id, 0, &plan)? {
                return Ok(done);
            }
            let recorded = &self.rounds[0];
            match host.send(HostMessage::Resume { request_id, outcomes: recorded.outcomes.clone(), events: recorded.events.clone() })? {
                GuestMessage::Completed { result, .. } => self.completed(1, result),
                GuestMessage::Yielded { plan, .. } => {
                    self.yielded(host, request_id, 1, &RoundPlan::Plan(plan))?;
                    Err(differs("the guest yielded a second time on ABI 1.0"))
                }
                other => self.ended(1, other),
            }
        }

        /// A yield number `index + 1`: compare it with the tape. `Some` when
        /// the recorded run ended here (the host stopped the guest).
        fn yielded(&self, host: &mut Host, request_id: RequestId, index: usize, plan: &RoundPlan) -> Result<Option<ReplayReport>> {
            let Some(recorded) = self.rounds.get(index) else {
                return Err(differs(format!(
                    "round {}: recorded no further yield (the invocation ended `{}`) but the guest yielded {}",
                    index + 1,
                    self.status,
                    plan_calls(plan).join(", ")
                )));
            };
            if let Some(diff) = diff_plan(recorded.round, &recorded.plan, plan) {
                return Err(differs(diff));
            }
            let last = index + 1 == self.rounds.len();
            if recorded.executed && !(last && self.ends_after_last_yield()) {
                return Ok(None);
            }
            // The host stopped the guest after this yield (refused plan,
            // failed execution, cancellation between rounds).
            match host.send(HostMessage::Cancel { request_id })? {
                GuestMessage::Cancelled { .. } => Ok(Some(ReplayReport { yields: index + 1, status: self.status.clone(), result: None })),
                other => Err(differs(format!("the guest did not accept the host's cancel: {other:?}"))),
            }
        }

        fn ends_after_last_yield(&self) -> bool {
            self.result.is_none() && matches!(self.status.as_str(), "refused" | "cancelled" | "sandboxFailed")
        }

        fn completed(&self, yields: usize, result: InvocationResult) -> Result<ReplayReport> {
            if let Some(next) = self.rounds.get(yields) {
                return Err(differs(format!(
                    "round {}: recorded a yield of {} but the guest completed",
                    next.round,
                    plan_calls(&next.plan).join(", ")
                )));
            }
            match &self.result {
                Some(recorded) if *recorded == result => Ok(ReplayReport { yields, status: self.status.clone(), result: Some(result) }),
                Some(recorded) => Err(differs(format!(
                    "result: recorded {} but the guest returned {}",
                    serde_json::to_string(recorded).unwrap_or_default(),
                    serde_json::to_string(&result).unwrap_or_default()
                ))),
                None => Err(differs(format!("result: recorded `{}` but the guest completed", self.status))),
            }
        }

        fn ended(&self, yields: usize, reply: GuestMessage) -> Result<ReplayReport> {
            if let Some(next) = self.rounds.get(yields) {
                return Err(differs(format!(
                    "round {}: recorded a yield of {} but the guest replied {reply:?}",
                    next.round,
                    plan_calls(&next.plan).join(", ")
                )));
            }
            match reply {
                GuestMessage::Failed { error, .. } => match &self.error {
                    Some((code, message)) if self.status == "guestFailed" && *code == error.code && *message == error.message => {
                        Ok(ReplayReport { yields, status: self.status.clone(), result: None })
                    }
                    _ => Err(differs(format!(
                        "result: recorded `{}` {:?} but the guest failed {:?}: {}",
                        self.status, self.error, error.code, error.message
                    ))),
                },
                GuestMessage::Cancelled { .. } if self.status == "cancelled" => Ok(ReplayReport { yields, status: self.status.clone(), result: None }),
                other => Err(differs(format!("result: recorded `{}` but the guest replied {other:?}", self.status))),
            }
        }
    }

    struct Host {
        machine: GuestMachine<Box<dyn Extension>>,
        abi: AbiVersion,
    }

    impl Host {
        fn send(&mut self, message: HostMessage) -> Result<GuestMessage> {
            let negotiating = matches!(message, HostMessage::Negotiate { .. });
            let frame = if negotiating || self.abi == ABI_VERSION { Frame::new(message) } else { Frame::with_abi(self.abi, message) };
            let bytes = abi::encode(&frame, LIMITS).map_err(|error| bad(format!("frame: {error:?}")))?;
            let reply = self.machine.dispatch(&bytes).map_err(|error| {
                differs(format!("the guest rejected a frame ({error:?}): {}", self.machine.last_error().unwrap_or("")))
            })?;
            let accepted = if negotiating { ABI_VERSION } else { self.abi };
            let frame: Frame<GuestMessage> = abi::decode_frame_versioned(&reply, LIMITS, &[accepted])
                .map_err(|error| differs(format!("the guest sent an invalid frame: {error:?}")))?;
            Ok(frame.message)
        }
    }
}
