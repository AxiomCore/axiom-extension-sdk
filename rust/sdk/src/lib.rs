#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;
extern crate self as axiom_extension_sdk;

mod primitives;
#[cfg(feature = "std")]
pub mod test;
mod typed;

use abi::{
    CodecLimits, Effect, EffectOutcome, EventBatch, ExtensionError, Field, Frame, GuestMessage,
    HostMessage, Invocation, InvocationResult, Patch, PatchOperation, RequestId,
    TransactionProposal, Value,
};
use alloc::{boxed::Box, collections::BTreeMap, string::String, vec, vec::Vec};
pub use axiom_extension_abi as abi;
pub use axiom_extension_sdk_derive::{export, extension, AxiomType};
pub use primitives::{Cents, DurationMs, Id, Percentage, Revision, TimestampMs};
pub use typed::{
    boxed_extension, AxiomDecode, AxiomEncode, AxiomType, EffectCall, Effects, ExtensionResponse,
    Operation, RecordDecoder, Result, ResumeContext, SdkError, Store, StoreField, StoreObject,
    StorePatch, StoreTransaction, Stream, TypedExtension, TypedInvocation, UiField, UiPatch,
    UiScope, UiState,
};

/// Ordinary extension authors should import this module. It contains the
/// typed, deterministic SDK surface and intentionally omits transport framing.
pub mod prelude {
    pub use crate::{
        boxed_extension, export, extension, AxiomDecode, AxiomEncode, AxiomType, Cents, DurationMs,
        EffectCall, Effects, ExtensionResponse, Id, Operation, Percentage, Result, ResumeContext,
        Revision, SdkError, Store, StoreField, StoreObject, StorePatch, StoreTransaction, Stream,
        TimestampMs, TypedExtension, TypedInvocation, UiField, UiPatch, UiScope, UiState,
    };
}

/// Advanced ABI-level compatibility surface. New extension code should use
/// [`prelude`] and generated scoped bindings instead.
pub mod raw {
    pub use crate::abi::*;
    pub use crate::{
        completed, completed_with_changes, Binding, EffectPlanBuilder, Extension, GuestExports,
        GuestMachine, StoreCursor, StoreSelector, StoreTransactionBuilder, UiPatchBuilder,
        UiSelector,
    };
}

#[doc(hidden)]
pub mod __private {
    pub use crate::typed::{canonical_record, RecordDecoder};
    pub use alloc::boxed::Box;

    pub fn unknown_export(export: &str, expected: &[&str]) -> alloc::string::String {
        alloc::format!(
            "unknown export `{export}`; expected one of: {}",
            expected.join(", ")
        )
    }
}

/// Implemented by a Rust extension. The host owns all state and resources;
/// guest code can only return typed results or a bounded effect plan.
pub trait Extension {
    fn initialize(&mut self) -> core::result::Result<(), ExtensionError> {
        Ok(())
    }
    fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage;
    fn resume(
        &mut self,
        request_id: RequestId,
        _outcomes: Vec<EffectOutcome>,
        _events: Vec<EventBatch>,
    ) -> GuestMessage {
        GuestMessage::Failed {
            request_id,
            error: ExtensionError {
                code: abi::ErrorCode::InvalidInput,
                message: "extension did not declare resumable effects or streams".into(),
                retryable: false,
            },
        }
    }
    fn cancel(&mut self, _request_id: RequestId) -> core::result::Result<(), ExtensionError> {
        Ok(())
    }
    fn shutdown(&mut self) {}
}

// Generated source wrappers construct a trait object so authored modules do
// not need to know the concrete type expected by the core-WASM export glue.
// The blanket forwarding implementation keeps the object at the guest side of
// the ABI; it never exposes a host pointer or mutable application state.
impl<T: Extension + ?Sized> Extension for Box<T> {
    fn initialize(&mut self) -> core::result::Result<(), ExtensionError> {
        (**self).initialize()
    }

    fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage {
        (**self).invoke(request_id, invocation)
    }

    fn resume(
        &mut self,
        request_id: RequestId,
        outcomes: Vec<EffectOutcome>,
        events: Vec<EventBatch>,
    ) -> GuestMessage {
        (**self).resume(request_id, outcomes, events)
    }

    fn cancel(&mut self, request_id: RequestId) -> core::result::Result<(), ExtensionError> {
        (**self).cancel(request_id)
    }

