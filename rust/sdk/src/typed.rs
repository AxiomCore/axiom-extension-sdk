use crate::{abi, Binding, Extension};
use abi::{
    Effect, EffectOutcome, ErrorCode, Event, EventBatch, ExtensionError, GuestMessage, Handle,
    HandleKind, Invocation, InvocationResult, Patch, PatchOperation, RequestId,
    TransactionProposal, Value,
};
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::marker::PhantomData;

pub type Result<T> = core::result::Result<T, SdkError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl SdkError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Denied, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }
}

impl core::fmt::Display for SdkError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<SdkError> for ExtensionError {
    fn from(value: SdkError) -> Self {
        Self {
            code: value.code,
            message: value.message,
            retryable: value.retryable,
        }
    }
}

pub trait AxiomEncode {
    fn encode(self) -> Value;
}

pub trait AxiomDecode: Sized {
    fn decode(value: &Value) -> Result<Self>;
}

pub trait AxiomType: AxiomEncode + AxiomDecode {}
impl<T: AxiomEncode + AxiomDecode> AxiomType for T {}

impl AxiomEncode for Value {
    fn encode(self) -> Value {
        self
    }
}

impl AxiomDecode for Value {
    fn decode(value: &Value) -> Result<Self> {
        Ok(value.clone())
    }
}

impl AxiomEncode for () {
    fn encode(self) -> Value {
        Value::Null
    }
}

impl AxiomDecode for () {
    fn decode(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(()),
            _ => Err(type_error("null", value)),
        }
    }
}

impl AxiomEncode for bool {
    fn encode(self) -> Value {
        Value::Bool(self)
    }
}

impl AxiomDecode for bool {
    fn decode(value: &Value) -> Result<Self> {
        match value {
            Value::Bool(value) => Ok(*value),
            _ => Err(type_error("boolean", value)),
        }
    }
}

macro_rules! unsigned_type {
    ($type:ty) => {
        impl AxiomEncode for $type {
            fn encode(self) -> Value {
                Value::Unsigned(u64::from(self))
            }
        }

        impl AxiomDecode for $type {
            fn decode(value: &Value) -> Result<Self> {
                let Value::Unsigned(value) = value else {
                    return Err(type_error("unsigned integer", value));
                };
                <$type>::try_from(*value)
                    .map_err(|_| SdkError::invalid_input("unsigned integer is out of range"))
            }
        }
    };
}

unsigned_type!(u16);
unsigned_type!(u32);
unsigned_type!(u64);

macro_rules! signed_type {
    ($type:ty) => {
        impl AxiomEncode for $type {
            fn encode(self) -> Value {
                Value::Signed(i64::from(self))
            }
        }

        impl AxiomDecode for $type {
            fn decode(value: &Value) -> Result<Self> {
                let Value::Signed(value) = value else {
                    return Err(type_error("signed integer", value));
                };
                <$type>::try_from(*value)
                    .map_err(|_| SdkError::invalid_input("signed integer is out of range"))
            }
        }
    };
}

signed_type!(i32);
signed_type!(i64);

impl AxiomEncode for String {
    fn encode(self) -> Value {
        Value::String(self)
    }
}

impl AxiomEncode for &str {
    fn encode(self) -> Value {
        Value::String(self.into())
    }
}

impl AxiomDecode for String {
    fn decode(value: &Value) -> Result<Self> {
        match value {
            Value::String(value) => Ok(value.clone()),
            _ => Err(type_error("string", value)),
        }
    }
}

impl<T: AxiomEncode> AxiomEncode for Option<T> {
    fn encode(self) -> Value {
        self.map(AxiomEncode::encode).unwrap_or(Value::Null)
    }
}

impl<T: AxiomDecode> AxiomDecode for Option<T> {
    fn decode(value: &Value) -> Result<Self> {
        match value {
            Value::Null => Ok(None),
            value => T::decode(value).map(Some),
        }
    }
}

impl<T: AxiomEncode> AxiomEncode for Vec<T> {
    fn encode(self) -> Value {
        Value::List(self.into_iter().map(AxiomEncode::encode).collect())
    }
}

impl<T: AxiomDecode> AxiomDecode for Vec<T> {
    fn decode(value: &Value) -> Result<Self> {
        let Value::List(values) = value else {
            return Err(type_error("list", value));
        };
        values.iter().map(T::decode).collect()
    }
}

