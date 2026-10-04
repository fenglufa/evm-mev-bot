//! §12's fifteen decision-rule tests.
//!
//! Each one changes exactly one condition and asserts which tier moved, because §12's point is
//! that the four tiers are four different questions: a test that only asserted
//! `safe_to_reuse_now == false` everywhere would pass for the wrong reason. The favourable case
//! is [`ReuseEvidence::default`] — one ask, one height, one hash-verified block, one component,
//! one lifecycle, no detected change in between — so a refusal always names something real
//! rather than an unset field.
//!
//! Nothing here runs a simulation or a network: these are the rules the evidence gate applies,
//! tested against declarations, which is what §14 asks for («测试应验证判定规则»).

use super::*;

/// A measured header pair, preflight → build, the most common shape in M8.4.2's 42.
fn measured_header() -> MeasuredCandidate {
    MeasuredCandidate {
        run: "route-91342-37700740-1791045857463".to_string(),
        candidate_id: "route-91342-37700740-1791045857463:rpc1".to_string(),
        method: "eth_getBlockByNumber".to_string(),
        category: "block_read".to_string(),
        scope: "cross_stage".to_string(),
        producer_caller: Some("head and header that fixed the pin".to_string()),
        consumer_caller: Some("step 1: gate — block binding at pin".to_string()),
        producer_stage: Some("preflight".to_string()),
        consumer_stage: "build".to_string(),
        duplicate_type: "exact_duplicate".to_string(),
        block_relation: "same_block".to_string(),
        producer_block_form: "number".to_string(),
        consumer_block_form: "number".to_string(),
        producer_block: Some("37700740".to_string()),
        consumer_block: Some("37700740".to_string()),
        record_reusable: "unknown".to_string(),
        record_safe_to_reuse: false,
    }
}

#[test]
fn every_category_is_declared_once() {
    let kinds = contracts().iter().map(|c| c.kind).collect::<Vec<_>>();
    assert_eq!(kinds.len(), ALL_KINDS.len());
    for kind in ALL_KINDS {
        assert_eq!(
            kinds.iter().filter(|declared| **declared == kind).count(),
            1,
            "{kind:?} is not declared exactly once"
        );
    }
}

/// §12.1 — same chain, same block, same state identity. Identity passes; reuse still does not,
/// and the refusal names the ownership condition rather than an identity one.
#[test]
fn one_chain_one_block_one_identity_is_necessary_and_not_sufficient() {
    let evidence = ReuseEvidence::default();
    for kind in ALL_KINDS {
        let assessment = assess(kind, &evidence);
        assert_eq!(
            assessment.semantically_equivalent,
            Some(true),
            "{kind:?}: the identity tier should be answered positively by default evidence, \
             otherwise this test proves nothing about ownership"
        );
        assert!(
            !assessment.safe_to_reuse_now,
            "{kind:?}: identical state identity must not by itself make a category safe"
        );
        assert!(
            !assessment.blockers.is_empty(),
            "{kind:?}: a refusal without a named condition is the §10 failure mode"
        );
        assert!(
            assessment
                .blockers
                .iter()
                .any(|blocker| blocker.starts_with("contract_")
                    || blocker.starts_with("ownership_")
                    || blocker.starts_with("freshness_")
                    || blocker.starts_with("invalidation_")
                    || blocker.starts_with("no_owner")),
            "{kind:?}: with identity, source and lifecycle all answered positively, the refusal              can only come from the declaration, so a blocker from elsewhere would mean the              default evidence is not as favourable as it claims"
        );
    }
}

/// §12.2 — same height, different hash: a positive identity claim dies, and the difference is
/// recorded as a measured negative rather than as silence.
#[test]
fn same_height_with_a_different_hash_is_a_negative_identity_claim() {
    let evidence = ReuseEvidence {
        same_block_hash: Some(false),
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::BlockHeader, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(false));
    assert_eq!(assessment.reusable_in_principle, Some(false));
    assert!(!assessment.safe_to_reuse_now);
    assert!(assessment
        .blockers
        .contains(&Blocker::BlockHashDiffers.as_str()));

    // The absence of a hash is a different refusal from a hash that disagrees, and the tiers
    // keep them apart: one is a measured negative, the other is a question the record cannot
    // answer.
    let unrecorded = ReuseEvidence {
        same_block_hash: None,
        ..ReuseEvidence::default()
    };
    let unrecorded = assess(StateKind::BlockHeader, &unrecorded);
    assert_eq!(unrecorded.semantically_equivalent, None);
    assert!(unrecorded
        .blockers
        .contains(&Blocker::BlockHashNotRecorded.as_str()));
    assert!(!unrecorded.safe_to_reuse_now);
}