    fn shutdown(&mut self) {
        (**self).shutdown()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MachineState {
    New,
    Negotiated,
    Ready,
    Yielded(RequestId),
    Draining,
    Stopped,
}

/// Deterministic guest harness shared by generated exports and unit tests.
/// It performs negotiation and lifecycle framing before guest code is called.
pub struct GuestMachine<E> {
    extension: E,
    state: MachineState,
    limits: CodecLimits,
}

impl<E: Extension> GuestMachine<E> {
    pub fn new(extension: E, limits: CodecLimits) -> Self {
        Self {
            extension,
            state: MachineState::New,
            limits,
        }
    }

    pub fn dispatch(&mut self, bytes: &[u8]) -> core::result::Result<Vec<u8>, abi::ProtocolError> {
        let frame = abi::decode_frame::<HostMessage>(bytes, self.limits)?;
        let response = match (self.state, frame.message) {
            (MachineState::New, HostMessage::Negotiate { supported }) => {
                if !supported.contains(&abi::ABI_VERSION) {
                    return Err(abi::ProtocolError::AbiMismatch);
                }
                self.state = MachineState::Negotiated;
                GuestMessage::Negotiated {
                    selected: abi::ABI_VERSION,
                }
            }
            (MachineState::Negotiated, HostMessage::Initialize { .. }) => {
                self.extension
                    .initialize()
                    .map_err(|_| abi::ProtocolError::InvalidTransition)?;
                self.state = MachineState::Ready;
                GuestMessage::Initialized
            }
            (
                MachineState::Ready,
                HostMessage::Invoke {
                    request_id,
                    invocation,
                },
            ) => {
                let response = self.extension.invoke(request_id, invocation);
                self.accept_outcome(request_id, response)?
            }
            (
                MachineState::Yielded(active),
                HostMessage::Resume {
                    request_id,
                    outcomes,
                    events,
                },
            ) if active == request_id => {
                let response = self.extension.resume(request_id, outcomes, events);
                self.accept_outcome(request_id, response)?
            }
            (MachineState::Yielded(active), HostMessage::Cancel { request_id })
                if active == request_id =>
            {
                match self.extension.cancel(request_id) {
                    Ok(()) => {
                        self.state = MachineState::Ready;
                        GuestMessage::Cancelled { request_id }
                    }
                    Err(error) => {
                        self.state = MachineState::Ready;
                        GuestMessage::Failed { request_id, error }
                    }
                }
            }
            (MachineState::Ready, HostMessage::Drain) => {
                self.state = MachineState::Draining;
                GuestMessage::Drained
            }
            (MachineState::Ready | MachineState::Draining, HostMessage::Shutdown) => {
                self.extension.shutdown();
                self.state = MachineState::Stopped;
                GuestMessage::Shutdown
            }
            (MachineState::Stopped, _) => return Err(abi::ProtocolError::UseAfterShutdown),
            _ => return Err(abi::ProtocolError::InvalidTransition),
        };
        abi::encode(&Frame::new(response), self.limits)
    }

    fn accept_outcome(
        &mut self,
        request_id: RequestId,
        response: GuestMessage,
    ) -> core::result::Result<GuestMessage, abi::ProtocolError> {
        match &response {
            GuestMessage::Yielded {
                request_id: actual, ..
            } if *actual == request_id => {
                self.state = MachineState::Yielded(request_id);
            }
            GuestMessage::Completed {
                request_id: actual, ..
            }
            | GuestMessage::Failed {
                request_id: actual, ..
            }
            | GuestMessage::Cancelled { request_id: actual }
                if *actual == request_id =>
            {
                self.state = MachineState::Ready;
            }
            _ => return Err(abi::ProtocolError::InvalidTransition),
        }
        Ok(response)
    }

    pub fn into_inner(self) -> E {
        self.extension
    }
}

/// Helper for the common pure-compute terminal result.
pub fn completed(request_id: RequestId, output: abi::Value) -> GuestMessage {
    GuestMessage::Completed {
        request_id,
        result: InvocationResult {
            output,
            patches: Vec::new(),
            transactions: Vec::new(),
            emitted_events: Vec::new(),
        },
    }
}

/// A generated, exact UI-state binding. The type parameter is a marker emitted
/// by contract code generation; values still use the ABI representation at the
/// wire boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiSelector<T> {
    pub namespace: String,
    pub operation: String,
    pub scope: String,
    pub path: String,
    marker: core::marker::PhantomData<T>,
}

impl<T> UiSelector<T> {
    pub fn generated(namespace: &str, operation: &str, scope: &str, path: &str) -> Self {
        Self {
            namespace: namespace.into(),
            operation: operation.into(),
            scope: scope.into(),
            path: path.into(),
            marker: core::marker::PhantomData,
        }
    }

    pub fn read(&self) -> Effect {
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: Value::Null,
        }
    }

    pub fn write(&self, expected_revision: u64, value: Value) -> Effect {
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: record(vec![
                ("expectedRevision", Value::Unsigned(expected_revision)),
                ("value", value),
            ]),
        }
    }

    pub fn subscribe(&self) -> Effect {
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: record(vec![("action", Value::String("open".into()))]),
        }
    }
}