impl<T: AxiomEncode> AxiomEncode for BTreeMap<String, T> {
    fn encode(self) -> Value {
        canonical_record(
            self.into_iter()
                .map(|(name, value)| (name, value.encode()))
                .collect(),
        )
    }
}

impl<T: AxiomDecode> AxiomDecode for BTreeMap<String, T> {
    fn decode(value: &Value) -> Result<Self> {
        let record = strict_record(value)?;
        record
            .into_iter()
            .map(|(name, value)| T::decode(value).map(|value| (name.into(), value)))
            .collect()
    }
}

pub struct RecordDecoder<'a> {
    fields: BTreeMap<&'a str, &'a Value>,
}

impl<'a> RecordDecoder<'a> {
    pub fn new(value: &'a Value) -> Result<Self> {
        Ok(Self {
            fields: strict_record(value)?,
        })
    }

    pub fn required<T: AxiomDecode>(&mut self, name: &str) -> Result<T> {
        let value = self
            .fields
            .remove(name)
            .ok_or_else(|| SdkError::invalid_input(format!("record field `{name}` is required")))?;
        T::decode(value).map_err(|mut error| {
            error.message = format!("record field `{name}`: {}", error.message);
            error
        })
    }

    pub fn finish(self) -> Result<()> {
        if let Some(name) = self.fields.keys().next() {
            return Err(SdkError::invalid_input(format!(
                "record contains undeclared field `{name}`"
            )));
        }
        Ok(())
    }
}

pub fn canonical_record<K: Into<String>>(fields: Vec<(K, Value)>) -> Value {
    let mut fields: Vec<_> = fields
        .into_iter()
        .map(|(name, value)| abi::Field {
            name: name.into(),
            value,
        })
        .collect();
    fields.sort_by(|left, right| left.name.cmp(&right.name));
    Value::Record(fields)
}

fn strict_record(value: &Value) -> Result<BTreeMap<&str, &Value>> {
    let Value::Record(fields) = value else {
        return Err(type_error("record", value));
    };
    if fields.windows(2).any(|pair| pair[0].name >= pair[1].name) {
        return Err(SdkError::invalid_input(
            "record fields must be canonical, sorted, and unique",
        ));
    }
    Ok(fields
        .iter()
        .map(|field| (field.name.as_str(), &field.value))
        .collect())
}

fn type_error(expected: &str, actual: &Value) -> SdkError {
    SdkError::invalid_input(format!(
        "expected {expected}, received {}",
        value_kind(actual)
    ))
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Signed(_) => "signed integer",
        Value::Unsigned(_) => "unsigned integer",
        Value::String(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::List(_) => "list",
        Value::Record(_) => "record",
        Value::Variant { .. } => "variant",
        Value::Handle(_) => "handle",
    }
}

#[derive(Clone, Debug)]
pub struct TypedInvocation {
    request_id: RequestId,
    invocation: Invocation,
}

impl TypedInvocation {
    pub fn new(request_id: RequestId, invocation: Invocation) -> Self {
        Self {
            request_id,
            invocation,
        }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn export(&self) -> &str {
        &self.invocation.export
    }

    pub fn require_export(&self, expected: &str) -> Result<()> {
        if self.export() == expected {
            Ok(())
        } else {
            Err(SdkError::invalid_input(format!(
                "unknown export `{}`; expected `{expected}`",
                self.export()
            )))
        }
    }

    pub fn input<T: AxiomDecode>(&self) -> Result<T> {
        T::decode(&self.invocation.input)
    }

    pub fn deadline_unix_ms(&self) -> u64 {
        self.invocation.deadline_unix_ms
    }

    fn snapshot(&self, resource: &str) -> Option<&abi::Snapshot> {
        self.invocation
            .snapshots
            .iter()
            .find(|snapshot| snapshot.resource == resource)
    }
}

#[derive(Clone, Debug)]
pub struct ResumeContext {
    request_id: RequestId,
    outcomes: Vec<EffectOutcome>,
    events: Vec<EventBatch>,
}

impl ResumeContext {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub fn outcome<T: AxiomDecode>(&self, index: u32) -> Result<T> {
        let outcome = self
            .outcomes
            .iter()
            .find(|outcome| outcome.index == index)
            .ok_or_else(|| SdkError::invalid_input("effect outcome is missing"))?;
        match &outcome.result {
            Ok(value) => T::decode(value),
            Err(error) => Err(SdkError {
                code: error.code,
                message: error.message.clone(),
                retryable: error.retryable,
            }),
        }
    }

