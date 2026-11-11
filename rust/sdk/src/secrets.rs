//! Opaque host authority for one invocation. The broker revalidates every use.
use crate::{
    abi::{self, Field, Handle, HandleKind, Value},
    AxiomDecode, AxiomEncode, Binding, Result, SdkError,
};
use alloc::{string::String, vec, vec::Vec};
#[derive(Clone, Copy)]
pub struct SecretHandle(Handle);
impl core::fmt::Debug for SecretHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SecretHandle([opaque])")
    }
}
impl AxiomDecode for SecretHandle {
    fn decode(value: &Value) -> Result<Self> {
        match value {
            Value::Handle(handle) if handle.kind == HandleKind::Secret => Ok(Self(*handle)),
            _ => Err(SdkError::denied("a secret capability is required")),
        }
    }
}
impl AxiomEncode for SecretHandle {
    fn encode(self) -> Value {
        Value::Handle(self.0)
    }
}
/// Acquisition yields a handle; no material exists in the guest message.
pub fn acquire(binding: Binding) -> abi::Effect {
    binding.effect(Value::Null)
}
/// The named network binding owns origin and method. The reviewed host grant
/// pairs it with this handle and restricts the normalized path prefix.
pub fn authenticated_request(
    binding: Binding,
    path: String,
    body: Vec<u8>,
    secret: SecretHandle,
) -> abi::Effect {
    binding.effect(Value::Record(vec![
        Field {
            name: "body".into(),
            value: Value::Bytes(body),
        },
        Field {
            name: "path".into(),
            value: Value::String(path),
        },
        Field {
            name: "secret".into(),
            value: secret.encode(),
        },
    ]))
}