/// Builds one revision-checked UI patch. Generated selectors prevent callers
/// from constructing dynamic paths in normal SDK use.
pub struct UiPatchBuilder {
    scope: String,
    expected_revision: u64,
    operations: Vec<PatchOperation>,
}

impl UiPatchBuilder {
    pub fn new(scope: &str, expected_revision: u64) -> Self {
        Self {
            scope: scope.into(),
            expected_revision,
            operations: Vec::new(),
        }
    }

    pub fn set<T>(mut self, selector: &UiSelector<T>, value: Value) -> Self {
        self.assert_scope(selector);
        self.operations.push(PatchOperation::Set {
            path: split_path(&selector.path),
            value,
        });
        self
    }

    pub fn compare_and_set<T>(
        mut self,
        selector: &UiSelector<T>,
        expected: Value,
        replacement: Value,
    ) -> Self {
        self.assert_scope(selector);
        self.operations.push(PatchOperation::CompareAndSet {
            path: split_path(&selector.path),
            expected,
            replacement,
        });
        self
    }

    pub fn append<T>(mut self, selector: &UiSelector<T>, value: Value) -> Self {
        self.assert_scope(selector);
        self.operations.push(PatchOperation::Append {
            path: split_path(&selector.path),
            value,
        });
        self
    }

    pub fn unset<T>(mut self, selector: &UiSelector<T>) -> Self {
        self.assert_scope(selector);
        self.operations.push(PatchOperation::Unset {
            path: split_path(&selector.path),
        });
        self
    }

    pub fn dispatch(mut self, action: &str, payload: Value) -> Self {
        self.operations.push(PatchOperation::Dispatch {
            action: action.into(),
            payload,
        });
        self
    }

    pub fn build(self) -> Patch {
        Patch {
            resource: alloc::format!("ui:{}", self.scope),
            expected_revision: self.expected_revision,
            operations: self.operations,
        }
    }

    fn assert_scope<T>(&self, selector: &UiSelector<T>) {
        assert_eq!(
            self.scope, selector.scope,
            "selector belongs to another UI scope"
        );
    }
}

/// A generated field binding for one durable store object type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSelector<T> {
    pub namespace: String,
    pub operation: String,
    pub object: String,
    pub field: String,
    marker: core::marker::PhantomData<T>,
}

impl<T> StoreSelector<T> {
    pub fn generated(namespace: &str, operation: &str, object: &str, field: &str) -> Self {
        Self {
            namespace: namespace.into(),
            operation: operation.into(),
            object: object.into(),
            field: field.into(),
            marker: core::marker::PhantomData,
        }
    }

    pub fn read(&self, id: &str) -> Effect {
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: record(vec![("id", Value::String(id.into()))]),
        }
    }

    pub fn write(&self, id: &str, expected_version: u64, value: Value) -> Effect {
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: record(vec![
                ("expectedVersion", Value::Unsigned(expected_version)),
                ("id", Value::String(id.into())),
                ("value", value),
            ]),
        }
    }
}

/// Generated bounded cursor binding. Pages are host-owned selections and the
/// continuation token is an object ID, never a database/ORM handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreCursor {
    pub namespace: String,
    pub operation: String,
}

impl StoreCursor {
    pub fn generated(namespace: &str, operation: &str) -> Self {
        Self {
            namespace: namespace.into(),
            operation: operation.into(),
        }
    }

    pub fn page(&self, after: Option<&str>, limit: u64) -> Effect {
        let mut fields = vec![("limit", Value::Unsigned(limit))];
        if let Some(after) = after {
            fields.push(("after", Value::String(after.into())));
        }
        Effect {
            namespace: self.namespace.clone(),
            operation: self.operation.clone(),
            input: record(fields),
        }
    }
}

/// Builds a durable multi-object transaction proposal. The host validates all
/// revisions and swaps every object atomically.
pub struct StoreTransactionBuilder {
    transaction_id: u64,
    patches: Vec<Patch>,
}

impl StoreTransactionBuilder {
    pub fn new(transaction_id: u64) -> Self {
        assert_ne!(transaction_id, 0, "transaction ID must be non-zero");
        Self {
            transaction_id,
            patches: Vec::new(),
        }
    }

    pub fn set<T>(
        mut self,
        selector: &StoreSelector<T>,
        id: &str,
        expected_version: u64,
        value: Value,
    ) -> Self {
        self.push_operation(
            &selector.object,
            id,
            expected_version,
            PatchOperation::Set {
                path: split_path(&selector.field),
                value,
            },
        );
        self
    }

    pub fn append<T>(
        mut self,
        selector: &StoreSelector<T>,
        id: &str,
        expected_version: u64,
        value: Value,
    ) -> Self {
        self.push_operation(
            &selector.object,
            id,
            expected_version,
            PatchOperation::Append {
                path: split_path(&selector.field),
                value,
            },
        );
        self
    }

