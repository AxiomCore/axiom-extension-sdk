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
use core::{
    cell::Cell,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

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

    /// An explicitly injected invocation-scoped host capability. This API
    /// never returns material and fails when the binding was not reviewed.
    pub fn secret(&self,alias:&str)->Result<crate::secrets::SecretHandle> {
        let snapshot=self.snapshot(&format!("secret:{alias}")).ok_or_else(||SdkError::denied("secret injection is unavailable"))?;
        crate::secrets::SecretHandle::decode(&snapshot.value)
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
    /// The plan this adapter yielded for `request_id`, when it is known.
    plan: Option<PlanRecord>,
    /// Without a recorded plan, the first ticket read binds this resume to
    /// its plan so tickets from two different plans cannot be mixed.
    bound: Cell<Option<u64>>,
}

impl ResumeContext {
    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Decode the outcome of the host call that issued `ticket`, with the
    /// output type the operation declared.
    ///
    /// A ticket is only valid in the resume of the plan (yield step) that
    /// issued it. When the adapter that yielded the plan also resumes it (the
    /// normal case for `export_extension_factory!`), a ticket from any other
    /// plan or an earlier step fails with `InvalidInput`. When no plan was
    /// recorded (for example an extension instance rebuilt for every message),
    /// the first ticket binds the resume to its plan and a ticket from another
    /// plan fails. The check is local; it costs no host round-trip.
    pub fn get<O: AxiomDecode>(&self, ticket: Ticket<O>) -> Result<O> {
        match self.plan {
            Some(plan) => {
                if ticket.plan != plan.plan {
                    return Err(SdkError::invalid_input(
                        "effect ticket belongs to another effect plan or yield step",
                    ));
                }
                if ticket.index >= plan.len {
                    return Err(SdkError::invalid_input(
                        "effect ticket index is outside its effect plan",
                    ));
                }
            }
            None => match self.bound.get() {
                Some(bound) if bound != ticket.plan => {
                    return Err(SdkError::invalid_input(
                        "effect tickets from different effect plans were read in one resume",
                    ));
                }
                Some(_) => {}
                None => self.bound.set(Some(ticket.plan)),
            },
        }
        self.decode_outcome(ticket.index)
    }

    /// Positional outcome access.
    #[deprecated(
        since = "0.1.5",
        note = "use typed tickets: `let ticket = effects.push(operation.call(input));` then `resume.get(ticket)`"
    )]
    pub fn outcome<T: AxiomDecode>(&self, index: u32) -> Result<T> {
        self.decode_outcome(index)
    }

    fn decode_outcome<T: AxiomDecode>(&self, index: u32) -> Result<T> {
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
    plan: PlanMark,
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
            plan: PlanMark(None),
        }
    }

    /// Suspend this invocation while the host executes the generated,
    /// capability-scoped effect plan. Continue in [`TypedExtension::resume`].
    pub fn yield_for(effects: Effects) -> Self {
        let plan = PlanMark(Some(effects.record()));
        Self {
            output: Value::Null,
            patches: Vec::new(),
            transactions: Vec::new(),
            events: Vec::new(),
            effects: Some(effects.build()),
            group: None,
            plan,
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
            plan: PlanMark(None),
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

pub struct TypedAdapter<T> {
    extension: T,
    /// The plan of the request that is currently yielded, if any. Guest-local
    /// bookkeeping only; nothing here reaches the wire.
    yielded: Option<(RequestId, PlanRecord)>,
}

impl<T> TypedAdapter<T> {
    fn respond(&mut self, request_id: RequestId, result: Result<ExtensionResponse>) -> GuestMessage {
        self.yielded = None;
        match result {
            Ok(response) => {
                if let PlanMark(Some(plan)) = response.plan {
                    self.yielded = Some((request_id, plan));
                }
                response.into_message(request_id)
            }
            Err(error) => failed(request_id, error),
        }
    }
}

/// Erase a typed implementation behind the ABI-compatible extension trait
/// object expected by the generated source wrapper.
pub fn boxed_extension<T: TypedExtension + 'static>(
    extension: T,
) -> alloc::boxed::Box<dyn Extension> {
    alloc::boxed::Box::new(TypedAdapter {
        extension,
        yielded: None,
    })
}

impl<T: TypedExtension> Extension for TypedAdapter<T> {
    fn initialize(&mut self) -> core::result::Result<(), ExtensionError> {
        TypedExtension::initialize(&mut self.extension).map_err(Into::into)
    }

    fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage {
        let result =
            TypedExtension::invoke(&mut self.extension, TypedInvocation::new(request_id, invocation));
        self.respond(request_id, result)
    }

    fn resume(
        &mut self,
        request_id: RequestId,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    ) -> GuestMessage {
        let plan = match self.yielded.take() {
            Some((yielded, plan)) if yielded == request_id => Some(plan),
            _ => None,
        };
        let result = TypedExtension::resume(
            &mut self.extension,
            ResumeContext {
                request_id,
                outcomes,
                events,
                plan,
                bound: Cell::new(None),
            },
        );
        self.respond(request_id, result)
    }

    fn cancel(&mut self, request_id: RequestId) -> core::result::Result<(), ExtensionError> {
        self.yielded = None;
        TypedExtension::cancel(&mut self.extension, request_id).map_err(Into::into)
    }

    fn shutdown(&mut self) {
        self.yielded = None;
        TypedExtension::shutdown(&mut self.extension)
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

/// Guest-local plan identities. A plain counter: plans are only compared for
/// equality inside one extension instance, and nothing is sent to the host.
static NEXT_PLAN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlanRecord {
    plan: u64,
    len: u32,
}

/// Guest-local plan identity carried beside a response. It never affects
/// equality or the encoded message: two responses with the same plan bytes
/// compare equal.
#[derive(Clone, Copy, Debug)]
struct PlanMark(Option<PlanRecord>);

impl PartialEq for PlanMark {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// A typed claim on the outcome of one host call in one effect plan.
///
/// Returned by [`Effects::push`]; read it in [`TypedExtension::resume`] with
/// [`ResumeContext::get`], which decodes the outcome as `O`. A ticket is
/// `Copy` so it can be kept in the extension between invoke and resume. It
/// cannot be built from a raw index:
///
/// ```compile_fail,E0451
/// let forged: axiom_extension_sdk::Ticket<u64> = axiom_extension_sdk::Ticket {
///     plan: 1,
///     index: 0,
///     marker: core::marker::PhantomData,
/// };
/// ```
///
/// and the decoded type is the one the operation declared:
///
/// ```compile_fail,E0308
/// use axiom_extension_sdk::{Binding, Effects, Operation, ResumeContext};
/// fn read(resume: &ResumeContext) {
///     let mut effects = Effects::new();
///     let ticket = effects.push(
///         Operation::<u64, u64>::generated(Binding {
///             namespace: "n", operation: "o", input: "Unsigned", output: "Unsigned",
///         })
///         .call(1),
///     );
///     let wrong: String = resume.get(ticket).unwrap();
/// }
/// ```
pub struct Ticket<O> {
    plan: u64,
    index: u32,
    marker: PhantomData<fn() -> O>,
}

impl<O> Ticket<O> {
    /// The ABI outcome index (declaration order in the plan).
    pub fn index(&self) -> u32 {
        self.index
    }
}

impl<O> Clone for Ticket<O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<O> Copy for Ticket<O> {}

impl<O> PartialEq for Ticket<O> {
    fn eq(&self, other: &Self) -> bool {
        self.plan == other.plan && self.index == other.index
    }
}

impl<O> Eq for Ticket<O> {}

impl<O> core::fmt::Debug for Ticket<O> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Ticket")
            .field("plan", &self.plan)
            .field("index", &self.index)
            .finish()
    }
}

pub struct Effects {
    effects: Vec<Effect>,
    plan: u64,
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

impl Effects {
    pub fn new() -> Self {
        Self {
            effects: Vec::new(),
            plan: NEXT_PLAN.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Append a call, chaining by value. The outcome can only be read
    /// positionally; prefer [`Effects::push`], which returns a typed ticket.
    pub fn call<O>(mut self, call: EffectCall<O>) -> Self {
        self.effects.push(call.into_effect());
        self
    }

    /// Append a call and return the typed ticket for its outcome. The plan
    /// bytes are identical to the ones [`Effects::call`] produces.
    pub fn push<O>(&mut self, call: EffectCall<O>) -> Ticket<O> {
        let index = u32::try_from(self.effects.len()).unwrap_or(u32::MAX);
        self.effects.push(call.into_effect());
        Ticket {
            plan: self.plan,
            index,
            marker: PhantomData,
        }
    }

    /// Number of host calls in the plan.
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    fn record(&self) -> PlanRecord {
        PlanRecord {
            plan: self.plan,
            len: u32::try_from(self.effects.len()).unwrap_or(u32::MAX),
        }
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

#[cfg(test)]
mod ticket_tests {
    use super::*;
    use alloc::{boxed::Box, vec};
    use abi::{CodecLimits, Frame};

    const LOOKUP: Binding = Binding {
        namespace: "contract:catalog",
        operation: "query_product",
        input: "Unsigned",
        output: "String",
    };
    const STOCK: Binding = Binding {
        namespace: "contract:catalog",
        operation: "in_stock",
        input: "Unsigned",
        output: "Boolean",
    };

    fn lookup() -> Operation<u64, String> {
        Operation::generated(LOOKUP)
    }
    fn stock() -> Operation<u64, bool> {
        Operation::generated(STOCK)
    }
    fn id(value: u64) -> RequestId {
        RequestId::new(value).unwrap()
    }
    fn invocation(export: &str) -> Invocation {
        Invocation {
            export: export.into(),
            input: Value::Null,
            snapshots: vec![],
            deadline_unix_ms: 1,
        }
    }
    fn ok(index: u32, value: Value) -> EffectOutcome {
        EffectOutcome {
            index,
            result: Ok(value),
        }
    }
    fn output(message: GuestMessage) -> Value {
        match message {
            GuestMessage::Completed { result, .. } => result.output,
            other => panic!("expected completion, got {other:?}"),
        }
    }
    fn failure(message: GuestMessage) -> ExtensionError {
        match message {
            GuestMessage::Failed { error, .. } => error,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    /// Yields two differently typed calls and reads both by ticket.
    #[derive(Default)]
    struct TwoCalls {
        tickets: Option<(Ticket<String>, Ticket<bool>)>,
    }
    impl TypedExtension for TwoCalls {
        fn invoke(&mut self, _: TypedInvocation) -> Result<ExtensionResponse> {
            let mut effects = Effects::new();
            let name = effects.push(lookup().call(7));
            let available = effects.push(stock().call(7));
            self.tickets = Some((name, available));
            Ok(ExtensionResponse::yield_for(effects))
        }
        fn resume(&mut self, resume: ResumeContext) -> Result<ExtensionResponse> {
            let (name, available) = self.tickets.take().unwrap();
            let name: String = resume.get(name)?;
            let available: bool = resume.get(available)?;
            Ok(ExtensionResponse::new(format!("{name}:{available}")))
        }
    }

    #[test]
    fn tickets_round_trip_typed_outcomes() {
        let mut extension = boxed_extension(TwoCalls::default());
        let GuestMessage::Yielded { plan, .. } = extension.invoke(id(1), invocation("load"))
        else {
            panic!("expected a yield");
        };
        assert_eq!(plan.effects.len(), 2);
        let resumed = extension.resume(
            id(1),
            vec![ok(0, Value::String("Linen".into())), ok(1, Value::Bool(true))],
            vec![],
        );
        assert_eq!(output(resumed), Value::String("Linen:true".into()));
    }

    #[test]
    fn ticket_reports_type_mismatch_and_host_errors() {
        let mut extension = boxed_extension(TwoCalls::default());
        extension.invoke(id(1), invocation("load"));
        // The host returns the right index with the wrong type: decoding fails
        // instead of silently reading another outcome.
        let error = failure(extension.resume(
            id(1),
            vec![ok(0, Value::Bool(true)), ok(1, Value::Bool(true))],
            vec![],
        ));
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("expected string"), "{}", error.message);

        extension.invoke(id(2), invocation("load"));
        let error = failure(extension.resume(
            id(2),
            vec![
                EffectOutcome {
                    index: 0,
                    result: Err(ExtensionError {
                        code: ErrorCode::Denied,
                        message: "denied by host".into(),
                        retryable: false,
                    }),
                },
                ok(1, Value::Bool(true)),
            ],
            vec![],
        ));
        assert_eq!(error.code, ErrorCode::Denied);
    }

    /// Keeps the ticket of the first invocation and reads it in a later one.
    #[derive(Default)]
    struct StaleTicket {
        first: Option<Ticket<String>>,
        unyielded: Option<Ticket<String>>,
        current: Option<Ticket<String>>,
    }
    impl TypedExtension for StaleTicket {
        fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
            let mut effects = Effects::new();
            let ticket = effects.push(lookup().call(1));
            if self.first.is_none() {
                self.first = Some(ticket);
            }
            self.current = Some(ticket);
            if invocation.export() == "unyielded" {
                // A plan that is built but never yielded issues tickets that
                // are valid for no resume.
                let mut other = Effects::new();
                self.unyielded = Some(other.push(lookup().call(2)));
            }
            Ok(ExtensionResponse::yield_for(effects))
        }
        fn resume(&mut self, resume: ResumeContext) -> Result<ExtensionResponse> {
            let ticket = match self.unyielded.take() {
                Some(ticket) => ticket,
                None => self.first.unwrap(),
            };
            Ok(ExtensionResponse::new(resume.get(ticket)?))
        }
    }

    #[test]
    fn ticket_from_another_plan_is_rejected() {
        let mut extension = boxed_extension(StaleTicket::default());
        extension.invoke(id(1), invocation("load"));
        assert_eq!(
            output(extension.resume(id(1), vec![ok(0, Value::String("a".into()))], vec![])),
            Value::String("a".into())
        );
        // Second request: same shape, same index, different plan.
        extension.invoke(id(2), invocation("load"));
        let error = failure(extension.resume(id(2), vec![ok(0, Value::String("b".into()))], vec![]));
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("another effect plan"), "{}", error.message);

        // A ticket from a plan that was never yielded.
        let mut extension = boxed_extension(StaleTicket::default());
        extension.invoke(id(3), invocation("unyielded"));
        let error = failure(extension.resume(id(3), vec![ok(0, Value::String("c".into()))], vec![]));
        assert!(error.message.contains("another effect plan"), "{}", error.message);
    }

    /// Yields twice in one request and reads the first step's ticket at the
    /// second resume.
    #[derive(Default)]
    struct TwoSteps {
        step_one: Option<Ticket<String>>,
        step_two: Option<Ticket<String>>,
        stale: bool,
    }
    impl TypedExtension for TwoSteps {
        fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
            self.stale = invocation.export() == "stale";
            let mut effects = Effects::new();
            self.step_one = Some(effects.push(lookup().call(1)));
            Ok(ExtensionResponse::yield_for(effects))
        }
        fn resume(&mut self, resume: ResumeContext) -> Result<ExtensionResponse> {
            match self.step_two {
                None => {
                    let first: String = resume.get(self.step_one.unwrap())?;
                    let mut effects = Effects::new();
                    self.step_two = Some(effects.push(lookup().call(first.len() as u64)));
                    Ok(ExtensionResponse::yield_for(effects))
                }
                Some(second) => {
                    let ticket = if self.stale { self.step_one.unwrap() } else { second };
                    Ok(ExtensionResponse::new(resume.get(ticket)?))
                }
            }
        }
    }

    #[test]
    fn ticket_from_an_earlier_yield_step_is_rejected() {
        for (export, expect_ok) in [("fresh", true), ("stale", false)] {
            let mut extension = boxed_extension(TwoSteps::default());
            extension.invoke(id(1), invocation(export));
            let step = extension.resume(id(1), vec![ok(0, Value::String("abc".into()))], vec![]);
            assert!(matches!(step, GuestMessage::Yielded { .. }), "{step:?}");
            let last = extension.resume(id(1), vec![ok(0, Value::String("second".into()))], vec![]);
            if expect_ok {
                assert_eq!(output(last), Value::String("second".into()));
            } else {
                assert!(failure(last).message.contains("another effect plan"));
            }
        }
    }

    #[test]
    fn cancel_and_failures_clear_the_recorded_plan() {
        let mut extension = boxed_extension(TwoCalls::default());
        extension.invoke(id(1), invocation("load"));
        extension.cancel(id(1)).unwrap();
        // After a cancel the adapter keeps no plan record (the host never
        // resumes a cancelled request; this checks the SDK does not reuse a
        // stale record). Both tickets come from one plan, so the unrecorded
        // binding mode accepts them.
        let adapter_resume = extension.resume(
            id(1),
            vec![ok(0, Value::String("x".into())), ok(1, Value::Bool(false))],
            vec![],
        );
        assert_eq!(output(adapter_resume), Value::String("x:false".into()));
    }

    fn unrecorded(outcomes: Vec<EffectOutcome>) -> ResumeContext {
        ResumeContext {
            request_id: id(5),
            outcomes,
            events: vec![],
            plan: None,
            bound: Cell::new(None),
        }
    }

    #[test]
    fn without_a_recorded_plan_tickets_from_two_plans_cannot_be_mixed() {
        let mut first = Effects::new();
        let a = first.push(lookup().call(1));
        let mut second = Effects::new();
        let _ = second.push(lookup().call(1));
        let b = second.push(stock().call(1));
        let resume = unrecorded(vec![ok(0, Value::String("a".into())), ok(1, Value::Bool(true))]);
        assert_eq!(resume.get(a).unwrap(), "a");
        let error = resume.get(b).unwrap_err();
        assert!(error.message.contains("different effect plans"), "{}", error.message);
        // Re-reading a ticket of the bound plan stays valid.
        assert_eq!(resume.get(a).unwrap(), "a");
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_positional_outcome_still_works() {
        let resume = unrecorded(vec![ok(0, Value::String("a".into())), ok(1, Value::Bool(true))]);
        assert_eq!(resume.outcome::<String>(0).unwrap(), "a");
        assert!(resume.outcome::<bool>(1).unwrap());
        assert!(resume.outcome::<bool>(2).is_err());
        // Positional reads do not bind the resume to a plan.
        let mut effects = Effects::new();
        let ticket = effects.push(lookup().call(1));
        assert_eq!(resume.get(ticket).unwrap(), "a");
    }

    #[test]
    fn tickets_are_copy_and_carry_declaration_order() {
        let mut effects = Effects::new();
        assert!(effects.is_empty());
        let a = effects.push(lookup().call(1));
        let b = effects.push(stock().call(2));
        let copy = a;
        assert_eq!(copy, a);
        assert_eq!((a.index(), b.index()), (0, 1));
        assert_eq!(effects.len(), 2);
        let mut other = Effects::new();
        assert_ne!(other.push(lookup().call(1)), a);
    }

    #[test]
    fn plan_bytes_are_identical_with_and_without_tickets() {
        let limits = CodecLimits { max_bytes: 65_536 };
        let request_id = id(3);
        let chained = ExtensionResponse::yield_for(
            Effects::new().call(lookup().call(7)).call(stock().call(9)),
        );
        let mut effects = Effects::new();
        let _name = effects.push(lookup().call(7));
        let _stock = effects.push(stock().call(9));
        let ticketed = ExtensionResponse::yield_for(effects);
        assert_eq!(chained, ticketed);
        let chained = abi::encode(&Frame::new(chained.into_message(request_id)), limits).unwrap();
        let ticketed = abi::encode(&Frame::new(ticketed.into_message(request_id)), limits).unwrap();
        assert!(!chained.is_empty());
        assert_eq!(chained, ticketed);

        // The same holds for the full adapter output on the wire.
        struct Chained;
        impl TypedExtension for Chained {
            fn invoke(&mut self, _: TypedInvocation) -> Result<ExtensionResponse> {
                Ok(ExtensionResponse::yield_for(
                    Effects::new().call(lookup().call(7)).call(stock().call(7)),
                ))
            }
        }
        let via_adapter = |mut extension: Box<dyn Extension>| {
            abi::encode(&Frame::new(extension.invoke(request_id, invocation("load"))), limits)
                .unwrap()
        };
        assert_eq!(
            via_adapter(boxed_extension(Chained)),
            via_adapter(boxed_extension(TwoCalls::default()))
        );
    }
}
