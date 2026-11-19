//! ABI 1.1 wire types, plan hash, frame versions and the negotiated lifecycle
//! (X8b tests 9 and 12, ABI side).
use axiom_extension_abi::*;
use sha2::{Digest, Sha256};

fn h(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn invocation() -> Invocation {
    Invocation {
        export: "run".into(),
        input: Value::Unsigned(3),
        snapshots: vec![],
        deadline_unix_ms: 42,
    }
}

fn plan(operation: &str) -> RoundPlan {
    RoundPlan::Plan(EffectPlan {
        effects: vec![Effect {
            namespace: "catalog".into(),
            operation: operation.into(),
            input: Value::Null,
        }],
    })
}

fn record(operation: &str) -> RoundRecord {
    RoundRecord {
        plan_hash: round_plan_hash(&plan(operation)),
        outcomes: vec![EffectOutcome {
            index: 0,
            result: Ok(Value::Unsigned(1)),
        }],
        events: vec![],
    }
}

fn result() -> InvocationResult {
    InvocationResult {
        output: Value::Null,
        patches: vec![],
        transactions: vec![],
        emitted_events: vec![],
    }
}

#[test]
fn new_variants_are_appended_with_pinned_bytes() {
    let limits = CodecLimits::default();
    let id = RequestId(7);
    // Variant tags: HostMessage::InvokeRound = 7 (after Shutdown = 6);
    // GuestMessage::YieldedRound = 9, CompletedRound = 10 (after YieldedGroup = 8).
    let invoke = Frame::with_abi(
        ABI_VERSION_1_1,
        HostMessage::InvokeRound {
            request_id: id,
            invocation: invocation(),
            round: 2,
            max_rounds: 3,
            tape: vec![RoundRecord {
                plan_hash: [0xab; 32],
                outcomes: vec![],
                events: vec![],
            }],
        },
    );
    let bytes = encode(&invoke, limits).unwrap();
    let expected = format!(
        "415845310101070703{}{}{}{}0000",
        "72756e0303002a", // "run", Unsigned(3), no snapshots, deadline 42
        "0203",           // round 2, max_rounds 3
        "01",             // one record
        "ab".repeat(32)
    );
    assert_eq!(h(&bytes), expected);
    assert_eq!(
        decode_frame_versioned::<HostMessage>(&bytes, limits, &[ABI_VERSION_1_1]).unwrap(),
        invoke
    );
    let yielded = Frame::with_abi(
        ABI_VERSION_1_1,
        GuestMessage::YieldedRound {
            request_id: id,
            round: 2,
            replayed: vec![[0x11; 32]],
            plan: RoundPlan::Group(EffectGroup {
                format: EFFECT_GROUP_FORMAT.into(),
                limit: 1,
                mode: EffectGroupMode::Collect,
                branches: vec![],
            }),
        },
    );
    let bytes = encode(&yielded, limits).unwrap();
    assert_eq!(&h(&bytes)[..20], "41584531010109070201");
    assert_eq!(
        decode_frame_versioned::<GuestMessage>(&bytes, limits, &[ABI_VERSION_1_1]).unwrap(),
        yielded
    );
    let completed = Frame::with_abi(
        ABI_VERSION_1_1,
        GuestMessage::CompletedRound {
            request_id: id,
            round: 1,
            replayed: vec![],
            result: result(),
        },
    );
    let bytes = encode(&completed, limits).unwrap();
    assert_eq!(h(&bytes), "4158453101010a07010000000000");
}

#[test]
fn round_plan_hash_matches_the_adr_definition() {
    for plan in [
        plan("get"),
        RoundPlan::Group(EffectGroup {
            format: EFFECT_GROUP_FORMAT.into(),
            limit: 2,
            mode: EffectGroupMode::Collect,
            branches: vec![NamedEffect {
                name: "a".into(),
                effect: Effect {
                    namespace: "n".into(),
                    operation: "o".into(),
                    input: Value::Bool(true),
                },
            }],
        }),
    ] {
        let mut hasher = Sha256::new();
        hasher.update(b"axiom-extension-round-plan/v1");
        hasher.update(postcard::to_allocvec(&plan).unwrap());
        assert_eq!(
            round_plan_hash(&plan).as_slice(),
            hasher.finalize().as_slice()
        );
    }
    assert_ne!(round_plan_hash(&plan("get")), round_plan_hash(&plan("put")));
    // A plan and a one-branch group with the same effect hash differently.
    assert_eq!(
        h(&round_plan_hash(&plan("get"))),
        h(&{
            let mut hasher = Sha256::new();
            hasher.update(ROUND_PLAN_HASH_DOMAIN);
            hasher.update([0u8, 1, 7]);
            hasher.update(b"catalog");
            hasher.update([3]);
            hasher.update(b"get");
            hasher.update([0]);
            hasher.finalize()
        })
    );
}

#[test]
fn the_strict_1_0_decoders_still_reject_1_1_frames() {
    let limits = CodecLimits::default();
    let bytes = encode(
        &Frame::with_abi(ABI_VERSION_1_1, HostMessage::Drain),
        limits,
    )
    .unwrap();
    assert_eq!(
        decode_frame::<HostMessage>(&bytes, limits),
        Err(ProtocolError::AbiMismatch)
    );
    assert_eq!(
        decode_frame_trusted::<HostMessage>(&bytes, limits),
        Err(ProtocolError::AbiMismatch)
    );
    assert!(decode_frame_versioned::<HostMessage>(&bytes, limits, &[ABI_VERSION_1_1]).is_ok());
    assert!(
        decode_frame_trusted_versioned::<HostMessage>(&bytes, limits, &[ABI_VERSION_1_1]).is_ok()
    );
    // A session negotiated at one version rejects frames stamped with the other.
    assert_eq!(
        decode_frame_versioned::<HostMessage>(&bytes, limits, &[ABI_VERSION]),
        Err(ProtocolError::AbiMismatch)
    );
    for abi in [
        AbiVersion { major: 1, minor: 2 },
        AbiVersion { major: 2, minor: 1 },
    ] {
        let bytes = encode(&Frame::with_abi(abi, HostMessage::Drain), limits).unwrap();
        for accepted in [&[abi][..], &SUPPORTED_ABI_VERSIONS[..]] {
            assert_eq!(
                decode_frame_versioned::<HostMessage>(&bytes, limits, accepted),
                Err(ProtocolError::AbiMismatch)
            );
            assert_eq!(
                decode_frame_trusted_versioned::<HostMessage>(&bytes, limits, accepted),
                Err(ProtocolError::AbiMismatch)
            );
        }
    }
}

fn session(offer: &[AbiVersion], select: AbiVersion) -> Result<Lifecycle, ProtocolError> {
    let mut life = Lifecycle::default();
    life.host(&HostMessage::Negotiate {
        supported: offer.to_vec(),
    })?;
    life.guest(&GuestMessage::Negotiated { selected: select })?;
    life.host(&HostMessage::Initialize {
        instance_id: 1,
        application: "a".into(),
        extension: "e".into(),
    })?;
    life.guest(&GuestMessage::Initialized)?;
    Ok(life)
}

fn invoke_round(id: u64, round: u16, max_rounds: u16, tape: usize) -> HostMessage {
    HostMessage::InvokeRound {
        request_id: RequestId(id),
        invocation: invocation(),
        round,
        max_rounds,
        tape: (0..tape).map(|_| record("get")).collect(),
    }
}

fn yielded_round(id: u64, round: u16, replayed: usize) -> GuestMessage {
    GuestMessage::YieldedRound {
        request_id: RequestId(id),
        round,
        replayed: vec![[0; 32]; replayed],
        plan: plan("get"),
    }
}

fn completed_round(id: u64, round: u16, replayed: usize) -> GuestMessage {
    GuestMessage::CompletedRound {
        request_id: RequestId(id),
        round,
        replayed: vec![[0; 32]; replayed],
        result: result(),
    }
}

#[test]
fn negotiation_selects_only_an_offered_supported_version() {
    let v12 = AbiVersion { major: 1, minor: 2 };
    assert_eq!(
        session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION_1_1)
            .unwrap()
            .negotiated(),
        Some(ABI_VERSION_1_1)
    );
    assert_eq!(
        session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION)
            .unwrap()
            .negotiated(),
        Some(ABI_VERSION)
    );
    // A guest claiming 1.1 against a 1.0-only (rounds disabled) offer.
    assert_eq!(
        session(&[ABI_VERSION], ABI_VERSION_1_1).unwrap_err(),
        ProtocolError::InvalidTransition
    );
    // A guest claiming 1.2, offered or not.
    assert_eq!(
        session(&SUPPORTED_ABI_VERSIONS, v12).unwrap_err(),
        ProtocolError::InvalidTransition
    );
    assert_eq!(
        session(&[v12, ABI_VERSION], v12).unwrap_err(),
        ProtocolError::InvalidTransition
    );
}