/// §12.3 — same target, different block: not one state, so the second tier is a flat `false`.
#[test]
fn same_target_at_two_heights_is_not_one_state() {
    let evidence = ReuseEvidence {
        same_block_number: Some(false),
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::Nonce, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::BlockNumberDiffers.as_str()));
    assert!(!assessment.safe_to_reuse_now);

    // The measured form of the same case, through the row translator.
    let mut measured = measured_header();
    measured.block_relation = "different_block".to_string();
    measured.consumer_block = Some("37700741".to_string());
    let assessed = assess_measured(&measured).expect("a header row maps to a category");
    assert_eq!(assessed.assessment.semantically_equivalent, Some(false));
    assert!(assessed
        .assessment
        .blockers
        .contains(&"block_number_differs"));
}

/// §12.4 — `latest` against an explicit height: a tag names whichever head the node held, so
/// the identity tier can never be answered positively from it.
#[test]
fn latest_is_not_an_explicit_height() {
    let evidence = ReuseEvidence {
        producer_term: BlockTerm::Latest,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::FeeParameters, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::ProducerNamesNoHeight.as_str()));
    assert!(!assessment.safe_to_reuse_now);
    assert!(!BlockTerm::Latest.names_a_height());
    assert_eq!(
        BlockTerm::from_record("tag", Some("latest")),
        BlockTerm::Latest,
        "the record's raw term is what distinguishes the two tags, so the mapping is part of \
         the rule"
    );
}

/// §12.5 — `pending` against an explicit height, kept a separate test because §5 lists it as a
/// separate semantic: a pending head includes transactions in no canonical block yet.
#[test]
fn pending_is_not_an_explicit_height() {
    let evidence = ReuseEvidence {
        consumer_term: BlockTerm::Pending,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::Nonce, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::ConsumerNamesNoHeight.as_str()));
    assert_eq!(
        BlockTerm::from_record("tag", Some("pending")),
        BlockTerm::Pending
    );

    // An unrecognised tag is not quietly read as `latest`: it is a tag, and a tag cannot name a
    // height, so the refusal is the same shape with a different word.
    assert_eq!(
        BlockTerm::from_record("tag", Some("earliest")),
        BlockTerm::Unknown
    );
    assert!(!BlockTerm::Unknown.names_a_height());
}

/// §12.6 — same value from two components: the identity tier survives (it is about the state,
/// not about who asked), and the ownership tier is where the pair is refused.
#[test]
fn same_value_from_different_producers_is_not_a_shared_state() {
    let evidence = ReuseEvidence {
        same_producing_component: false,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::ContractCode, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(true));
    assert_eq!(assessment.reusable_in_principle, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::SameValueDifferentSource.as_str()));
    assert!(!assessment.safe_to_reuse_now);
}

/// §12.7 — same source, different lifecycle: exactly the StateStore-versus-REVM distinction §7
/// forbids collapsing, and the tiers keep the two cases in different fields.
#[test]
fn same_source_in_two_lifecycles_is_not_reusable() {
    let evidence = ReuseEvidence {
        same_lifecycle_scope: false,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::PoolReserves, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(true));
    assert_eq!(assessment.reusable_in_principle, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::DifferentLifecycleScope.as_str()));
    assert!(!assessment.safe_to_reuse_now);
}

/// §12.8 — the nonce moved after preflight: a measured change is a hard negative, and an
/// unmeasurable one is a refusal with a different name. The build's own re-read is the check
/// this evidence would replace, so `check_would_stop_existing` is tested alongside it.
#[test]
fn a_moved_nonce_after_preflight_refuses_reuse() {
    let moved = ReuseEvidence {
        state_changed_in_between: Some(true),
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::Nonce, &moved);
    assert!(!assessment.safe_to_reuse_now);
    assert!(assessment
        .blockers
        .contains(&Blocker::StateMovedBetweenReads.as_str()));

    let undetectable = ReuseEvidence {
        state_changed_in_between: None,
        ..ReuseEvidence::default()
    };
    let undetectable = assess(StateKind::Nonce, &undetectable);
    assert!(!undetectable.safe_to_reuse_now);
    assert!(undetectable
        .blockers
        .contains(&Blocker::StateMovementNotDetectable.as_str()));

    // §6: a shared value that removes a check is refused even when nothing moved.
    let stops = ReuseEvidence {
        check_would_stop_existing: true,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::Nonce, &stops);
    assert!(!assessment.safe_to_reuse_now);
    assert!(assessment
        .blockers
        .contains(&Blocker::CheckWouldStopExisting.as_str()));
}