    pub fn unset<T>(
        mut self,
        selector: &StoreSelector<T>,
        id: &str,
        expected_version: u64,
    ) -> Self {
        self.push_operation(
            &selector.object,
            id,
            expected_version,
            PatchOperation::Unset {
                path: split_path(&selector.field),
            },
        );
        self
    }

    pub fn build(self) -> TransactionProposal {
        TransactionProposal {
            transaction_id: self.transaction_id,
            patches: self.patches,
        }
    }

    fn push_operation(
        &mut self,
        object: &str,
        id: &str,
        expected_revision: u64,
        operation: PatchOperation,
    ) {
        let resource = alloc::format!("store:{object}:{id}");
        if let Some(patch) = self
            .patches
            .iter_mut()
            .find(|patch| patch.resource == resource)
        {
            assert_eq!(
                patch.expected_revision, expected_revision,
                "one object transaction must use one expected revision"
            );
            patch.operations.push(operation);
        } else {
            self.patches.push(Patch {
                resource,
                expected_revision,
                operations: vec![operation],
            });
        }
    }
}

/// Finish an invocation with host-validated state/store proposals.
pub fn completed_with_changes(
    request_id: RequestId,
    output: Value,
    patches: Vec<Patch>,
    transactions: Vec<TransactionProposal>,
) -> GuestMessage {
    GuestMessage::Completed {
        request_id,
        result: InvocationResult {
            output,
            patches,
            transactions,
            emitted_events: Vec::new(),
        },
    }
}

fn split_path(path: &str) -> Vec<String> {
    path.split('.').map(String::from).collect()
}

fn record(fields: Vec<(&str, Value)>) -> Value {
    let mut fields: Vec<Field> = fields
        .into_iter()
        .map(|(name, value)| Field {
            name: name.into(),
            value,
        })
        .collect();
    fields.sort_by(|left, right| left.name.cmp(&right.name));
    Value::Record(fields)
}

/// Safe owner for the core-WASM transport buffers used by generated exports.
/// Pointers are opaque allocation IDs from the host's perspective; all actual
/// ownership remains in this table until dispatch or deallocation consumes it.
pub struct GuestExports<E> {
    machine: GuestMachine<E>,
    allocations: BTreeMap<u32, Box<[u8]>>,
    max_bytes: usize,
}

impl<E: Extension> GuestExports<E> {
    pub fn new(extension: E, limits: CodecLimits) -> Self {
        Self {
            machine: GuestMachine::new(extension, limits),
            allocations: BTreeMap::new(),
            max_bytes: limits.max_bytes,
        }
    }

    pub fn allocate(&mut self, length: u32) -> u32 {
        let length = length as usize;
        if length == 0 || length > self.max_bytes {
            return 0;
        }
        let mut bytes = vec![0; length].into_boxed_slice();
        let pointer = bytes.as_mut_ptr() as usize;
        let Ok(pointer) = u32::try_from(pointer) else {
            return 0;
        };
        if pointer == 0 || self.allocations.insert(pointer, bytes).is_some() {
            return 0;
        }
        pointer
    }

    /// Consume one host-written input allocation and return a packed
    /// `(response_length << 32) | response_pointer`. Zero is a terminal
    /// transport failure; protocol failures never expose partial output.
    pub fn dispatch(&mut self, pointer: u32, length: u32) -> u64 {
        let Some(input) = self.allocations.remove(&pointer) else {
            return 0;
        };
        if input.len() != length as usize {
            return 0;
        }
        let Ok(response) = self.machine.dispatch(&input) else {
            return 0;
        };
        if response.is_empty() || response.len() > self.max_bytes {
            return 0;
        }
        let mut response = response.into_boxed_slice();
        let response_length = response.len() as u32;
        let pointer = response.as_mut_ptr() as usize;
        let Ok(pointer) = u32::try_from(pointer) else {
            return 0;
        };
        if pointer == 0 || self.allocations.insert(pointer, response).is_some() {
            return 0;
        }
        (u64::from(response_length) << 32) | u64::from(pointer)
    }

    pub fn deallocate(&mut self, pointer: u32, length: u32) -> bool {
        match self.allocations.remove(&pointer) {
            Some(bytes) if bytes.len() == length as usize => true,
            Some(bytes) => {
                self.allocations.insert(pointer, bytes);
                false
            }
            None => false,
        }
    }

    pub fn live_allocations(&self) -> usize {
        self.allocations.len()
    }
}

