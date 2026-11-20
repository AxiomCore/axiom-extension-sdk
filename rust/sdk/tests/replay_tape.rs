//! X9d: `TestHost::from_tape` (feature `replay-tape`) against tapes recorded
//! by the broker from the wasm build of the runtime's async Rust conformance
//! guest (`axiom-runtime/extensions/fixtures/async-rounds-guest`, export `run`
//! and `sync_pure`; recorded by `vac-conformance/tests/replay_tape.rs`). The
//! guest below is the same source. Run with `--features replay-tape`.
#![cfg(feature = "replay-tape")]

use axiom_extension_sdk::{abi::Value, prelude::*, test::TestHost, AsyncExtension, Binding};

const fn catalog(operation: &'static str) -> Operation<Value, u64> {
    Operation::generated(Binding { namespace: "catalog", operation, input: "Value", output: "Unsigned" })
}
const LOAD: Operation<Value, u64> = catalog("load");
const PRICE: Operation<Value, u64> = catalog("price");
const RESERVE: Operation<Value, u64> = catalog("reserve");

struct Checkout;
impl UiScope for Checkout {
    const SCOPE: &'static str = "checkout";
}
const COUNT: UiField<u64> = UiField::generated("count");

fn prev(value: Option<u64>) -> Value {
    Value::Record(vec![axiom_extension_sdk::abi::Field {
        name: "prev".into(),
        value: value.map(Value::Unsigned).unwrap_or(Value::Null),
    }])
}

fn n(value: u64) -> Value {
    Value::Unsigned(value)
}

fn commit(cx: &Cx, total: u64) -> Result<()> {
    let checkout = UiState::<Checkout>::generated(cx.invocation())?;
    cx.patch(checkout.patch().generated_set(&COUNT, total));
    Ok(())
}

mod same {
    use super::*;

    #[derive(Default)]
    struct Guest;

    #[extension]
    impl Guest {
        #[export]
        async fn run(cx: &Cx, _program: Value) -> Result<Value> {
            let load = cx.call(LOAD.call(prev(None))).await?;
            let price = cx.call(PRICE.call(prev(Some(load)))).await?;
            let reserve = cx.call(RESERVE.call(prev(Some(price)))).await?;
            commit(cx, load + price + reserve)?;
            Ok(Value::Record(vec![
                axiom_extension_sdk::abi::Field { name: "outcomes".into(), value: Value::List(vec![n(load), n(price), n(reserve)]) },
                axiom_extension_sdk::abi::Field { name: "round".into(), value: n(u64::from(cx.round())) },
            ]))
        }

        #[export]
        fn sync_pure(&mut self, invocation: TypedInvocation) -> Result<ExtensionResponse> {
            Ok(ExtensionResponse::new(invocation.input::<u64>()? + 1))
        }
    }

    pub fn extension() -> AsyncExtension {
        axiom_extension()
    }
}

/// The same guest after a change: it prices with the plain load value.
mod changed {
    use super::*;

    #[derive(Default)]
    struct Guest;

    #[extension]
    impl Guest {
        #[export]
        async fn run(cx: &Cx, _program: Value) -> Result<Value> {
            let load = cx.call(LOAD.call(prev(None))).await?;
            let price = cx.call(PRICE.call(n(load))).await?;
            Ok(n(price))
        }
    }

    pub fn extension() -> AsyncExtension {
        axiom_extension()
    }
}

fn tape(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/tapes/{name}.axtape.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn recorded_tapes_replay_identically_in_process() {
    for (name, yields, status) in [("vac_rust_w2a-run", 3, "completed"), ("vac_rust_w2a-sync_pure", 0, "completed"), ("vac_rust_w2a-run-cancelled", 2, "cancelled"), ("vac_rust_w2a-run-max-rounds", 3, "refused")] {
        let replay = TestHost::from_tape(&tape(name)).unwrap();
        let report = replay.replay(same::extension()).unwrap_or_else(|error| panic!("{name}: {}", error.message));
        assert_eq!((report.yields, report.status.as_str()), (yields, status), "{name}");
    }
    let report = TestHost::from_tape(&tape("vac_rust_w2a-run")).unwrap().replay(same::extension()).unwrap();
    assert_eq!(report.result.unwrap().patches.len(), 1);
}

#[test]
fn a_changed_guest_is_named_at_its_first_differing_call() {
    let error = TestHost::from_tape(&tape("vac_rust_w2a-run")).unwrap().replay(changed::extension()).unwrap_err();
    assert!(
        error.message.starts_with("AXIOM_EXTENSION_REPLAY_DIFFERS: round 2, call 0: recorded catalog.price({\"Record\":[{\"name\":\"prev\",\"value\":{\"Unsigned\":10}}]}) but the guest yielded catalog.price({\"Unsigned\":10})"),
        "{}",
        error.message
    );
}

#[test]
fn tampered_tapes_fail_the_integrity_check() {
    let text = String::from_utf8(tape("vac_rust_w2a-run")).unwrap();
    let tampered = text.replacen("\"Unsigned\": 20", "\"Unsigned\": 21", 1);
    assert_ne!(tampered, text);
    let error = TestHost::from_tape(tampered.as_bytes()).unwrap_err();
    assert!(error.message.contains("integrity check"), "{}", error.message);
    assert!(TestHost::from_tape(b"{}").unwrap_err().message.contains("not an axiom-extension-replay-tape"));
}