    pub fn event_batches(&self) -> &[EventBatch] {
        &self.events
    }

    /// Checked generated resume tables inspect the complete correlation set;
    /// selecting the first matching index alone cannot detect duplicate replies.
    pub fn outcomes(&self) -> &[EffectOutcome] { &self.outcomes }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionResponse {
    output: Value,
    patches: Vec<Patch>,
    transactions: Vec<TransactionProposal>,
    events: Vec<Event>,
    effects: Option<abi::EffectPlan>,
    group: Option<abi::EffectGroup>,
}

impl ExtensionResponse {
    pub fn new<T: AxiomEncode>(output: T) -> Self {
        Self {
            output: output.encode(),
            patches: Vec::new(),
            transactions: Vec::new(),
            events: Vec::new(),
            effects: None,
            group: None,
        }
    }

    /// Suspend this invocation while the host executes the generated,
    /// capability-scoped effect plan. Continue in [`TypedExtension::resume`].
    pub fn yield_for(effects: Effects) -> Self {
        Self {
            output: Value::Null,
            patches: Vec::new(),
            transactions: Vec::new(),
            events: Vec::new(),
            effects: Some(effects.build()),
            group: None,
        }
    }

    /// A finite host-owned group. Hosts without the explicit capability reject.
    /// Branch names/indexes retain declaration order; this adds no grants.
    pub fn yield_group(group: abi::EffectGroup) -> Self {
        Self {
            output: Value::Null,
            patches: Vec::new(),
            transactions: Vec::new(),
            events: Vec::new(),
            effects: None,
            group: Some(group),
        }
    }
    pub fn patch(mut self, patch: impl Into<Patch>) -> Self {
        self.patches.push(patch.into());
        self
    }

    pub fn transaction(mut self, transaction: impl Into<TransactionProposal>) -> Self {
        self.transactions.push(transaction.into());
        self
    }

    pub fn emit<T: AxiomEncode>(mut self, sequence: u64, value: T) -> Self {
        self.events.push(Event {
            sequence,
            value: value.encode(),
        });
        self
    }

    fn into_message(self, request_id: RequestId) -> GuestMessage {
        if let Some(group) = self.group {
            return GuestMessage::YieldedGroup { request_id, group };
        }
        if let Some(plan) = self.effects {
            return GuestMessage::Yielded { request_id, plan };
        }
        GuestMessage::Completed {
            request_id,
            result: InvocationResult {
                output: self.output,
                patches: self.patches,
                transactions: self.transactions,
                emitted_events: self.events,
            },
        }
    }
}

pub trait TypedExtension {
    fn initialize(&mut self) -> Result<()> {
        Ok(())
    }

    fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse>;

    fn resume(&mut self, _resume: ResumeContext) -> Result<ExtensionResponse> {
        Err(SdkError::invalid_input(
            "extension did not declare resumable effects or streams",
        ))
    }

    fn cancel(&mut self, _request_id: RequestId) -> Result<()> {
        Ok(())
    }

    fn shutdown(&mut self) {}
}

pub struct TypedAdapter<T>(T);

/// Erase a typed implementation behind the ABI-compatible extension trait
/// object expected by the generated source wrapper.
pub fn boxed_extension<T: TypedExtension + 'static>(
    extension: T,
) -> alloc::boxed::Box<dyn Extension> {
    alloc::boxed::Box::new(TypedAdapter(extension))
}

impl<T: TypedExtension> Extension for TypedAdapter<T> {
    fn initialize(&mut self) -> core::result::Result<(), ExtensionError> {
        TypedExtension::initialize(&mut self.0).map_err(Into::into)
    }

    fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage {
        match TypedExtension::invoke(&mut self.0, TypedInvocation::new(request_id, invocation)) {
            Ok(response) => response.into_message(request_id),
            Err(error) => failed(request_id, error),
        }
    }

    fn resume(
        &mut self,
        request_id: RequestId,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    ) -> GuestMessage {
        match TypedExtension::resume(
            &mut self.0,
            ResumeContext {
                request_id,
                outcomes,
                events,
            },
        ) {
            Ok(response) => response.into_message(request_id),
            Err(error) => failed(request_id, error),
        }
    }

    fn cancel(&mut self, request_id: RequestId) -> core::result::Result<(), ExtensionError> {
        TypedExtension::cancel(&mut self.0, request_id).map_err(Into::into)
    }