/// Generate the only core-WASM exports consumed by the Phase 5 sandbox. The
/// macro is intentionally available only on wasm32: native callers use
/// `GuestMachine` directly and never exchange process pointers.
#[macro_export]
macro_rules! export_extension {
    ($extension_type:ty, $constructor:expr, max_bytes = $max_bytes:expr) => {
        #[cfg(target_arch = "wasm32")]
        std::thread_local! {
            static AXIOM_EXTENSION_EXPORTS: std::cell::RefCell<$crate::GuestExports<$extension_type>> =
                std::cell::RefCell::new($crate::GuestExports::new(
                    $constructor,
                    $crate::abi::CodecLimits { max_bytes: $max_bytes },
                ));
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_abi_version() -> i32 {
            (($crate::abi::ABI_VERSION.major as i32) << 16)
                | $crate::abi::ABI_VERSION.minor as i32
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_alloc(length: i32) -> i32 {
            if length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                runtime.borrow_mut().allocate(length as u32) as i32
            })
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_dispatch(pointer: i32, length: i32) -> i64 {
            if pointer <= 0 || length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                runtime
                    .borrow_mut()
                    .dispatch(pointer as u32, length as u32) as i64
            })
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_dealloc(pointer: i32, length: i32) -> i32 {
            if pointer <= 0 || length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                i32::from(
                    runtime
                        .borrow_mut()
                        .deallocate(pointer as u32, length as u32),
                )
            })
        }
    };
}

/// Export a source-authored extension factory through the Axiom core-WASM
/// ABI. Axiom's generated crate owns this macro invocation; an authored
/// `AxiomDeps.toml` Rust entry only provides `fn axiom_extension() ->
/// Box<dyn axiom_extension_sdk::Extension>`. This keeps packaging and all
/// host-facing symbols out of application source.
#[macro_export]
macro_rules! export_extension_factory {
    ($factory:expr, max_bytes = $max_bytes:expr) => {
        #[cfg(target_arch = "wasm32")]
        std::thread_local! {
            static AXIOM_EXTENSION_EXPORTS: std::cell::RefCell<$crate::GuestExports<Box<dyn $crate::Extension>>> =
                std::cell::RefCell::new($crate::GuestExports::new(
                    $factory(),
                    $crate::abi::CodecLimits { max_bytes: $max_bytes },
                ));
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_abi_version() -> i32 {
            (($crate::abi::ABI_VERSION.major as i32) << 16)
                | $crate::abi::ABI_VERSION.minor as i32
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_alloc(length: i32) -> i32 {
            if length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                runtime.borrow_mut().allocate(length as u32) as i32
            })
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_dispatch(pointer: i32, length: i32) -> i64 {
            if pointer <= 0 || length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                runtime
                    .borrow_mut()
                    .dispatch(pointer as u32, length as u32) as i64
            })
        }

        #[cfg(target_arch = "wasm32")]
        #[unsafe(no_mangle)]
        pub extern "C" fn axiom_extension_dealloc(pointer: i32, length: i32) -> i32 {
            if pointer <= 0 || length <= 0 {
                return 0;
            }
            AXIOM_EXTENSION_EXPORTS.with(|runtime| {
                i32::from(
                    runtime
                        .borrow_mut()
                        .deallocate(pointer as u32, length as u32),
                )
            })
        }
    };
}

/// Generated binding descriptors are plain constants so interface review does
/// not depend on executing build scripts or guest code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub namespace: &'static str,
    pub operation: &'static str,
    pub input: &'static str,
    pub output: &'static str,
}

impl Binding {
    /// Build an effect through a generated binding descriptor. Guest code does
    /// not spell dynamic service/method names at the call site.
    pub fn effect(&self, input: abi::Value) -> abi::Effect {
        abi::Effect {
            namespace: self.namespace.into(),
            operation: self.operation.into(),
            input,
        }
    }
}

#[derive(Default)]
pub struct EffectPlanBuilder {
    effects: Vec<abi::Effect>,
}

impl EffectPlanBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn call(mut self, binding: &Binding, input: abi::Value) -> Self {
        self.effects.push(binding.effect(input));
        self
    }

    pub fn build(self) -> abi::EffectPlan {
        abi::EffectPlan {
            effects: self.effects,
        }
    }
}

