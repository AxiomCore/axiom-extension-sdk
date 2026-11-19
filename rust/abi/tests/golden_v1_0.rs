//! Golden byte fixtures for every ABI 1.0 message (X8b, test 12).
//!
//! The hex strings below were produced by the 1.0 crate at `origin/main`
//! before ABI 1.1 was added. ABI 1.1 only appends enum variants, so every
//! 1.0 message must still encode to exactly these bytes and decode back to
//! the same value, through both the checked and the trusted decoder.
use axiom_extension_abi::*;

fn h(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn values() -> Value {
    Value::List(vec![
        Value::Null,
        Value::Bool(true),
        Value::Signed(-3),
        Value::Unsigned(1 << 40),
        Value::String("s".into()),
        Value::Bytes(vec![0, 255]),
        Value::Record(vec![Field {
            name: "f".into(),
            value: Value::Unsigned(1),
        }]),
        Value::Variant {
            case: "some".into(),
            value: Some(Box::new(Value::Bool(false))),
        },
        Value::Variant {
            case: "none".into(),
            value: None,
        },
        Value::Handle(Handle {
            id: 9,
            generation: 2,
            kind: HandleKind::Stream,
        }),
    ])
}

fn invocation() -> Invocation {
    Invocation {
        export: "run".into(),
        input: values(),
        snapshots: vec![Snapshot {
            resource: "ui:cart".into(),
            revision: 70_000,
            value: Value::Signed(-5),
        }],
        deadline_unix_ms: 1_700_000_000_000,
    }
}

fn effect(name: &str) -> Effect {
    Effect {
        namespace: "catalog".into(),
        operation: name.into(),
        input: Value::String("sku".into()),
    }
}

fn error(code: ErrorCode) -> ExtensionError {
    ExtensionError {
        code,
        message: "m".into(),
        retryable: true,
    }
}

fn result() -> InvocationResult {
    let path = vec!["a".to_string(), "b".to_string()];
    InvocationResult {
        output: Value::String("out".into()),
        patches: vec![Patch {
            resource: "ui:cart".into(),
            expected_revision: 3,
            operations: vec![
                PatchOperation::Set {
                    path: path.clone(),
                    value: Value::Unsigned(1),
                },
                PatchOperation::Unset { path: path.clone() },
                PatchOperation::Increment {
                    path: path.clone(),
                    amount: -2,
                },
                PatchOperation::Append {
                    path: path.clone(),
                    value: Value::Null,
                },
                PatchOperation::CompareAndSet {
                    path,
                    expected: Value::Null,
                    replacement: Value::Bool(true),
                },
                PatchOperation::Dispatch {
                    action: "go".into(),
                    payload: Value::Null,
                },
            ],
        }],
        transactions: vec![TransactionProposal {
            transaction_id: 5,
            patches: vec![],
        }],
        emitted_events: vec![Event {
            sequence: 1,
            value: Value::Null,
        }],
    }
}

/// Every 1.0 host message, in declaration order.
fn host_messages() -> Vec<(&'static str, HostMessage)> {
    let id = RequestId(7);
    vec![
        (
            "negotiate",
            HostMessage::Negotiate {
                supported: vec![ABI_VERSION],
            },
        ),
        (
            "initialize",
            HostMessage::Initialize {
                instance_id: 300,
                application: "app".into(),
                extension: "ext".into(),
            },
        ),
        (
            "invoke",
            HostMessage::Invoke {
                request_id: id,
                invocation: invocation(),
            },
        ),
        (
            "resume",
            HostMessage::Resume {
                request_id: id,
                outcomes: vec![
                    EffectOutcome {
                        index: 0,
                        result: Ok(Value::Bytes(vec![1, 2, 3])),
                    },
                    EffectOutcome {
                        index: 1,
                        result: Err(error(ErrorCode::Denied)),
                    },
                ],
                events: vec![EventBatch {
                    subscription: Handle {
                        id: 1,
                        generation: 1,
                        kind: HandleKind::StoreCursor,
                    },
                    events: vec![Event {
                        sequence: 4,
                        value: Value::Bool(true),
                    }],
                    dropped: 2,
                    terminal: true,
                }],
            },
        ),
        ("cancel", HostMessage::Cancel { request_id: id }),
        ("drain", HostMessage::Drain),
        ("shutdown", HostMessage::Shutdown),
    ]
}

/// Every 1.0 guest message, in declaration order.
fn guest_messages() -> Vec<(&'static str, GuestMessage)> {
    let id = RequestId(7);
    vec![
        (
            "negotiated",
            GuestMessage::Negotiated {
                selected: ABI_VERSION,
            },
        ),
        ("initialized", GuestMessage::Initialized),
        (
            "yielded",
            GuestMessage::Yielded {
                request_id: id,
                plan: EffectPlan {
                    effects: vec![effect("get"), effect("price")],
                },
            },
        ),
        (
            "completed",
            GuestMessage::Completed {
                request_id: id,
                result: result(),
            },
        ),
        (
            "failed",
            GuestMessage::Failed {
                request_id: id,
                error: error(ErrorCode::Protocol),
            },
        ),
        ("cancelled", GuestMessage::Cancelled { request_id: id }),
        ("drained", GuestMessage::Drained),
        ("shutdown", GuestMessage::Shutdown),
        (
            "yielded_group",
            GuestMessage::YieldedGroup {
                request_id: id,
                group: EffectGroup {
                    format: EFFECT_GROUP_FORMAT.into(),
                    limit: 2,
                    mode: EffectGroupMode::Collect,
                    branches: vec![NamedEffect {
                        name: "first".into(),
                        effect: effect("get"),
                    }],
                },
            },
        ),
    ]
}

const GOLDEN_HOST: &[(&str, &str)] = &[
    ("negotiate", "41584531010000010100"),
    ("initialize", "41584531010001ac020361707003657874"),
    ("invoke", "41584531010002070372756e060a000101020503808080808020040173050200ff0701016603010804736f6d6501010008046e6f6e650009090201010775693a63617274f0a204020980d095ffbc31"),
    ("resume", "41584531010003070200000503010203010101016d0101010102010401010201"),
    ("cancel", "4158453101000407"),
    ("drain", "41584531010005"),
    ("shutdown", "41584531010006"),
];
const GOLDEN_GUEST: &[(&str, &str)] = &[
    ("negotiated", "415845310100000100"),
    ("initialized", "41584531010001"),
    ("yielded", "41584531010002070207636174616c6f67036765740403736b7507636174616c6f670570726963650403736b75"),
    ("completed", "415845310100030704036f7574010775693a636172740306000201610162030101020161016202020161016203030201610162000402016101620001010502676f00010500010100"),
    ("failed", "415845310100040708016d01"),
    ("cancelled", "4158453101000507"),
    ("drained", "41584531010006"),
    ("shutdown", "41584531010007"),
    ("yielded_group", "41584531010008071f6178696f6d2d657874656e73696f6e2d6566666563742d67726f75702f763102010105666972737407636174616c6f67036765740403736b75"),
];

#[test]
fn every_abi_1_0_message_keeps_its_golden_bytes() {
    let limits = CodecLimits::default();
    if std::env::var_os("AXIOM_ABI_GOLDEN_PRINT").is_some() {
        for (name, message) in host_messages() {
            println!(
                "HOST (\"{name}\", \"{}\"),",
                h(&encode(&Frame::new(message), limits).unwrap())
            );
        }
        for (name, message) in guest_messages() {
            println!(
                "GUEST (\"{name}\", \"{}\"),",
                h(&encode(&Frame::new(message), limits).unwrap())
            );
        }
        return;
    }
    let host = host_messages();
    assert_eq!(host.len(), GOLDEN_HOST.len());
    for ((name, message), (golden_name, golden)) in host.into_iter().zip(GOLDEN_HOST) {
        assert_eq!(name, *golden_name);
        let frame = Frame::new(message);
        let bytes = encode(&frame, limits).unwrap();
        assert_eq!(h(&bytes), *golden, "host {name} changed its 1.0 encoding");
        assert_eq!(
            decode_frame::<HostMessage>(&unhex(golden), limits).unwrap(),
            frame
        );
        assert_eq!(
            decode_frame_trusted::<HostMessage>(&unhex(golden), limits).unwrap(),
            frame
        );
    }
    let guest = guest_messages();
    assert_eq!(guest.len(), GOLDEN_GUEST.len());
    for ((name, message), (golden_name, golden)) in guest.into_iter().zip(GOLDEN_GUEST) {
        assert_eq!(name, *golden_name);
        let frame = Frame::new(message);
        let bytes = encode(&frame, limits).unwrap();
        assert_eq!(h(&bytes), *golden, "guest {name} changed its 1.0 encoding");
        assert_eq!(
            decode_frame::<GuestMessage>(&unhex(golden), limits).unwrap(),
            frame
        );
    }
}