#[test]
fn a_1_0_session_rejects_every_round_variant() {
    let mut life = session(&[ABI_VERSION], ABI_VERSION).unwrap();
    assert_eq!(
        life.host(&invoke_round(1, 1, 3, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    life.host(&HostMessage::Invoke {
        request_id: RequestId(1),
        invocation: invocation(),
    })
    .unwrap();
    assert_eq!(
        life.guest(&yielded_round(1, 0, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    assert_eq!(
        life.guest(&completed_round(1, 0, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    // The 1.0 single-yield path is untouched.
    life.guest(&GuestMessage::Yielded {
        request_id: RequestId(1),
        plan: EffectPlan { effects: vec![] },
    })
    .unwrap();
    life.host(&HostMessage::Resume {
        request_id: RequestId(1),
        outcomes: vec![],
        events: vec![],
    })
    .unwrap();
    life.guest(&GuestMessage::Completed {
        request_id: RequestId(1),
        result: result(),
    })
    .unwrap();
}

#[test]
fn a_1_1_session_rejects_legacy_invoke_and_terminals() {
    let mut life = session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION_1_1).unwrap();
    assert_eq!(
        life.host(&HostMessage::Invoke {
            request_id: RequestId(1),
            invocation: invocation()
        }),
        Err(ProtocolError::InvalidTransition)
    );
    life.host(&invoke_round(1, 1, 2, 0)).unwrap();
    for legacy in [
        GuestMessage::Completed {
            request_id: RequestId(1),
            result: result(),
        },
        GuestMessage::Yielded {
            request_id: RequestId(1),
            plan: EffectPlan { effects: vec![] },
        },
    ] {
        assert_eq!(
            life.clone().guest(&legacy),
            Err(ProtocolError::InvalidTransition)
        );
    }
    life.guest(&yielded_round(1, 1, 0)).unwrap();
    assert_eq!(
        life.clone().host(&HostMessage::Resume {
            request_id: RequestId(1),
            outcomes: vec![],
            events: vec![]
        }),
        Err(ProtocolError::InvalidTransition)
    );
}

#[test]
fn round_sequencing_is_exact() {
    let ready = || session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION_1_1).unwrap();
    // Round 1 needs an empty tape and a max_rounds within the ceiling.
    for bad in [
        invoke_round(1, 2, 3, 1),
        invoke_round(1, 1, 3, 1),
        invoke_round(1, 1, 0, 0),
        invoke_round(1, 1, MAX_ROUNDS_CEILING + 1, 0),
    ] {
        assert_eq!(ready().host(&bad), Err(ProtocolError::InvalidTransition));
    }
    let mut life = ready();
    life.host(&invoke_round(1, 1, 3, 0)).unwrap();
    // `replayed` must have one hash per tape record (none in round 1).
    assert_eq!(
        life.clone().guest(&yielded_round(1, 1, 1)),
        Err(ProtocolError::InvalidTransition)
    );
    assert_eq!(
        life.clone().guest(&yielded_round(1, 2, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    assert_eq!(
        life.clone().guest(&yielded_round(2, 1, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    life.guest(&yielded_round(1, 1, 0)).unwrap();
    // Suspended: only the next round of the same request with the same ceiling.
    for bad in [
        invoke_round(2, 2, 3, 1),
        invoke_round(1, 3, 3, 2),
        invoke_round(1, 2, 4, 1),
        invoke_round(1, 2, 3, 0),
        invoke_round(1, 2, 3, 2),
    ] {
        assert_eq!(
            life.clone().host(&bad),
            Err(ProtocolError::InvalidTransition)
        );
    }
    life.host(&invoke_round(1, 2, 3, 1)).unwrap();
    assert_eq!(
        life.clone().guest(&completed_round(1, 2, 0)),
        Err(ProtocolError::InvalidTransition)
    );
    assert_eq!(
        life.clone().guest(&completed_round(1, 2, 2)),
        Err(ProtocolError::InvalidTransition)
    );
    life.guest(&yielded_round(1, 2, 1)).unwrap();
    life.host(&invoke_round(1, 3, 3, 2)).unwrap();
    life.guest(&yielded_round(1, 3, 2)).unwrap();
    // max_rounds counts yields: three yields are allowed, so round 4 (the
    // completing round) still runs ...
    life.host(&invoke_round(1, 4, 3, 3)).unwrap();
    life.guest(&yielded_round(1, 4, 3)).unwrap();
    // ... but there is no round 5 after a fourth yield; the host must cancel.
    assert_eq!(
        life.clone().host(&invoke_round(1, 5, 3, 4)),
        Err(ProtocolError::InvalidTransition)
    );
    life.host(&HostMessage::Cancel {
        request_id: RequestId(1),
    })
    .unwrap();
    life.guest(&GuestMessage::Cancelled {
        request_id: RequestId(1),
    })
    .unwrap();
    // A fresh invocation restarts at round 1.
    life.host(&invoke_round(2, 1, 3, 0)).unwrap();
    life.guest(&completed_round(2, 1, 0)).unwrap();
    assert_eq!(
        life.guest(&completed_round(2, 1, 0)),
        Err(ProtocolError::DuplicateTerminal)
    );
}

#[test]
fn round_frames_survive_a_mutation_corpus_without_panics() {
    let limits = CodecLimits { max_bytes: 4096 };
    let messages = [
        encode(
            &Frame::with_abi(ABI_VERSION_1_1, invoke_round(7, 2, 3, 1)),
            limits,
        )
        .unwrap(),
        encode(
            &Frame::with_abi(ABI_VERSION_1_1, yielded_round(7, 2, 1)),
            limits,
        )
        .unwrap(),
        encode(
            &Frame::with_abi(ABI_VERSION_1_1, completed_round(7, 2, 1)),
            limits,
        )
        .unwrap(),
    ];
    for canonical in messages {
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
        for bytes in corpus {
            let host = std::panic::catch_unwind(|| {
                decode_frame_versioned::<HostMessage>(&bytes, limits, &[ABI_VERSION_1_1])
            })
            .expect("decoder must not panic");
            let guest = std::panic::catch_unwind(|| {
                decode_frame_versioned::<GuestMessage>(&bytes, limits, &[ABI_VERSION_1_1])
            })
            .expect("decoder must not panic");
            if let Ok(frame) = host {
                assert_eq!(encode(&frame, limits).unwrap(), bytes);
            }
            if let Ok(frame) = guest {
                assert_eq!(encode(&frame, limits).unwrap(), bytes);
            }
        }
    }
}

/// `max_rounds = N` allows N yields: rounds 1..=N+1 are valid, round N+2 is
/// not (boundary cases N-1, N and N+1 yields for N = 1, 2 and 16).
#[test]
fn max_rounds_counts_yields_not_invokes() {
    assert_eq!(last_round(1), 2);
    assert_eq!(last_round(MAX_ROUNDS_CEILING), MAX_ROUNDS_CEILING + 1);
    for n in [1u16, 2, MAX_ROUNDS_CEILING] {
        // N - 1 and N yields, each followed by a completing round.
        for yields in [n - 1, n] {
            let mut life = session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION_1_1).unwrap();
            for round in 1..=yields {
                life.host(&invoke_round(1, round, n, usize::from(round - 1)))
                    .unwrap();
                life.guest(&yielded_round(1, round, usize::from(round - 1)))
                    .unwrap();
            }
            let last = yields + 1;
            life.host(&invoke_round(1, last, n, usize::from(yields)))
                .unwrap_or_else(|error| panic!("N={n} yields={yields}: {error:?}"));
            life.guest(&completed_round(1, last, usize::from(yields)))
                .unwrap();
        }
        // N + 1 yields: the (N+1)th yield arrives in round N+1, and no round
        // N+2 may follow; only Cancel is valid.
        let mut life = session(&SUPPORTED_ABI_VERSIONS, ABI_VERSION_1_1).unwrap();
        for round in 1..=n + 1 {
            life.host(&invoke_round(1, round, n, usize::from(round - 1)))
                .unwrap();
            life.guest(&yielded_round(1, round, usize::from(round - 1)))
                .unwrap();
        }
        assert_eq!(
            life.clone()
                .host(&invoke_round(1, n + 2, n, usize::from(n + 1))),
            Err(ProtocolError::InvalidTransition),
            "N={n}"
        );
        life.host(&HostMessage::Cancel {
            request_id: RequestId(1),
        })
        .unwrap();
        life.guest(&GuestMessage::Cancelled {
            request_id: RequestId(1),
        })
        .unwrap();
    }
}