/// §12.9 — the balance moved after preflight, the same rule over a different category, because
/// §6 forbids deriving one category's answer from another's.
#[test]
fn a_moved_balance_after_preflight_refuses_reuse() {
    let evidence = ReuseEvidence {
        state_changed_in_between: Some(true),
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::NativeBalance, &evidence);
    assert!(!assessment.safe_to_reuse_now);
    assert!(assessment
        .blockers
        .contains(&Blocker::StateMovedBetweenReads.as_str()));

    // And the two categories are answered separately: whatever the nonce row concludes, the
    // balance row's own declaration is what decides the balance pair.
    let nonce = assess(StateKind::Nonce, &ReuseEvidence::default());
    let balance = assess(StateKind::NativeBalance, &ReuseEvidence::default());
    assert_eq!(nonce.safe_to_reuse_now, balance.safe_to_reuse_now);
    assert_ne!(
        nonce.blockers, balance.blockers,
        "two categories refusing for identical word lists would mean the declarations are not \
         doing any work"
    );
}

/// §12.10 — fee and header freshness. The rule under test is the general one: a category whose
/// declared freshness rule is not proven cannot be safe, and a category whose rule *is* proven
/// is refused for whatever else the declaration names.
#[test]
fn an_unproven_freshness_or_invalidation_rule_alone_refuses_safe_reuse() {
    for kind in ALL_KINDS {
        let contract = contract_for(kind).expect("every category is declared");
        let assessment = assess(kind, &ReuseEvidence::default());
        assert_eq!(
            assessment
                .blockers
                .contains(&Blocker::FreshnessRuleNotProven.as_str()),
            contract.freshness.status != ProofStatus::Proven,
            "{kind:?}: the freshness refusal must track the declared status exactly"
        );
        assert_eq!(
            assessment
                .blockers
                .contains(&Blocker::InvalidationRuleNotProven.as_str()),
            contract.invalidation.status != ProofStatus::Proven,
            "{kind:?}: the invalidation refusal must track the declared status exactly"
        );
        assert_eq!(
            assessment
                .blockers
                .contains(&Blocker::NoOwnerInCode.as_str()),
            contract.owner == Owner::NoOwnerInCode,
            "{kind:?}: a named owner must not be reported as an absent one"
        );
        if !contract.ownership_status.supports_positive_claim()
            || !contract.reuse.missing_proof.is_empty()
            || contract.reuse.check_would_stop_existing
        {
            assert!(
                !assessment.safe_to_reuse_now,
                "{kind:?}: a contract with a listed gap cannot be safe under perfect evidence"
            );
        }
    }
}

/// §12.11 — simulation and build disagree on an input field: the plan is refused at the
/// lifecycle tier even though the state identity is intact.
#[test]
fn simulation_and_build_input_mismatch_refuses_reuse() {
    let evidence = ReuseEvidence {
        sim_and_build_fields_match: false,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::TransactionIntent, &evidence);
    assert_eq!(assessment.semantically_equivalent, Some(true));
    assert_eq!(assessment.reusable_in_principle, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::SimulationBuildFieldsDiffer.as_str()));
    assert!(!assessment.safe_to_reuse_now);
}

/// §12.12 — a run that depended on a state override is not a proof of a real execution
/// environment, so the dependency alone refuses reuse over the simulation result.
#[test]
fn a_state_override_dependence_is_not_reproducible_on_chain() {
    let evidence = ReuseEvidence {
        depends_on_state_override: true,
        ..ReuseEvidence::default()
    };
    let assessment = assess(StateKind::SimulationResult, &evidence);
    assert_eq!(assessment.reusable_in_principle, Some(false));
    assert!(assessment
        .blockers
        .contains(&Blocker::StateOverrideNotReproducible.as_str()));
    assert!(!assessment.safe_to_reuse_now);
}

/// §12.13 — no owner, or no invalidation rule: §10's refusal to judge reuse safe.
#[test]
fn a_missing_owner_refuses_the_safe_tier_by_rule_not_by_wording() {
    let ownerless: Vec<StateKind> = ALL_KINDS
        .iter()
        .copied()
        .filter(|kind| contract_for(*kind).expect("declared").owner == Owner::NoOwnerInCode)
        .collect();
    assert!(
        !ownerless.is_empty(),
        "the model would otherwise never exercise the no-owner branch"
    );
    for kind in ownerless {
        let assessment = assess(kind, &ReuseEvidence::default());
        assert!(
            !assessment.safe_to_reuse_now,
            "{kind:?}: a category with no owner in code cannot be judged safe to reuse"
        );
        assert!(assessment.blockers.contains(&"no_owner_in_code"));
    }
}