    fn shutdown(&mut self) {
        TypedExtension::shutdown(&mut self.0)
    }
}

fn failed(request_id: RequestId, error: SdkError) -> GuestMessage {
    GuestMessage::Failed {
        request_id,
        error: error.into(),
    }
}

pub trait UiScope {
    const SCOPE: &'static str;
}

pub struct UiField<T> {
    path: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> UiField<T> {
    #[doc(hidden)]
    pub const fn generated(path: &'static str) -> Self {
        Self {
            path,
            marker: PhantomData,
        }
    }
}

pub struct UiState<S> {
    revision: u64,
    value: Value,
    marker: PhantomData<fn() -> S>,
}

impl<S: UiScope> UiState<S> {
    #[doc(hidden)]
    pub fn generated(invocation: &TypedInvocation) -> Result<Self> {
        let resource = format!("ui:{}", S::SCOPE);
        let snapshot = invocation.snapshot(&resource).ok_or_else(|| {
            SdkError::invalid_input(format!("authorized UI snapshot `{resource}` is missing"))
        })?;
        Ok(Self {
            revision: snapshot.revision,
            value: snapshot.value.clone(),
            marker: PhantomData,
        })
    }

    #[doc(hidden)]
    pub fn generated_read<T: AxiomDecode>(&self, field: &UiField<T>) -> Result<T> {
        T::decode(value_at_path(&self.value, field.path)?)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn patch(&self) -> UiPatch<S> {
        UiPatch {
            expected_revision: self.revision,
            operations: Vec::new(),
            marker: PhantomData,
        }
    }
}

pub struct UiPatch<S: UiScope> {
    expected_revision: u64,
    operations: Vec<PatchOperation>,
    marker: PhantomData<fn() -> S>,
}

impl<S: UiScope> UiPatch<S> {
    #[doc(hidden)]
    pub fn generated_set<T: AxiomEncode, Field>(
        mut self,
        field: &UiField<Field>,
        value: T,
    ) -> Self {
        self.operations.push(PatchOperation::Set {
            path: split_path(field.path),
            value: value.encode(),
        });
        self
    }

    #[doc(hidden)]
    pub fn generated_unset<Field>(mut self, field: &UiField<Field>) -> Self {
        self.operations.push(PatchOperation::Unset {
            path: split_path(field.path),
        });
        self
    }

    #[doc(hidden)]
    pub fn generated_dispatch<T: AxiomEncode>(mut self, action: &str, payload: T) -> Self {
        self.operations.push(PatchOperation::Dispatch {
            action: action.into(),
            payload: payload.encode(),
        });
        self
    }
}

impl<S: UiScope> From<UiPatch<S>> for Patch {
    fn from(value: UiPatch<S>) -> Self {
        Self {
            resource: format!("ui:{}", S::SCOPE),
            expected_revision: value.expected_revision,
            operations: value.operations,
        }
    }
}

pub trait StoreObject {
    const OBJECT: &'static str;
}

pub struct StoreField<T> {
    field: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> StoreField<T> {
    #[doc(hidden)]
    pub const fn generated(field: &'static str) -> Self {
        Self {
            field,
            marker: PhantomData,
        }
    }
}

pub struct Store<S> {
    id: String,
    revision: u64,
    value: Value,
    marker: PhantomData<fn() -> S>,
}

impl<S: StoreObject> Store<S> {
    #[doc(hidden)]
    pub fn generated(invocation: &TypedInvocation, id: &str) -> Result<Self> {
        let resource = format!("store:{}:{id}", S::OBJECT);
        let snapshot = invocation.snapshot(&resource).ok_or_else(|| {
            SdkError::invalid_input(format!("authorized store snapshot `{resource}` is missing"))
        })?;
        Ok(Self {
            id: id.into(),
            revision: snapshot.revision,
            value: snapshot.value.clone(),
            marker: PhantomData,
        })
    }

