//! X7e cross-language vectors: every case is computed by the Rust `Cents` /
//! `Percentage` primitives and must equal `validation/x7e-money-vectors.json`,
//! which the TypeScript (`typescript/test/money.test.mjs`) and Python
//! (`python/tests/test_money.py`) helpers replay. Regenerate after a deliberate
//! semantic change with `AXIOM_WRITE_MONEY_VECTORS=1 cargo test --test money_vectors`.
use axiom_extension_sdk::{abi::Value, AxiomDecode, AxiomEncode, Cents, Percentage, Result};
use std::path::PathBuf;

const U53: u64 = (1 << 53) - 1;
const MAX: u64 = u64::MAX;

fn outcome<T>(result: Result<T>, show: impl Fn(T) -> String) -> (String, String) {
    match result {
        Ok(value) => ("ok".into(), show(value)),
        Err(error) => ("error".into(), error.message),
    }
}
fn unsigned(value: Value) -> String {
    match value {
        Value::Unsigned(value) => value.to_string(),
        other => panic!("expected an unsigned wire value, got {other:?}"),
    }
}
fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn cases() -> Vec<(&'static str, Vec<String>, (String, String))> {
    let mut cases = vec![];
    let mut push = |op: &'static str, args: Vec<u64>, result: (String, String)| {
        cases.push((op, args.iter().map(u64::to_string).collect(), result));
    };
    for v in [0, 1, 99, 100, 12_345, U53, U53 + 1, MAX] {
        push("cents.new", vec![v], ("ok".into(), Cents::new(v).value().to_string()));
        push("cents.encode", vec![v], ("ok".into(), unsigned(Cents::new(v).encode())));
        push("cents.decode", vec![v], outcome(Cents::decode(&Value::Unsigned(v)), |c| c.value().to_string()));
    }
    for d in [0, 1, 42, MAX / 100, MAX / 100 + 1, U53, MAX] {
        push("cents.from_dollars", vec![d], outcome(Cents::from_dollars(d), |c| c.value().to_string()));
    }
    for (a, b) in [(500, 200), (200, 500), (0, 0), (MAX, 1), (1, MAX), (MAX, MAX), (U53 + 1, U53)] {
        push("cents.checked_sub", vec![a, b], outcome(Cents::new(a).checked_sub(Cents::new(b)), |c| c.value().to_string()));
    }
    for (a, b) in [(3, 5), (5, 3), (MAX, 0), (U53 + 1, U53 + 1)] {
        push("cents.min", vec![a, b], ("ok".into(), Cents::new(a).min(Cents::new(b)).value().to_string()));
    }
    for v in [0, 5, 99, 100, 101, 123_456, U53, U53 + 1, MAX] {
        push("cents.as_currency", vec![v], ("ok".into(), Cents::new(v).as_currency()));
    }
    for v in [0, 1_999, MAX] {
        push("cents.as_saving", vec![v], ("ok".into(), Cents::new(v).as_saving()));
    }
    for v in [0u16, 1, 50, 100, 101, u16::MAX] {
        push("percentage.from_whole", vec![v.into()], outcome(Percentage::from_whole(v), |p| p.basis_points().to_string()));
    }
    for v in [0u16, 1, 9_999, 10_000, 10_001, u16::MAX] {
        push("percentage.from_basis_points", vec![v.into()], outcome(Percentage::from_basis_points(v), |p| p.basis_points().to_string()));
    }
    for v in [0u64, 2_500, 10_000, 10_001, 65_535, 65_536, U53, MAX] {
        push("percentage.decode", vec![v], outcome(Percentage::decode(&Value::Unsigned(v)), |p| p.basis_points().to_string()));
    }
    for v in [0u16, 2_500, 10_000] {
        push("percentage.encode", vec![v.into()], ("ok".into(), unsigned(Percentage::from_basis_points(v).unwrap().encode())));
    }
    let per = MAX / 10_000;
    for (bp, amount) in [
        (0u16, 12_345u64), (10_000, 12_345), (1, 9_999), (1, 10_000), (2_500, 1_999), (3_333, 100), (5_000, 1),
        (10_000, per), (10_000, per + 1), (1, MAX), (2, MAX), (10_000, U53), (2_048, U53), (2_049, U53), (7_777, U53 + 1),
    ] {
        let percentage = Percentage::from_basis_points(bp).unwrap();
        push("percentage.of", vec![bp.into(), amount], outcome(percentage.of(Cents::new(amount)), |c| c.value().to_string()));
    }
    cases
}

fn render() -> String {
    let mut out = String::from("{\"format\":\"axiom-x7e-money-vectors/v1\",\"generator\":\"axiom-extension-sdk/rust/sdk/tests/money_vectors.rs\",\"cases\":[\n");
    let rows: Vec<String> = cases()
        .into_iter()
        .map(|(op, args, (kind, value))| {
            let args = args.iter().map(|a| json_string(a)).collect::<Vec<_>>().join(",");
            format!("{{\"op\":{},\"args\":[{}],\"{}\":{}}}", json_string(op), args, kind, json_string(&value))
        })
        .collect();
    out.push_str(&rows.join(",\n"));
    out.push_str("\n]}\n");
    out
}

#[test]
fn checked_in_vectors_equal_the_rust_primitives() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../validation/x7e-money-vectors.json");
    let rendered = render();
    if std::env::var_os("AXIOM_WRITE_MONEY_VECTORS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let stored = std::fs::read_to_string(&path).expect("validation/x7e-money-vectors.json");
    assert_eq!(stored, rendered, "vectors drifted from the Rust primitives; regenerate deliberately");
}