#[macro_export]
macro_rules! bindings {
    ($visibility:vis $name:ident = [$($namespace:literal :: $operation:literal ($input:literal) -> $output:literal),* $(,)?]) => {
        $visibility const $name: &[$crate::Binding] = &[
            $($crate::Binding { namespace: $namespace, operation: $operation, input: $input, output: $output }),*
        ];
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi::{ErrorCode, Value};

    #[derive(Clone, Debug, PartialEq, Eq, AxiomType)]
    struct TypedRecord {
        count: u64,
        label: String,
    }

    struct TypedEcho;

    impl TypedExtension for TypedEcho {
        fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
            invocation.require_export("echo")?;
            Ok(ExtensionResponse::new(invocation.input::<Value>()?))
        }
    }

    #[derive(Default)]
    struct MacroEcho;

    #[extension]
    impl MacroEcho {
        #[export(name = "macro_echo")]
        fn echo(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
            Ok(ExtensionResponse::new(invocation.input::<String>()?))
        }
    }

    struct Echo;
    impl Extension for Echo {
        fn invoke(&mut self, request_id: RequestId, invocation: Invocation) -> GuestMessage {
            completed(request_id, invocation.input)
        }
        fn resume(
            &mut self,
            request_id: RequestId,
            _: Vec<EffectOutcome>,
            _: Vec<EventBatch>,
        ) -> GuestMessage {
            completed(request_id, Value::Null)
        }
        fn cancel(&mut self, _: RequestId) -> core::result::Result<(), ExtensionError> {
            Ok(())
        }
    }

    fn send(machine: &mut GuestMachine<Echo>, message: HostMessage) -> GuestMessage {
        let input = abi::encode(&Frame::new(message), CodecLimits::default()).unwrap();
        let output = machine.dispatch(&input).unwrap();
        abi::decode_frame::<GuestMessage>(&output, CodecLimits::default())
            .unwrap()
            .message
    }

    #[test]
    fn negotiates_invokes_and_stops() {
        let mut machine = GuestMachine::new(Echo, CodecLimits::default());
        assert!(matches!(
            send(
                &mut machine,
                HostMessage::Negotiate {
                    supported: vec![abi::ABI_VERSION]
                }
            ),
            GuestMessage::Negotiated { .. }
        ));
        assert!(matches!(
            send(
                &mut machine,
                HostMessage::Initialize {
                    instance_id: 1,
                    application: "a".into(),
                    extension: "e".into()
                }
            ),
            GuestMessage::Initialized
        ));
        let id = RequestId::new(1).unwrap();
        let invocation = Invocation {
            export: "echo".into(),
            input: Value::Unsigned(5),
            snapshots: vec![],
            deadline_unix_ms: 10,
        };
        assert!(matches!(
            send(
                &mut machine,
                HostMessage::Invoke {
                    request_id: id,
                    invocation
                }
            ),
            GuestMessage::Completed { .. }
        ));
        assert!(matches!(
            send(&mut machine, HostMessage::Shutdown),
            GuestMessage::Shutdown
        ));
        let frame = Frame::new(HostMessage::Drain);
        let bytes = abi::encode(&frame, CodecLimits::default()).unwrap();
        assert_eq!(
            machine.dispatch(&bytes),
            Err(abi::ProtocolError::UseAfterShutdown)
        );
        let _ = ErrorCode::Guest;
    }

    #[test]
    fn transport_owner_rejects_wrong_lengths_and_reclaims_buffers() {
        let mut exports = GuestExports::new(Echo, CodecLimits { max_bytes: 32 });
        assert_eq!(exports.allocate(0), 0);
        assert_eq!(exports.allocate(33), 0);
        // Native pointers need not fit the wasm32 pointer width, so the full
        // allocate/dispatch route is exercised by the compiled guest/kernel
        // gate instead of this host unit test.
        assert_eq!(exports.live_allocations(), 0);
    }

    #[test]
    fn generated_bindings_build_an_ordered_effect_plan_without_dynamic_names() {
        const GET: Binding = Binding {
            namespace: "catalog",
            operation: "get-product",
            input: "GetProductInput",
            output: "Product",
        };
        const LOG: Binding = Binding {
            namespace: "runtime",
            operation: "logging-info",
            input: "LogInput",
            output: "Unit",
        };
        let plan = EffectPlanBuilder::new()
            .call(&GET, Value::Unsigned(7))
            .call(&LOG, Value::String("loaded".into()))
            .build();
        assert_eq!(plan.effects.len(), 2);
        assert_eq!(plan.effects[0].namespace, "catalog");
        assert_eq!(plan.effects[0].operation, "get-product");
        assert_eq!(plan.effects[1].namespace, "runtime");
        assert_eq!(plan.effects[1].operation, "logging-info");
    }

    #[test]
    fn typed_effects_yield_and_resume_without_raw_guest_messages() {
        struct CallsCatalog;
        impl TypedExtension for CallsCatalog {
            fn invoke(&mut self, _: TypedInvocation) -> Result<ExtensionResponse> {
                let operation = Operation::<u64, String>::generated(Binding {
                    namespace: "contract:catalog",
                    operation: "query_product",
                    input: "Unsigned",
                    output: "String",
                });
                Ok(ExtensionResponse::yield_for(
                    Effects::new().call(operation.call(42)),
                ))
            }

            fn resume(&mut self, resume: ResumeContext) -> Result<ExtensionResponse> {
                Ok(ExtensionResponse::new(resume.outcome::<String>(0)?))
            }
        }

        let request_id = RequestId::new(9).unwrap();
        let mut extension = boxed_extension(CallsCatalog);
        let yielded = extension.invoke(
            request_id,
            Invocation {
                export: "load".into(),
                input: Value::Null,
                snapshots: vec![],
                deadline_unix_ms: 1,
            },
        );
        let GuestMessage::Yielded { plan, .. } = yielded else {
            panic!("typed effects must lower to a yielded ABI message");
        };
        assert_eq!(plan.effects.len(), 1);

        let resumed = extension.resume(
            request_id,
            vec![EffectOutcome {
                index: 0,
                result: Ok(Value::String("Linen carryall".into())),
            }],
            vec![],
        );
        let GuestMessage::Completed { result, .. } = resumed else {
            panic!("typed resume must lower to a completed ABI message");
        };
        assert_eq!(result.output, Value::String("Linen carryall".into()));
    }

    #[test]
    fn extension_and_export_macros_generate_dispatch_factory_and_errors() {
        assert_eq!(AXIOM_EXTENSION_EXPORTS, &["macro_echo"]);
        let request_id = RequestId::new(12).unwrap();
        let mut extension = axiom_extension();
        let completed = extension.invoke(
            request_id,
            Invocation {
                export: "macro_echo".into(),
                input: Value::String("typed".into()),
                snapshots: vec![],
                deadline_unix_ms: 1,
            },
        );
        assert!(matches!(
            completed,
            GuestMessage::Completed {
                result: InvocationResult {
                    output: Value::String(ref value),
                    ..
                },
                ..
            } if value == "typed"
        ));
        let denied = extension.invoke(
            request_id,
            Invocation {
                export: "undeclared".into(),
                input: Value::Null,
                snapshots: vec![],
                deadline_unix_ms: 1,
            },
        );
        assert!(matches!(
            denied,
            GuestMessage::Failed {
                error: ExtensionError {
                    code: ErrorCode::InvalidInput,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn in_memory_test_host_commits_typed_state_and_exposes_audit_assertions() {
        #[derive(AxiomType)]
        struct CartState {
            discount_cents: u64,
            subtotal_cents: u64,
        }
        struct CartScope;
        impl UiScope for CartScope {
            const SCOPE: &'static str = "cart";
        }
        struct Discount;
        struct TestPricing;
        impl TypedExtension for TestPricing {
            fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
                let cart = UiState::<CartScope>::generated(&invocation)?;
                let subtotal = cart.generated_read::<u64>(&UiField::generated("subtotal_cents"))?;
                Ok(
                    ExtensionResponse::new(subtotal / 10).patch(cart.patch().generated_set(
                        &UiField::<Discount>::generated("discount_cents"),
                        subtotal / 10,
                    )),
                )
            }
        }

        let mut host = test::TestHost::new()
            .ui_state(
                "cart",
                3,
                CartState {
                    discount_cents: 0,
                    subtotal_cents: 12_000,
                },
            )
            .allow_ui_write("cart", "discount_cents");
        let mut extension = boxed_extension(TestPricing);
        let run = host.invoke(&mut *extension, "price", ()).unwrap();
        assert_eq!(run.output, Value::Unsigned(1_200));
        let cart: CartState = host.ui("cart").unwrap();
        assert_eq!(cart.discount_cents, 1_200);
        assert_eq!(host.revision("ui:cart"), Some(4));
        host.assert_wrote("ui:cart", "discount_cents");
        host.assert_no_denials();
    }

    #[test]
    fn in_memory_test_host_rolls_back_state_and_write_audit_on_denied_patch() {
        #[derive(AxiomType)]
        struct CartState {
            discount_cents: u64,
            total_cents: u64,
        }
        struct CartScope;
        impl UiScope for CartScope {
            const SCOPE: &'static str = "cart";
        }
        struct Discount;
        struct Total;
        struct TestPricing;
        impl TypedExtension for TestPricing {
            fn invoke(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
                let cart = UiState::<CartScope>::generated(&invocation)?;
                Ok(ExtensionResponse::new(()).patch(
                    cart.patch()
                        .generated_set(&UiField::<Discount>::generated("discount_cents"), 1_200_u64)
                        .generated_set(&UiField::<Total>::generated("total_cents"), 10_800_u64),
                ))
            }
        }

        let mut host = test::TestHost::new()
            .ui_state(
                "cart",
                3,
                CartState {
                    discount_cents: 0,
                    total_cents: 12_000,
                },
            )
            .allow_ui_write("cart", "discount_cents");
        let mut extension = boxed_extension(TestPricing);
        assert!(host.invoke(&mut *extension, "price", ()).is_err());

        let cart: CartState = host.ui("cart").unwrap();
        assert_eq!(cart.discount_cents, 0);
        assert_eq!(cart.total_cents, 12_000);
        assert_eq!(host.revision("ui:cart"), Some(3));
        assert!(!host.audit().iter().any(|record| {
            record.kind == test::AuditKind::Patch
                && record.operation.as_deref() == Some("discount_cents")
        }));
        assert!(host
            .audit()
            .iter()
            .any(|record| { record.kind == test::AuditKind::Shutdown }));
        host.assert_denied();
    }

    #[test]
    fn generated_state_and_store_builders_emit_coarse_atomic_proposals() {
        struct Count;
        struct Amount;
        let count = UiSelector::<Count>::generated("ui", "write-count", "checkout", "count");
        let amount =
            StoreSelector::<Amount>::generated("store", "write-amount", "orders", "amount");

        let patch = UiPatchBuilder::new("checkout", 4)
            .set(&count, Value::Unsigned(2))
            .build();
        assert_eq!(patch.resource, "ui:checkout");
        assert_eq!(patch.expected_revision, 4);

        let transaction = StoreTransactionBuilder::new(9)
            .set(&amount, "order-1", 7, Value::Unsigned(10))
            .set(&amount, "order-1", 7, Value::Unsigned(11))
            .build();
        assert_eq!(transaction.patches.len(), 1);
        assert_eq!(transaction.patches[0].operations.len(), 2);

        let cursor = StoreCursor::generated("store", "orders-cursor").page(Some("order-1"), 25);
        assert_eq!(cursor.namespace, "store");
        assert_eq!(cursor.operation, "orders-cursor");
        assert!(matches!(cursor.input, Value::Record(_)));
    }

    #[test]
    fn axiom_type_derive_is_canonical_and_strict() {
        let value = TypedRecord {
            count: 7,
            label: "ready".into(),
        }
        .encode();
        let Value::Record(fields) = &value else {
            panic!("derived record did not encode as a record");
        };
        assert_eq!(
            fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
            vec!["count", "label"]
        );
        assert_eq!(
            TypedRecord::decode(&value).unwrap(),
            TypedRecord {
                count: 7,
                label: "ready".into()
            }
        );
        let out_of_order = Value::Record(vec![
            Field {
                name: "label".into(),
                value: Value::String("ready".into()),
            },
            Field {
                name: "count".into(),
                value: Value::Unsigned(7),
            },
        ]);
        assert!(TypedRecord::decode(&out_of_order).is_err());
    }

    #[test]
    fn typed_dispatch_preserves_the_existing_guest_message_bytes() {
        let request_id = RequestId::new(8).unwrap();
        let invocation = Invocation {
            export: "echo".into(),
            input: Value::Unsigned(42),
            snapshots: vec![],
            deadline_unix_ms: 100,
        };
        let mut typed = boxed_extension(TypedEcho);
        let typed = typed.invoke(request_id, invocation.clone());
        let raw = completed(request_id, invocation.input);
        assert_eq!(
            abi::encode(&Frame::new(typed), CodecLimits::default()).unwrap(),
            abi::encode(&Frame::new(raw), CodecLimits::default()).unwrap()
        );
    }

    #[test]
    fn deterministic_primitives_and_typed_ui_patch_have_no_raw_value_plumbing() {
        struct Cart;
        impl UiScope for Cart {
            const SCOPE: &'static str = "cart";
        }
        let request_id = RequestId::new(3).unwrap();
        let invocation = TypedInvocation::new(
            request_id,
            Invocation {
                export: "price".into(),
                input: Value::Null,
                snapshots: vec![abi::Snapshot {
                    resource: "ui:cart".into(),
                    revision: 4,
                    value: typed::canonical_record(vec![("subtotal", Value::Unsigned(23_700))]),
                }],
                deadline_unix_ms: 100,
            },
        );
        let state = UiState::<Cart>::generated(&invocation).unwrap();
        let subtotal: Cents = state
            .generated_read(&UiField::generated("subtotal"))
            .unwrap();
        let discount = Percentage::from_whole(15).unwrap().of(subtotal).unwrap();
        assert_eq!(discount, Cents::new(3_555));
        assert_eq!(discount.as_saving(), "−$35.55");
        let patch: Patch = state
            .patch()
            .generated_set(&UiField::<Cents>::generated("discount"), discount)
            .into();
        assert_eq!(patch.resource, "ui:cart");
        assert_eq!(patch.expected_revision, 4);
        assert!(matches!(
            &patch.operations[0],
            PatchOperation::Set {
                value: Value::Unsigned(3_555),
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_defaults_cancel_safely_and_reject_undeclared_resume() {
        let request_id = RequestId::new(11).unwrap();
        let mut extension = boxed_extension(TypedEcho);
        assert!(extension.cancel(request_id).is_ok());
        assert!(matches!(
            extension.resume(request_id, vec![], vec![]),
            GuestMessage::Failed {
                error: ExtensionError {
                    code: ErrorCode::InvalidInput,
                    ..
                },
                ..
            }
        ));
    }
}