    #[doc(hidden)]
    pub fn generated_read<T: AxiomDecode>(&self, field: &StoreField<T>) -> Result<T> {
        T::decode(value_at_path(&self.value, field.field)?)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn patch(&self) -> StorePatch<S> {
        StorePatch {
            id: self.id.clone(),
            expected_revision: self.revision,
            operations: Vec::new(),
            marker: PhantomData,
        }
    }

    pub fn transaction(&self, transaction_id: u64) -> Result<StoreTransaction<S>> {
        if transaction_id == 0 {
            return Err(SdkError::invalid_input("transaction ID must be non-zero"));
        }
        Ok(StoreTransaction {
            transaction_id,
            patch: self.patch(),
        })
    }
}

pub struct StorePatch<S: StoreObject> {
    id: String,
    expected_revision: u64,
    operations: Vec<PatchOperation>,
    marker: PhantomData<fn() -> S>,
}

impl<S: StoreObject> StorePatch<S> {
    #[doc(hidden)]
    pub fn generated_set<T: AxiomEncode, Field>(
        mut self,
        field: &StoreField<Field>,
        value: T,
    ) -> Self {
        self.operations.push(PatchOperation::Set {
            path: split_path(field.field),
            value: value.encode(),
        });
        self
    }

    #[doc(hidden)]
    pub fn generated_unset<Field>(mut self, field: &StoreField<Field>) -> Self {
        self.operations.push(PatchOperation::Unset {
            path: split_path(field.field),
        });
        self
    }

    #[doc(hidden)]
    pub fn generated_action<T: AxiomEncode>(mut self, action: &str, payload: T) -> Self {
        self.operations.push(PatchOperation::Dispatch {
            action: action.into(),
            payload: payload.encode(),
        });
        self
    }
}

impl<S: StoreObject> From<StorePatch<S>> for Patch {
    fn from(value: StorePatch<S>) -> Self {
        Self {
            resource: format!("store:{}:{}", S::OBJECT, value.id),
            expected_revision: value.expected_revision,
            operations: value.operations,
        }
    }
}

pub struct StoreTransaction<S: StoreObject> {
    transaction_id: u64,
    patch: StorePatch<S>,
}

impl<S: StoreObject> StoreTransaction<S> {
    #[doc(hidden)]
    pub fn generated_set<T: AxiomEncode, Field>(
        mut self,
        field: &StoreField<Field>,
        value: T,
    ) -> Self {
        self.patch = self.patch.generated_set(field, value);
        self
    }

    #[doc(hidden)]
    pub fn generated_unset<Field>(mut self, field: &StoreField<Field>) -> Self {
        self.patch = self.patch.generated_unset(field);
        self
    }
}

impl<S: StoreObject> From<StoreTransaction<S>> for TransactionProposal {
    fn from(value: StoreTransaction<S>) -> Self {
        Self {
            transaction_id: value.transaction_id,
            patches: alloc::vec![value.patch.into()],
        }
    }
}

pub struct Operation<I, O> {
    binding: Binding,
    marker: PhantomData<fn(I) -> O>,
}

impl<I, O> Operation<I, O> {
    #[doc(hidden)]
    pub const fn generated(binding: Binding) -> Self {
        Self {
            binding,
            marker: PhantomData,
        }
    }
}

impl<I: AxiomEncode, O> Operation<I, O> {
    pub fn call(&self, input: I) -> EffectCall<O> {
        EffectCall {
            effect: self.binding.effect(input.encode()),
            marker: PhantomData,
        }
    }
}

pub struct EffectCall<O> {
    effect: Effect,
    marker: PhantomData<fn() -> O>,
}

impl<O> EffectCall<O> {
    pub fn into_effect(self) -> Effect {
        self.effect
    }
}

#[derive(Default)]
pub struct Effects {
    effects: Vec<Effect>,
}

impl Effects {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn call<O>(mut self, call: EffectCall<O>) -> Self {
        self.effects.push(call.into_effect());
        self
    }

    fn build(self) -> abi::EffectPlan {
        abi::EffectPlan {
            effects: self.effects,
        }
    }
}

/// A handle-bound ordered decoder. Progress commits only after the whole batch
/// validates, so malformed values cannot consume sequence numbers.
pub struct Stream<T> {
    handle: Handle,
    next_sequence: core::cell::Cell<u64>,
    terminal: core::cell::Cell<bool>,
    max_events: usize,
    max_bytes: usize,
    marker: PhantomData<fn() -> T>,
}

pub struct StreamBatch<T> {
    pub events: Vec<T>,
    pub dropped: u32,
    pub terminal: bool,
}

impl<T> Stream<T> {
    pub fn from_value(value: &Value) -> Result<Self> {
        Self::bounded(value, 256, 65536)
    }

