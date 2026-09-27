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
