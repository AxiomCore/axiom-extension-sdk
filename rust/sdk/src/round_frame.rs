//! W2A-1: the encoded `InvokeRound` frame of the round being run, readable
//! for the duration of that round only.
//!
//! An async export whose input parameter is `Value` takes the input by move
//! (H-1: no copy of the input every round). If the same export also asks the
//! context for the input (`Cx::input`, `cx.invocation().input()`, also from a
//! helper given `cx`), the input is decoded again from this frame, so such a
//! program sees exactly what it saw before H-1. The common case (the input is
//! read only from the argument) pays one copy of the frame bytes per round
//! instead of a copy of the input value tree.
//!
//! Without `std` there is no frame slot: the executor then copies the input
//! as before H-1 instead of moving it.

use crate::abi::{AbiVersion, CodecLimits, Value};
#[cfg(feature = "std")]
use crate::abi::{self, HostMessage};

#[cfg(feature = "std")]
std::thread_local! {
    static FRAME: core::cell::RefCell<Option<(alloc::vec::Vec<u8>, CodecLimits, AbiVersion)>> =
        const { core::cell::RefCell::new(None) };
}

/// Run `f` (one `InvokeRound`) with `bytes` readable through [`input`].
#[cfg(feature = "std")]
pub(crate) fn scoped<R>(bytes: &[u8], limits: CodecLimits, version: AbiVersion, f: impl FnOnce() -> R) -> R {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            FRAME.with(|frame| frame.borrow_mut().take());
        }
    }
    FRAME.with(|frame| *frame.borrow_mut() = Some((bytes.to_vec(), limits, version)));
    let _clear = Clear;
    f()
}

#[cfg(not(feature = "std"))]
pub(crate) fn scoped<R>(_bytes: &[u8], _limits: CodecLimits, _version: AbiVersion, f: impl FnOnce() -> R) -> R {
    f()
}

/// True while an `InvokeRound` frame is readable (a round run through
/// `GuestMachine`), so a moved input can be decoded again.
pub(crate) fn available() -> bool {
    #[cfg(feature = "std")]
    {
        FRAME.with(|frame| frame.borrow().is_some())
    }
    #[cfg(not(feature = "std"))]
    {
        false
    }
}

/// The invocation input of the current round's frame, decoded again.
pub(crate) fn input() -> Option<Value> {
    #[cfg(feature = "std")]
    {
        FRAME.with(|frame| {
            let frame = frame.borrow();
            let (bytes, limits, version) = frame.as_ref()?;
            let decoded = abi::decode_frame_trusted_versioned::<HostMessage>(bytes, *limits, &[*version]).ok()?;
            match decoded.message {
                HostMessage::InvokeRound { invocation, .. } => Some(invocation.input),
                _ => None,
            }
        })
    }
    #[cfg(not(feature = "std"))]
    {
        None
    }
}
