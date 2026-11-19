//! X9c: interned source sites on the ABI 1.1 wire (`YieldedRoundSited`).
//!
//! The variant is appended (tag 11), a 1.1 guest without sites encodes
//! exactly as before X9c, sites are not part of the plan hash, a 1.0 session
//! never accepts the variant, and the strict 1.0 decoders reject it.
use axiom_extension_abi::*;

fn h(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn plan(n: usize) -> RoundPlan {
    RoundPlan::Plan(EffectPlan {
        effects: (0..n)
            .map(|_| Effect {
                namespace: "c".into(),
                operation: "q".into(),
                input: Value::Null,
            })
            .collect(),
    })
}

fn group(n: usize) -> RoundPlan {
    RoundPlan::Group(EffectGroup {
        format: EFFECT_GROUP_FORMAT.into(),
        limit: 2,
        mode: EffectGroupMode::Collect,
        branches: (0..n)
            .map(|i| NamedEffect {
                name: format!("b{i}"),
                effect: Effect {
                    namespace: "c".into(),
                    operation: "q".into(),
                    input: Value::Null,
                },
            })
            .collect(),
    })
}

fn invocation() -> Invocation {
    Invocation {
        export: "run".into(),
        input: Value::Null,
        snapshots: vec![],
        deadline_unix_ms: 0,
    }
}

#[test]
fn sited_yield_is_appended_with_pinned_bytes() {
    let limits = CodecLimits::default();
    let frame = Frame::with_abi(
        ABI_VERSION_1_1,
        GuestMessage::YieldedRoundSited {
            request_id: RequestId(7),
            round: 1,
            replayed: vec![],
            plan: plan(2),
            sites: vec![Some(3), None],
        },
    );
    let bytes = encode(&frame, limits).unwrap();
    // magic, 1.1, tag 11, id 7, round 1, no replays, RoundPlan::Plan (0),
    // 2 effects ("c", "q", Null) x2, 2 sites: Some(3), None.
    assert_eq!(
        h(&bytes),
        "41584531010\
         10b070100\
         0002\
         0163017100\
         0163017100\
         02010300"
            .replace(' ', "")
    );
    assert_eq!(
        decode_frame_versioned::<GuestMessage>(&bytes, limits, &[ABI_VERSION_1_1]).unwrap(),
        frame
    );
    // A large id is a plain varint; it is still only a label.
    let big = encode(
        &Frame::with_abi(
            ABI_VERSION_1_1,
            GuestMessage::YieldedRoundSited {
                request_id: RequestId(7),
                round: 1,
                replayed: vec![],
                plan: plan(1),
                sites: vec![Some(u32::MAX)],
            },
        ),
        limits,
    )
    .unwrap();
    assert!(h(&big).ends_with("0101ffffffff0f"), "{}", h(&big));
}

#[test]
fn a_guest_without_sites_encodes_exactly_as_before_x9c() {
    let limits = CodecLimits::default();
    for p in [plan(0), plan(1), plan(3), group(2)] {
        let legacy = GuestMessage::YieldedRound {
            request_id: RequestId(9),
            round: 2,
            replayed: vec![[0x22; 32]],
            plan: p.clone(),
        };
        let legacy_bytes = encode(&Frame::with_abi(ABI_VERSION_1_1, legacy.clone()), limits).unwrap();
        // No sites at all, or every site unknown: the plain `YieldedRound`.
        for sites in [vec![], vec![None; p.len()]] {
            let built = yielded_round(RequestId(9), 2, vec![[0x22; 32]], p.clone(), sites);
            assert_eq!(built, legacy);
            assert_eq!(
                encode(&Frame::with_abi(ABI_VERSION_1_1, built), limits).unwrap(),
                legacy_bytes
            );
        }
        // Tag 9 (YieldedRound), as pinned by rounds_v1_1.rs.
        assert_eq!(&h(&legacy_bytes)[..14], "41584531010109");
    }
    // One known site switches to the sited form (tag 11).
    let sited = yielded_round(RequestId(9), 2, vec![[0x22; 32]], plan(2), vec![None, Some(0)]);
    assert!(matches!(sited, GuestMessage::YieldedRoundSited { .. }));
    let bytes = encode(&Frame::with_abi(ABI_VERSION_1_1, sited), limits).unwrap();
    assert_eq!(&h(&bytes)[..14], "4158453101010b");
}

/// Hash rule (contract x9c §2): sites are excluded from the plan hash. The
/// hash is over `RoundPlan` only, so a sited and an unsited yield of the
/// same plan replay against the same tape record, and a guest whose sites
/// differ between rounds is not "non-deterministic".
#[test]
fn sites_are_not_part_of_the_plan_hash() {
    let p = plan(2);
    let base = round_plan_hash(&p);
    for sites in [vec![Some(0), Some(1)], vec![Some(1), Some(0)], vec![None, Some(7)]] {
        let GuestMessage::YieldedRoundSited { plan, .. } =
            yielded_round(RequestId(1), 1, vec![], p.clone(), sites)
        else {
            panic!("expected the sited form");
        };
        assert_eq!(round_plan_hash(&plan), base);
    }
    // The hash still binds the plan itself.
    assert_ne!(round_plan_hash(&plan(3)), base);
}

#[test]
fn round_plan_len_counts_effects_or_branches() {
    assert_eq!(plan(0).len(), 0);
    assert!(plan(0).is_empty());
    assert_eq!(plan(3).len(), 3);
    assert_eq!(group(4).len(), 4);
}

fn session(version: AbiVersion) -> Lifecycle {
    let mut life = Lifecycle::default();
    life.host(&HostMessage::Negotiate {
        supported: vec![version],
    })
    .unwrap();
    life.guest(&GuestMessage::Negotiated { selected: version }).unwrap();
    life.host(&HostMessage::Initialize {
        instance_id: 1,
        application: "a".into(),
        extension: "e".into(),
    })
    .unwrap();
    life.guest(&GuestMessage::Initialized).unwrap();
    life
}

fn sited(id: u64, round: u16, replayed: usize) -> GuestMessage {
    GuestMessage::YieldedRoundSited {
        request_id: RequestId(id),
        round,
        replayed: vec![[0; 32]; replayed],
        plan: plan(1),
        sites: vec![Some(0)],
    }
}

#[test]
fn the_1_1_lifecycle_treats_a_sited_yield_as_a_yield() {
    let mut life = session(ABI_VERSION_1_1);
    life.host(&HostMessage::InvokeRound {
        request_id: RequestId(4),
        invocation: invocation(),
        round: 1,
        max_rounds: 2,
        tape: vec![],
    })
    .unwrap();
    // Same sequencing rules as `YieldedRound`: wrong id, wrong round and a
    // wrong replay count are refused.
    assert_eq!(life.guest(&sited(5, 1, 0)), Err(ProtocolError::InvalidTransition));
    assert_eq!(life.guest(&sited(4, 2, 0)), Err(ProtocolError::InvalidTransition));
    assert_eq!(life.guest(&sited(4, 1, 1)), Err(ProtocolError::InvalidTransition));
    life.guest(&sited(4, 1, 0)).unwrap();
    life.host(&HostMessage::InvokeRound {
        request_id: RequestId(4),
        invocation: invocation(),
        round: 2,
        max_rounds: 2,
        tape: vec![RoundRecord {
            plan_hash: [0; 32],
            outcomes: vec![],
            events: vec![],
        }],
    })
    .unwrap();
    life.guest(&sited(4, 2, 1)).unwrap();
    life.host(&HostMessage::Cancel {
        request_id: RequestId(4),
    })
    .unwrap();
    life.guest(&GuestMessage::Cancelled {
        request_id: RequestId(4),
    })
    .unwrap();
}

#[test]
fn a_1_0_session_never_accepts_a_sited_yield() {
    let mut life = session(ABI_VERSION);
    life.host(&HostMessage::Invoke {
        request_id: RequestId(1),
        invocation: invocation(),
    })
    .unwrap();
    assert_eq!(life.guest(&sited(1, 1, 0)), Err(ProtocolError::InvalidTransition));
    assert_eq!(life.guest(&sited(1, 0, 0)), Err(ProtocolError::InvalidTransition));
    // The strict 1.0 decoders reject the 1.1 frame outright.
    let bytes = encode(&Frame::with_abi(ABI_VERSION_1_1, sited(1, 1, 0)), CodecLimits::default()).unwrap();
    assert!(decode_frame::<GuestMessage>(&bytes, CodecLimits::default()).is_err());
    assert!(decode_frame_trusted::<GuestMessage>(&bytes, CodecLimits::default()).is_err());
    // A 1.0-stamped frame carrying the variant is refused by a 1.1 decoder.
    let mut stamped = bytes.clone();
    stamped[5] = 0;
    assert!(decode_frame_versioned::<GuestMessage>(&stamped, CodecLimits::default(), &[ABI_VERSION_1_1]).is_err());
}

#[test]
fn sited_frames_survive_a_mutation_corpus_and_oversized_claims() {
    let limits = CodecLimits { max_bytes: 4096 };
    let canonical = encode(
        &Frame::with_abi(
            ABI_VERSION_1_1,
            GuestMessage::YieldedRoundSited {
                request_id: RequestId(7),
                round: 2,
                replayed: vec![[0x11; 32]],
                plan: group(3),
                sites: vec![Some(0), None, Some(70_000)],
            },
        ),
        limits,
    )
    .unwrap();
    let mut corpus = vec![canonical.clone()];
    for end in 0..canonical.len() {
        corpus.push(canonical[..end].to_vec());
    }
    for index in 0..canonical.len() {
        for mask in [0x01_u8, 0x80, 0xff] {
            let mut mutated = canonical.clone();
            mutated[index] ^= mask;
            corpus.push(mutated);
        }
    }
    // A sites vector claiming 2^32 entries in a tiny frame.
    let mut huge = canonical[..canonical.len() - 6].to_vec();
    huge.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f, 0x01]);
    corpus.push(huge);
    for bytes in corpus {
        let decoded = std::panic::catch_unwind(|| {
            decode_frame_versioned::<GuestMessage>(&bytes, limits, &[ABI_VERSION_1_1])
        })
        .expect("decoder must not panic");
        if let Ok(frame) = decoded {
            assert_eq!(encode(&frame, limits).unwrap(), bytes);
        }
    }
}