/// §12.14 — `unknown` never degrades into `safe_to_reuse_now`: an evidence set full of
/// unanswered questions produces refusals, not a default true, and the two non-positive
/// statuses are refused by the same predicate the declarations use.
#[test]
fn unknown_never_becomes_safe_to_reuse_now() {
    assert!(!ProofStatus::Unknown.supports_positive_claim());
    assert!(!ProofStatus::NotApplicable.supports_positive_claim());
    assert!(ProofStatus::Proven.supports_positive_claim());

    let blind = ReuseEvidence {
        same_value: None,
        same_block_hash: None,
        state_changed_in_between: None,
        ..ReuseEvidence::default()
    };
    for kind in ALL_KINDS {
        let assessment = assess(kind, &blind);
        assert_eq!(
            assessment.semantically_equivalent, None,
            "{kind:?}: an unanswered identity question must stay unknown"
        );
        assert!(!assessment.safe_to_reuse_now);
        assert!(assessment.blockers.contains(&"answer_bytes_not_recorded"));
        assert!(assessment.blockers.contains(&"block_hash_not_recorded"));
    }

    // The measured rows behave the same way: every one of them leaves those three fields
    // unanswered, so none can reach the fourth tier by arithmetic.
    let assessed = assess_measured(&measured_header()).expect("the sample row maps");
    assert_eq!(assessed.evidence.same_value, None);
    assert_eq!(assessed.evidence.same_block_hash, None);
    assert_eq!(assessed.evidence.state_changed_in_between, None);
    assert!(!assessed.assessment.safe_to_reuse_now);
    assert_eq!(
        assessed.assessment.safe_to_reuse_now, assessed.record_safe_to_reuse,
        "this milestone's verdict and M8.4.2's recorded verdict agree on the sample row"
    );
}

/// §12.15 — the aggregates are recomputable: the same rows produce the same tables, and the
/// summary counts are derived from the rows rather than written beside them.
#[test]
fn verdicts_are_deterministic_and_aggregates_recompute_from_rows() {
    let rows = [
        measured_header(),
        MeasuredCandidate {
            candidate_id: "route:rpc2".to_string(),
            method: "eth_chainId".to_string(),
            category: "chain_identity".to_string(),
            producer_block_form: "absent".to_string(),
            consumer_block_form: "absent".to_string(),
            producer_block: None,
            consumer_block: None,
            block_relation: "no_block_in_either_request".to_string(),
            consumer_caller: Some("step 1: gate — endpoint chain id".to_string()),
            ..measured_header()
        },
    ];
    let first: Vec<_> = rows
        .iter()
        .map(|row| serde_json::to_string(&assess_measured(row).unwrap()).unwrap())
        .collect();
    let second: Vec<_> = rows
        .iter()
        .map(|row| serde_json::to_string(&assess_measured(row).unwrap()).unwrap())
        .collect();
    assert_eq!(first, second, "the same rows must produce the same rows");

    // A count over the rows equals the count recomputed from the rows, which is the property
    // the evidence gate checks across files.
    let assessments: Vec<_> = rows
        .iter()
        .map(|row| assess_measured(row).unwrap().assessment)
        .collect();
    let safe = assessments.iter().filter(|a| a.safe_to_reuse_now).count();
    assert_eq!(safe, 0);
    assert_eq!(assessments.len(), rows.len());
    let refused_for_no_height = assessments
        .iter()
        .filter(|a| a.blockers.contains(&"producer_names_no_height"))
        .count();
    assert_eq!(
        refused_for_no_height,
        assessments
            .iter()
            .filter(|a| a.blockers.contains(&"consumer_names_no_height"))
            .count(),
        "an absent-block pair refuses both sides at once, so the two counts cannot disagree"
    );

    // No declared category is safe under the most favourable evidence the build can name: this
    // is §17's decision, stated as an assertion rather than as prose.
    let never_safe = ALL_KINDS
        .iter()
        .copied()
        .filter(|kind| assess(*kind, &ReuseEvidence::default()).safe_to_reuse_now)
        .count();
    assert_eq!(
        never_safe, 0,
        "some declared categories are safe even under the most favourable evidence"
    );
}