    pub fn bounded(value: &Value, max_events: usize, max_bytes: usize) -> Result<Self> {
        let Value::Handle(handle) = value else {
            return Err(type_error("stream handle", value));
        };
        if handle.kind != HandleKind::Stream || handle.id == 0 || handle.generation == 0 {
            return Err(SdkError::invalid_input("invalid stream handle"));
        }
        if !(1..=256).contains(&max_events) || !(64..=65536).contains(&max_bytes) {
            return Err(SdkError::invalid_input("invalid stream decoder bounds"));
        }
        Ok(Self {
            handle: *handle,
            next_sequence: core::cell::Cell::new(1),
            terminal: core::cell::Cell::new(false),
            max_events,
            max_bytes,
            marker: PhantomData,
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence.get()
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal.get()
    }
}

impl<T: AxiomDecode> Stream<T> {
    /// Event processing requires contiguous delivery and rejects reported loss.
    pub fn decode_batch(&self, batch: &EventBatch) -> Result<Vec<T>> {
        if batch.dropped != 0 {
            return Err(SdkError::conflict("stream reports lost events"));
        }
        self.decode_state_batch(batch).map(|b| b.events)
    }

    /// Explicitly lossy replaceable-state consumers receive loss metadata.
    /// The host's source contract must separately authorize coalescing.
    pub fn decode_state_batch(&self, batch: &EventBatch) -> Result<StreamBatch<T>> {
        if batch.subscription != self.handle {
            return Err(SdkError::denied(
                "event batch belongs to another stream handle or generation",
            ));
        }
        if self.terminal.get() {
            return Err(SdkError::conflict("stream is already terminal"));
        }
        if batch.events.len() > self.max_events {
            return Err(SdkError::invalid_input("stream event bound exceeded"));
        }
        // Preflight before allocating decoded values or a protocol buffer.
        let mut bytes = 32usize;
        let mut nodes = 0usize;
        for event in &batch.events {
            stream_value_bound(&event.value, 0, &mut nodes, &mut bytes, self.max_bytes)?;
            bytes = bytes.saturating_add(16);
            if bytes > self.max_bytes {
                return Err(SdkError::invalid_input("stream byte bound exceeded"));
            }
        }
        let mut next = self.next_sequence.get();
        let mut values = Vec::with_capacity(batch.events.len());
        for event in &batch.events {
            if event.sequence != next {
                return Err(SdkError::conflict(
                    "stream sequence gap, duplicate or reorder",
                ));
            }
            next = next
                .checked_add(1)
                .ok_or_else(|| SdkError::invalid_input("stream sequence exhausted"))?;
            values.push(T::decode(&event.value)?);
        }
        self.next_sequence.set(next);
        self.terminal.set(batch.terminal);
        Ok(StreamBatch {
            events: values,
            dropped: batch.dropped,
            terminal: batch.terminal,
        })
    }
}
fn stream_value_bound(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
    limit: usize,
) -> Result<()> {
    *nodes = nodes.saturating_add(1);
    *bytes = bytes.saturating_add(16);
    if depth > 32 || *nodes > 4096 || *bytes > limit {
        return Err(SdkError::invalid_input(
            "stream value exceeds depth/node/byte bounds",
        ));
    }
    match value {
        Value::String(v) => *bytes = bytes.saturating_add(v.len()),
        Value::Bytes(v) => *bytes = bytes.saturating_add(v.len()),
        Value::List(values) => {
            for v in values {
                stream_value_bound(v, depth + 1, nodes, bytes, limit)?;
            }
        }
        Value::Record(fields) => {
            for f in fields {
                *bytes = bytes.saturating_add(f.name.len());
                stream_value_bound(&f.value, depth + 1, nodes, bytes, limit)?;
            }
        }
        Value::Variant { case, value } => {
            *bytes = bytes.saturating_add(case.len());
            if let Some(v) = value {
                stream_value_bound(v, depth + 1, nodes, bytes, limit)?;
            }
        }
        _ => {}
    }
    if *bytes > limit {
        return Err(SdkError::invalid_input("stream byte bound exceeded"));
    }
    Ok(())
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Result<&'a Value> {
    let mut value = value;
    for segment in path.split('.') {
        let Value::Record(fields) = value else {
            return Err(SdkError::invalid_input(format!(
                "path `{path}` traverses a non-record value"
            )));
        };
        value = fields
            .iter()
            .find(|field| field.name == segment)
            .map(|field| &field.value)
            .ok_or_else(|| SdkError::invalid_input(format!("path `{path}` is missing")))?;
    }
    Ok(value)
}

fn split_path(path: &str) -> Vec<String> {
    path.split('.').map(ToString::to_string).collect()
}
