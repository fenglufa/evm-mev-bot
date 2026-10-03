//! §29's six test groups, over rows built the way the recorder builds them.
//!
//! Every row here is assembled by [`evm_chain::describe_call`], the same function
//! `pipeline_call_row` calls when it writes a real row, so a test that passes on a key
//! shape the recorder never writes is not testing this milestone's rule — it is testing a
//! fiction. The consequence is that §4.2's collapse is visible here too: the rows that
//! 「spell one ask two ways」 arrive already spelled one way, which is what
//! `semantic_duplicate = 0` claims, and
//! [`semantic_spelling_collapse_is_a_record_time_fact`] is the measurement of that claim
//! rather than a restatement of it.

use evm_chain::{describe_call, DEDUP_KEY_UNAVAILABLE_FOR_METHOD};
use serde_json::{json, Value};

use super::*;

const CHAIN: u64 = 91_342;
const RUN: &str = "run-unit-001";
const ENDPOINT: &str = "rpc-unit-endpoint";
const POOL_A: &str = "0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e";
const POOL_B: &str = "0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4";
const POOL_A_MIXED_CASE: &str = "0x5B3C1E3FB6A97C0130AE015fF10F53A1A30c353E";
const SLOT_RESERVES: &str = "0x8";
const SLOT_PADDED: &str = "0x0000000000000000000000000000000000000000000000000000000000000008";
const PINNED: &str = "0x23da5df";
const PINNED_DECIMAL: &str = "37594591";
const NEXT_BLOCK: &str = "0x23da5e0";
const GET_RESERVES: &str = "0x0902f1ac";
const TOKEN_0: &str = "0xd06ca61f";
const VIEWER: &str = "views: getReserves()";

/// One published row of `pipeline-calls.json`, with the §12 key the recorder would have
/// written for these params. `rpc_id` and `started_ns` are the only fields a caller has to
/// invent per ask, because §5's direction is read off the clock and nothing else.
///
/// The sink comes from the stage the way the recorder assigns it: the simulation lane
/// stamps `simulation`, every other stage's ask lands on the `lifecycle` sink. Hardcoding
/// one value here would make §9's `eth_call` rule read the wrong side of every row.
fn ask(
    method: &str,
    params: &Value,
    stage: &str,
    caller: &str,
    rpc_id: u64,
    started_ns: u64,
) -> Value {
    let sink = if stage == crate::diagnosis::PIPELINE_SINK_SIMULATION {
        crate::diagnosis::PIPELINE_SINK_SIMULATION
    } else {
        crate::diagnosis::PIPELINE_SINK_LIFECYCLE
    };
    ask_on(sink, method, params, stage, caller, rpc_id, started_ns)
}

/// [`ask`] on a named sink, since §13's pairs run across the two sinks M8.4.1 records.
fn ask_on(
    sink: &str,
    method: &str,
    params: &Value,
    stage: &str,
    caller: &str,
    rpc_id: u64,
    started_ns: u64,
) -> Value {
    let described = describe_call(method, params, Some(CHAIN));
    json!({
        "run": RUN,
        "sink": sink,
        "rpc_id": rpc_id,
        "logical_request_id": format!("req-{rpc_id}"),
        "method": method,
        "stage": stage,
        "caller": caller,
        "context_note": Value::Null,
        "block_tag": described.block,
        "target": described.target,
        "slot": described.slot,
        "started_ns": started_ns,
        "finished_ns": started_ns + 1_000,
        "duration_ns": 1_000,
        "attempt": 1,
        "attempts_recorded": true,
        "attempts": [
            { "started_ns": started_ns, "finished_ns": started_ns + 1_000, "success": true }
        ],
        "success": true,
        "error_class": Value::Null,
        "error_detail": Value::Null,
        "endpoint_id": ENDPOINT,
        "trace_schema": 3,
        "dedup_key": described.dedup_key,
        "key_note": described.key_note,
    })
}

/// The same ask at one storage word, which is the corpus's most repeated read.
fn storage_at(address: &str, slot: &str, block: &str, stage: &str, rpc_id: u64, ns: u64) -> Value {
    ask(
        "eth_getStorageAt",
        &json!([address, slot, block]),
        stage,
        VIEWER,
        rpc_id,
        ns,
    )
}

fn at_pin(slot: &str, rpc_id: u64, ns: u64) -> Value {
    storage_at(POOL_A, slot, PINNED, "opportunity_detection", rpc_id, ns)
}

fn key_of(row: &Value) -> RpcReadKey {
    RpcReadKey::of_row(row, Some(CHAIN))
}

fn analysis_of(rows: &[Value]) -> RunAnalysis {
    RunAnalysis::of(json!({ "run": RUN, "chain_id": CHAIN }), rows)
}

/// The one pair a given set of asks forms, handed over in the order written — so a caller
/// can pass them out of clock order and check that §5's direction comes from the stamps and
/// not from the input.
fn only_pair(rows: &[Value]) -> DuplicatePair {
    let analysis = analysis_of(rows);
    assert_eq!(
        analysis.pairs.len(),
        1,
        "these asks have exactly one repeat between them: {:?}",
        analysis
            .pairs
            .iter()
            .map(|pair| pair.duplicate_class)
            .collect::<Vec<&str>>()
    );
    analysis.pairs.into_iter().next().expect("one pair")
}

fn without(row: &Value, field: &str) -> Value {
    let mut cloned = row.clone();
    if let Some(object) = cloned.as_object_mut() {
        object.remove(field);
    }
    cloned
}

fn with(row: &Value, field: &str, value: Value) -> Value {
    let mut cloned = row.clone();
    if let Some(object) = cloned.as_object_mut() {
        object.insert(field.to_string(), value);
    }
    cloned
}

// ---------------------------------------------------------------------------
// §29 / Canonicalization (five lines, five tests, plus the two this build adds)
// ---------------------------------------------------------------------------

/// §29's first line: the same ask twice is one identity.
#[test]
fn same_request_yields_same_key() {
    let first = at_pin(SLOT_RESERVES, 1, 1_000);
    let second = at_pin(SLOT_RESERVES, 2, 2_000);
    let (a, b) = (key_of(&first), key_of(&second));
    assert_eq!(a.identity_source, IDENTITY_FROM_RECORDED_KEY);
    assert!(a.identity().is_some(), "a keyed ask always has an identity");
    assert_eq!(a.identity(), b.identity());
    assert_eq!(a.identity_without_block(), b.identity_without_block());
    assert!(a.row_matches_key, "the row's own fields agree with its key");
}

/// §29's second line, and the mechanism §4.3 is built on: the identities differ while the
/// targets match, so a difference exists and it has a name.
#[test]
fn different_block_yields_different_key() {
    let first = at_pin(SLOT_RESERVES, 1, 1_000);
    let second = storage_at(
        POOL_A,
        SLOT_RESERVES,
        NEXT_BLOCK,
        "opportunity_detection",
        2,
        2_000,
    );
    let (a, b) = (key_of(&first), key_of(&second));
    assert_ne!(a.identity(), b.identity());
    assert_eq!(a.identity_without_block(), b.identity_without_block());
    assert_eq!(
        a.block.as_deref(),
        Some(PINNED_DECIMAL),
        "a hex height is decimalised at record time, as §12 does"
    );
    assert_eq!(b.block.as_deref(), Some("37594592"));
    assert_eq!(classify_pair(&a, &b), SAME_TARGET_DIFFERENT_BLOCK);
}

/// §29's third line: two pools are two asks at one height, and the address is what says so.
#[test]
fn different_address_yields_different_key() {
    let one = ask(
        "eth_getBalance",
        &json!([POOL_A, PINNED]),
        "simulation",
        "account: sender",
        1,
        1_000,
    );
    let other = ask(
        "eth_getBalance",
        &json!([POOL_B, PINNED]),
        "simulation",
        "account: sender",
        2,
        2_000,
    );
    let (a, b) = (key_of(&one), key_of(&other));
    assert_eq!(a.address.as_deref(), Some(POOL_A));
    assert_ne!(a.identity(), b.identity());
    assert_ne!(a.identity_without_block(), b.identity_without_block());
    assert_eq!(classify_pair(&a, &b), NOT_A_DUPLICATE);
    assert_eq!(analysis_of(&[one, other]).pairs.len(), 0);
}

/// §29's fourth line, in the two forms the live corpus contains: `0x8` against `0x9` are two
/// words, and one word written two ways is one word.
#[test]
fn different_slot_yields_different_key() {
    let eight = at_pin(SLOT_RESERVES, 1, 1_000);
    let nine = at_pin("0x9", 2, 2_000);
    let (a, b) = (key_of(&eight), key_of(&nine));
    assert_ne!(a.identity(), b.identity());
    assert_eq!(classify_pair(&a, &b), NOT_A_DUPLICATE);
    assert_eq!(a.slot.as_deref(), Some(SLOT_PADDED));

    let padded = at_pin(SLOT_PADDED, 3, 3_000);
    let c = key_of(&padded);
    assert_eq!(
        a.identity(),
        c.identity(),
        "one storage word written two ways is one ask: §12 zero-pads it before the row is \
         written, so this module never sees the second spelling"
    );
}

/// §29's fifth line, and §23's reason for existing: `latest` is not a height, so a tag
/// against the pin is neither a match nor a proven difference.
#[test]
fn latest_and_pinned_block_yield_different_key() {
    let pinned = at_pin(SLOT_RESERVES, 1, 1_000);
    let head = storage_at(
        POOL_A,
        SLOT_RESERVES,
        "latest",
        "opportunity_detection",
        2,
        2_000,
    );
    let (a, b) = (key_of(&pinned), key_of(&head));
    assert_ne!(a.identity(), b.identity());
    assert_eq!(a.block_form, BLOCK_FORM_NUMBER);
    assert_eq!(b.block_form, BLOCK_FORM_TAG);
    assert_eq!(b.block.as_deref(), Some("latest"));
    let relation = RpcReadKey::same_block(&a, &b);
    assert_eq!(relation, SameBlock::TagAgainstHeight);
    assert_eq!(
        relation.figure(),
        None,
        "§5 forbids writing a tag against a height as either true or false"
    );
    assert_eq!(classify_pair(&a, &b), SAME_TARGET_BLOCK_UNDETERMINED);

    let pending = storage_at(
        POOL_A,
        SLOT_RESERVES,
        "pending",
        "opportunity_detection",
        3,
        3_000,
    );
    let c = key_of(&pending);
    assert_eq!(RpcReadKey::same_block(&b, &c), SameBlock::DifferentTags);
    assert_eq!(classify_pair(&b, &c), SAME_METHOD_DIFFERENT_SEMANTICS);
}

/// §4.2's whole finding, measured: the collapse the task book asks us to look for has
/// already happened by the time a row exists, so the pair is exact and §4.2's class is the
/// one this build reports as empty. A test that manufactured a `semantic_duplicate` pair
/// would be testing a record shape the recorder does not produce.
#[test]
fn semantic_spelling_collapse_is_a_record_time_fact() {
    let mixed_case = ask(
        "eth_getStorageAt",
        &json!([POOL_A_MIXED_CASE, SLOT_PADDED, PINNED_DECIMAL]),
        "opportunity_detection",
        VIEWER,
        1,
        1_000,
    );
    let canonical = at_pin(SLOT_RESERVES, 2, 2_000);
    let (a, b) = (key_of(&mixed_case), key_of(&canonical));
    assert_eq!(
        a.identity(),
        b.identity(),
        "address case and a decimal height are the same ask: the normalization happened \
         before the row was written"
    );
    assert_eq!(classify_pair(&a, &b), DUPLICATE_EXACT);
    assert_eq!(classify_pair(&b, &a), DUPLICATE_EXACT);
    assert_ne!(classify_pair(&a, &b), DUPLICATE_SEMANTIC);
    assert_ne!(classify_pair(&b, &a), DUPLICATE_SEMANTIC);
    assert_eq!(
        DUPLICATE_CLASSES
            .iter()
            .filter(|c| **c == DUPLICATE_SEMANTIC)
            .count(),
        1,
        "the class stays named in the published list, because the summary reports a zero \
         against it rather than against an absent rule"
    );
}

/// §26's guard on this module's own input: a row is a request, and an ask §12 could not key
/// has no identity to compare rather than a defaulted one.
#[test]
fn identity_only_ever_comes_from_a_recorded_request() {
    let unkeyed = ask(
        "eth_getLogs",
        &json!([{ "fromBlock": PINNED, "toBlock": PINNED }]),
        "observation",
        "logs: pool events",
        1,
        1_000,
    );
    let key = key_of(&unkeyed);
    assert_eq!(key.identity_source, IDENTITY_UNAVAILABLE);
    assert_eq!(
        key.identity_note.as_deref(),
        Some(DEDUP_KEY_UNAVAILABLE_FOR_METHOD)
    );
    assert_eq!(key.identity(), None);
    assert!(
        key.is_physical_request(),
        "it was asked, so it was recorded"
    );

    assert!(
        !key_of(&without(&unkeyed, "logical_request_id")).is_physical_request(),
        "a row with no logical request is not an ask this module may count"
    );
}

// ---------------------------------------------------------------------------
// §29 / Duplicate classification
// ---------------------------------------------------------------------------

/// §5's direction, tested by handing the rows over in the wrong order: the producer is the
/// earlier stamp, not the earlier line of the input.
#[test]
fn exact_duplicate_is_directional_and_cross_stage() {
    let early = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "opportunity_detection",
        "reserves of both venues, at the pin",
        1,
        1_000,
    );
    let late = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "preflight",
        "reserves of leg 0",
        2,
        9_000,
    );
    let pair = only_pair(&[late, early]);
    assert_eq!(pair.duplicate_class, DUPLICATE_EXACT);
    assert!(pair.is_duplicate());
    assert_eq!(pair.scope, SCOPE_CROSS_STAGE);
    assert_eq!(pair.producer.rpc_id, Some(1));
    assert_eq!(pair.consumer.rpc_id, Some(2));
    assert_eq!(pair.same_block, SameBlock::Yes);
    assert_eq!(pair.same_block_figure(), Some(true));
}

/// §17's separation, in the direction that matters: an intra-stage repeat must not be able
/// to read as the cross-stage finding this milestone is about.
#[test]
fn same_stage_repeat_is_intra_stage_not_cross_stage() {
    let first = at_pin(SLOT_RESERVES, 1, 1_000);
    let second = at_pin(SLOT_RESERVES, 2, 2_000);
    let other_stage = storage_at(POOL_A, SLOT_RESERVES, PINNED, "simulation", 3, 3_000);
    let (a, b, c) = (key_of(&first), key_of(&second), key_of(&other_stage));
    assert_eq!(pair_scope(&a, &b), SCOPE_INTRA_STAGE);
    assert_eq!(pair_scope(&a, &c), SCOPE_CROSS_STAGE);

    let pair = only_pair(&[first, second]);
    assert_eq!(pair.duplicate_class, DUPLICATE_EXACT);
    assert_eq!(pair.scope, SCOPE_INTRA_STAGE);
    let counts = analysis_of(&[
        storage_at(
            POOL_A,
            SLOT_RESERVES,
            PINNED,
            "opportunity_detection",
            1,
            1_000,
        ),
        storage_at(
            POOL_A,
            SLOT_RESERVES,
            PINNED,
            "opportunity_detection",
            2,
            2_000,
        ),
    ])
    .counts();
    assert_eq!(counts.get(&(DUPLICATE_EXACT, SCOPE_INTRA_STAGE)), Some(&1));
    assert_eq!(
        counts.get(&(DUPLICATE_EXACT, SCOPE_CROSS_STAGE)),
        None,
        "an intra-stage pair cannot appear in the cross-stage tally"
    );
}

/// §25, on the two shapes a retry can arrive in: one row that tried three times, and two
/// rows that share one logical request. Neither is a duplicate.
#[test]
fn retry_is_not_a_duplicate() {
    let retried = {
        let row = at_pin(SLOT_RESERVES, 1, 1_000);
        with(
            &with(
                &row,
                "attempts",
                json!([
                    { "started_ns": 1_000, "finished_ns": 1_500, "success": false },
                    { "started_ns": 1_600, "finished_ns": 2_100, "success": false },
                    { "started_ns": 2_200, "finished_ns": 2_900, "success": true }
                ]),
            ),
            "attempt",
            json!(3),
        )
    };
    let analysis = analysis_of(std::slice::from_ref(&retried));
    assert!(analysis.pairs.is_empty(), "one ask forms no pair");
    assert_eq!(analysis.asks, 1);
    assert_eq!(
        analysis.rows_with_more_than_one_try, 1,
        "the three tries are recorded beside the row, not as three rows"
    );

    let second_try = with(&retried, "rpc_id", json!(2));
    let pair = only_pair(&[retried, second_try]);
    assert_eq!(pair.scope, SCOPE_SAME_LOGICAL_REQUEST);
    assert_ne!(pair.scope, SCOPE_CROSS_STAGE);
    assert_ne!(pair.scope, SCOPE_INTRA_STAGE);
}

/// A pair that differs in a term other than the block is not a duplicate, and the case §4
/// does not name is reported as unnamed rather than rounded either way.
#[test]
fn different_parameter_is_not_a_duplicate_and_undetermined_block_is_not_either() {
    let data_a = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "preflight",
        "reserves of leg 0",
        1,
        1_000,
    );
    let data_b = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": TOKEN_0 }, PINNED]),
        "preflight",
        "reserves of leg 1",
        2,
        2_000,
    );
    let (a, b) = (key_of(&data_a), key_of(&data_b));
    assert_eq!(classify_pair(&a, &b), NOT_A_DUPLICATE);
    assert_eq!(
        analysis_of(&[data_a, data_b]).pairs.len(),
        0,
        "two different calls are two different targets, so they form no pair at all"
    );

    let pair = only_pair(&[
        at_pin(SLOT_RESERVES, 3, 3_000),
        storage_at(
            POOL_A,
            SLOT_RESERVES,
            "latest",
            "opportunity_detection",
            4,
            4_000,
        ),
    ]);
    assert_eq!(pair.duplicate_class, SAME_TARGET_BLOCK_UNDETERMINED);
    assert!(!pair.is_duplicate());
    assert_eq!(pair.verdict, ReuseVerdict::NotACandidate);
    assert_eq!(pair.reusable(), "no");
    assert_eq!(pair.same_block_figure(), None);
    assert_eq!(pair.to_json()["safe_to_reuse"], json!(false));
}

/// §9's categories, including the two the task book names by hand: `eth_chainId` is not a
/// state read, and an `eth_call` is judged by what it was asked for.
#[test]
fn read_categories_follow_the_ask_not_the_method_name() {
    let category_of = |row: &Value| key_of(row).category;
    let chain_id = ask(
        "eth_chainId",
        &json!([]),
        "observation",
        "connect: which chain answers",
        1,
        1_000,
    );
    let reserves = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "opportunity_detection",
        "reserves of both venues, at the pin",
        2,
        2_000,
    );
    let ceiling = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "preflight",
        "cost ceiling per step",
        3,
        3_000,
    );
    let evm_call = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "simulation",
        "execute: step 4 (nonce 10)",
        4,
        4_000,
    );
    let balance = ask(
        "eth_getBalance",
        &json!([POOL_A, PINNED]),
        "simulation",
        "account: sender",
        5,
        5_000,
    );
    let nonce = ask(
        "eth_getTransactionCount",
        &json!([POOL_A, "pending"]),
        "preflight",
        "pending and latest nonces",
        6,
        6_000,
    );
    let fee = ask(
        "eth_maxPriorityFeePerGas",
        &json!([]),
        "build",
        "fee: per step",
        7,
        7_000,
    );

    assert_eq!(category_of(&chain_id), READ_CATEGORY_CHAIN_IDENTITY);
    assert_eq!(category_of(&reserves), READ_CATEGORY_STATE_READ);
    assert_eq!(category_of(&ceiling), READ_CATEGORY_TRANSACTION_PREPARATION);
    assert_eq!(category_of(&evm_call), READ_CATEGORY_SIMULATION_CALL);
    assert_eq!(category_of(&balance), READ_CATEGORY_STATE_READ);
    assert_eq!(category_of(&nonce), READ_CATEGORY_TRANSACTION_PREPARATION);
    assert_eq!(category_of(&fee), READ_CATEGORY_TRANSACTION_PREPARATION);
    for row in &[
        &chain_id, &reserves, &ceiling, &evm_call, &balance, &nonce, &fee,
    ] {
        let key = key_of(row);
        assert!(
            READ_CATEGORIES.contains(&key.category),
            "a category outside §9's six: {}",
            key.category
        );
        assert!(
            !key.category_rule.is_empty(),
            "every category carries the rule that produced it"
        );
    }

    // The one addition this milestone declares, and the reason it needs declaring: §12 keys
    // no parameterless method, so without it these asks have no identity at all.
    let a = key_of(&chain_id);
    let b = key_of(&ask(
        "eth_chainId",
        &json!([]),
        "preflight",
        "connect: which chain answers",
        8,
        8_000,
    ));
    assert_eq!(a.identity_source, IDENTITY_FROM_PARAMLESS_REQUEST);
    assert_eq!(a.identity(), b.identity());
    assert_eq!(classify_pair(&a, &b), DUPLICATE_EXACT);
    assert_eq!(pair_scope(&a, &b), SCOPE_CROSS_STAGE);
    assert_eq!(
        a.block_form, BLOCK_FORM_ABSENT,
        "a chain id read names no block, so §23 has nothing to compare"
    );
}

// ---------------------------------------------------------------------------
// §29 / Reuse classification
// ---------------------------------------------------------------------------

/// `duplicate != reusable`: an exact duplicate at one height on one endpoint still has a
/// condition this build cannot check, so level B is `unknown` and not `yes`.
#[test]
fn duplicate_is_not_reusable() {
    let first = at_pin(SLOT_RESERVES, 1, 1_000);
    let second = storage_at(POOL_A, SLOT_RESERVES, PINNED, "simulation", 2, 2_000);
    let pair = only_pair(&[first, second]);
    assert_eq!(pair.duplicate_class, DUPLICATE_EXACT);
    assert!(pair.is_duplicate());
    assert_eq!(pair.reusable(), "unknown");
    assert!(!pair.verdict.counts_as_safe());
    assert_eq!(pair.verdict, ReuseVerdict::CandidateReuseUnknown);

    let unmet = pair
        .conditions
        .iter()
        .find(|condition| condition.condition == CONDITION_LIFECYCLE_OWNERSHIP)
        .expect("every condition is listed, including the uncheckable ones");
    assert_eq!(unmet.outcome, NOT_CHECKABLE_FROM_A_RECORD);
    assert_eq!(
        pair.first_unmet_condition()
            .map(|condition| condition.condition),
        Some(CONDITION_LIFECYCLE_OWNERSHIP),
        "the two checkable conditions are met for this pair, so the first that is not is \
         the one this build cannot read"
    );
    let row = pair.to_json();
    assert_eq!(
        row["conditions"].as_array().map(Vec::len),
        Some(REUSE_CONDITIONS.len())
    );
    assert_eq!(row["reusable"], json!("unknown"));
}

/// `reusable != safe_to_reuse`, both ways at once: the level is computed from the condition
/// list, so a pair whose conditions all resolve to met does reach
/// [`ReuseVerdict::SafeToReuse`] — and §23's refusal is a different verdict again, not the
/// same one arrived at by a longer route.
#[test]
fn reusable_is_not_safe_to_reuse() {
    let met: Vec<ConditionOutcome> = REUSE_CONDITIONS
        .iter()
        .map(|condition| ConditionOutcome {
            condition,
            outcome: CHECKED_MET,
            reason: "fed as met by this test".to_string(),
        })
        .collect();
    assert_eq!(
        reuse_verdict(DUPLICATE_EXACT, SameBlock::Yes, &met),
        ReuseVerdict::SafeToReuse,
        "the rule is a fold over the conditions, not a constant false"
    );
    assert!(ReuseVerdict::SafeToReuse.counts_as_safe());
    assert!(ReuseVerdict::SafeToReuse.counts_as_candidate());

    let one_unmet = {
        let mut list = met.clone();
        list[0].outcome = CHECKABLE_NOT_MET;
        list
    };
    let unknown = reuse_verdict(DUPLICATE_EXACT, SameBlock::Yes, &one_unmet);
    assert_eq!(unknown, ReuseVerdict::CandidateReuseUnknown);
    assert!(!unknown.counts_as_safe(), "a candidate is not a safe one");
    assert!(unknown.counts_as_candidate());

    // §23's own refusal, on the live shape of the case: one target, two heights.
    let pair = only_pair(&[
        at_pin(SLOT_RESERVES, 1, 1_000),
        storage_at(POOL_A, SLOT_RESERVES, NEXT_BLOCK, "simulation", 2, 2_000),
    ]);
    assert_eq!(pair.duplicate_class, SAME_TARGET_DIFFERENT_BLOCK);
    assert!(pair.is_duplicate(), "§4.3 is a duplicate class");
    assert_eq!(pair.verdict, ReuseVerdict::RefusedByBlockIdentity);
    assert_eq!(pair.reusable(), "no");
    assert_eq!(pair.same_block_figure(), Some(false));
    assert!(!pair.verdict.counts_as_candidate());
    assert!(!pair.verdict.counts_as_safe());
    let reason = pair.to_json()["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        reason.contains("different block identity")
            && reason.contains(CONDITION_SAME_BLOCK_REQUIRED),
        "§23 asks the refusal to carry its reason: {reason}"
    );
}

/// A consumer that asks a tag has stated a freshness requirement, and §7 says matching data
/// is then not permission. A consumer that names a height has not stated the absence of one.
#[test]
fn freshness_requirement_is_read_off_the_consumer() {
    let pair = only_pair(&[
        at_pin(SLOT_RESERVES, 1, 1_000),
        storage_at(POOL_A, SLOT_RESERVES, PINNED, "simulation", 2, 2_000),
    ]);
    let freshness = pair
        .conditions
        .iter()
        .find(|condition| condition.condition == CONDITION_NO_FRESHNESS_REQUIREMENT)
        .expect("the condition is listed");
    assert_eq!(
        freshness.outcome, NOT_CHECKABLE_FROM_A_RECORD,
        "the consumer names a height, which is weaker than a stated requirement and is not \
         its absence: {}",
        freshness.reason
    );

    let head = storage_at(
        POOL_A,
        SLOT_RESERVES,
        "latest",
        "opportunity_detection",
        3,
        3_000,
    );
    let other = storage_at(POOL_A, SLOT_RESERVES, "latest", "simulation", 4, 4_000);
    let pair = only_pair(&[head, other]);
    assert_eq!(pair.duplicate_class, DUPLICATE_EXACT);
    let freshness = pair
        .conditions
        .iter()
        .find(|condition| condition.condition == CONDITION_NO_FRESHNESS_REQUIREMENT)
        .expect("the condition is listed");
    assert_eq!(freshness.outcome, CHECKABLE_NOT_MET);
    let block_condition = pair
        .conditions
        .iter()
        .find(|condition| condition.condition == CONDITION_SAME_BLOCK_REQUIRED)
        .expect("§23 is a condition of its own");
    assert_eq!(block_condition.outcome, CHECKABLE_NOT_MET);
    assert_eq!(pair.verdict, ReuseVerdict::CandidateReuseUnknown);

    let (a, b) = (
        key_of(&ask(
            "eth_maxPriorityFeePerGas",
            &json!([]),
            "preflight",
            "fee: per step",
            5,
            5_000,
        )),
        key_of(&ask(
            "eth_maxPriorityFeePerGas",
            &json!([]),
            "build",
            "fee: per step",
            6,
            6_000,
        )),
    );
    let conditions = reuse_conditions(&a, &b, DUPLICATE_EXACT, RpcReadKey::same_block(&a, &b));
    assert_eq!(
        conditions
            .iter()
            .find(|condition| condition.condition == CONDITION_NO_FRESHNESS_REQUIREMENT)
            .map(|condition| condition.outcome),
        Some(CHECKABLE_NOT_MET),
        "a read whose answer is a property of the head states its own freshness requirement"
    );
}

/// §6's conditions are never omitted, including for a pair that is not a duplicate: an
/// absent condition would read as a passed one.
#[test]
fn conditions_are_listed_even_when_the_pair_is_not_a_duplicate() {
    let storage = at_pin(SLOT_RESERVES, 1, 1_000);
    let balance = ask(
        "eth_getBalance",
        &json!([POOL_A, PINNED]),
        "simulation",
        "account: sender",
        2,
        2_000,
    );
    let (a, b) = (key_of(&storage), key_of(&balance));
    let same_block = RpcReadKey::same_block(&a, &b);
    let conditions = reuse_conditions(&a, &b, NOT_A_DUPLICATE, same_block);
    assert_eq!(
        conditions
            .iter()
            .map(|condition| condition.condition)
            .collect::<Vec<&str>>(),
        vec![
            CONDITION_SAME_BLOCK_REQUIRED,
            CONDITION_BLOCK_IDENTITY,
            CONDITION_STATE_SEMANTICS,
            CONDITION_LIFECYCLE_OWNERSHIP,
            CONDITION_NO_FRESHNESS_REQUIREMENT,
        ],
        "§23's condition is the one the list opens with, and the other four follow in \
         REUSE_CONDITIONS' order"
    );
    assert_eq!(conditions.len(), REUSE_CONDITIONS.len());
    assert_eq!(
        reuse_verdict(NOT_A_DUPLICATE, same_block, &conditions),
        ReuseVerdict::NotACandidate
    );
}

/// Two providers, two answers: the pair stays a duplicate (§8's key carries no endpoint) and
/// fails §6's second condition, rather than disappearing from the matrix.
#[test]
fn same_ask_on_two_endpoints_is_state_semantics_unmet() {
    let here = at_pin(SLOT_RESERVES, 1, 1_000);
    let elsewhere = with(
        &at_pin(SLOT_RESERVES, 2, 2_000),
        "endpoint_id",
        json!("rpc-other"),
    );
    let (a, b) = (key_of(&here), key_of(&elsewhere));
    assert_eq!(classify_pair(&a, &b), DUPLICATE_EXACT);
    assert!(!same_endpoint(&a, &b));
    let conditions = reuse_conditions(&a, &b, DUPLICATE_EXACT, RpcReadKey::same_block(&a, &b));
    let semantics = conditions
        .iter()
        .find(|condition| condition.condition == CONDITION_STATE_SEMANTICS)
        .expect("the condition is listed");
    assert_eq!(semantics.outcome, CHECKABLE_NOT_MET);
    assert!(
        semantics.reason.contains("rpc-unit-endpoint") && semantics.reason.contains("rpc-other"),
        "the reason names the two endpoints it compared: {}",
        semantics.reason
    );
    assert_eq!(
        reuse_verdict(DUPLICATE_EXACT, SameBlock::Yes, &conditions),
        ReuseVerdict::CandidateReuseUnknown
    );
    assert!(same_endpoint(&a, &key_of(&at_pin(SLOT_RESERVES, 3, 3_000))));
}

// ---------------------------------------------------------------------------
// §29 / Cache isolation, Evidence, No-extra-RPC
// ---------------------------------------------------------------------------

/// §18's rule at the level this module can enforce it: a cache hit has no row, so it has no
/// identity, so the only duplicates these tables count are physical requests. M8.3.1's cache
/// contributes 「cross-stage logical reuse candidate, physical duplicate = false」, and this
/// build has no way to write the first half — which is the point.
#[test]
fn cache_hit_is_not_a_physical_duplicate() {
    let asked = at_pin(SLOT_RESERVES, 1, 1_000);
    let served_from_memory = without(&without(&asked, "logical_request_id"), "dedup_key");
    let key = key_of(&served_from_memory);
    assert!(!key.is_physical_request());
    assert_eq!(key.identity_source, IDENTITY_UNAVAILABLE);
    assert_eq!(key.identity(), None);

    let analysis = analysis_of(&[asked, served_from_memory]);
    assert!(analysis.pairs.is_empty());
    assert_eq!(analysis.asks, 2);
    assert_eq!(analysis.asks_with_identity, 1);
    assert_eq!(analysis.asks_without_identity, 1);
}

/// §14's chain: raw rows → the four tables → the figures a report would quote. The tables
/// agree with each other because they are folds over one pair list, so a figure that drifted
/// from it fails here rather than in a report.
#[test]
fn raw_rows_recompute_the_tables() {
    let rows = vec![
        at_pin(SLOT_RESERVES, 1, 1_000),
        storage_at(POOL_A, SLOT_RESERVES, PINNED, "simulation", 2, 2_000),
        storage_at(
            POOL_A,
            SLOT_RESERVES,
            NEXT_BLOCK,
            "opportunity_detection",
            3,
            3_000,
        ),
        ask(
            "eth_getBalance",
            &json!([POOL_B, PINNED]),
            "build",
            "before-snapshot: native and input-token balances",
            4,
            4_000,
        ),
    ];
    let run = json!({
        "run": RUN,
        "chain_id": CHAIN,
        "source": "unit",
        "block_number": json!(PINNED_DECIMAL),
        "endpoint_id": json!(ENDPOINT),
        "execution_mode": json!("build_only"),
        "git_revision": json!("0000000"),
        "generated_at_unix_ms": json!(1),
        "calls": rows,
    });
    let tables = cross_stage_tables(std::slice::from_ref(&run));
    assert_eq!(
        tables
            .as_object()
            .map(|map| map.keys().cloned().collect::<Vec<String>>())
            .unwrap_or_default()
            .len(),
        4,
        "§14's four files and nothing else"
    );
    let summary = &tables[DUPLICATE_SUMMARY_FILE];
    let candidates = &tables[REUSE_CANDIDATES_FILE];
    let matrix = &tables[DUPLICATE_MATRIX_FILE];
    let stage_rows = &tables[STAGE_PAIRS_FILE];

    assert_eq!(summary["total_asks"], json!(4));
    assert_eq!(summary["asks_with_identity"], json!(4));
    assert_eq!(summary["duplicate_pairs"], json!(2));
    assert_eq!(
        summary["exact_duplicate_pairs"],
        json!(1),
        "one word at one height, asked by two stages — and nothing else"
    );
    assert_eq!(summary["same_target_different_block_pairs"], json!(1));
    assert_eq!(summary["semantic_duplicate_pairs"], json!(0));
    assert_eq!(summary["same_method_different_semantics_pairs"], json!(0));
    assert_eq!(summary["safe_reuse_candidates"], json!(0));
    assert_eq!(summary["reuse_candidates"], candidates["candidates"]);
    assert_eq!(summary["duplicate_pairs"], candidates["pairs"]);
    assert_eq!(summary["duplicate_pairs"], matrix["totals"]["pairs"]);
    assert_eq!(
        summary["cross_stage_pairs"].as_u64().unwrap_or(0)
            + summary["intra_stage_pairs"].as_u64().unwrap_or(0)
            + summary["same_logical_request_pairs"].as_u64().unwrap_or(0),
        summary["duplicate_pairs"],
        "§17's split covers every pair"
    );
    assert_eq!(summary["refused_by_block_identity"], json!(1));
    assert_eq!(summary["unknown_reuse_candidates"], json!(1));

    // aggregate → report: the per-run column is the same fold over one run, so a pooled
    // figure can always be traced to the run that carries it.
    let per_run = summary["per_run"]
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new);
    assert_eq!(per_run.len(), 1);
    assert_eq!(per_run[0]["asks"], json!(4));
    assert_eq!(per_run[0]["pairs"], json!(2));
    assert_eq!(
        per_run
            .iter()
            .map(|row| row["pairs"].as_u64().unwrap_or(0))
            .sum::<u64>(),
        summary["duplicate_pairs"]
    );
    assert_eq!(
        per_run[0]["detail_file"],
        json!(format!("{RUN}/{CROSS_STAGE_RUN_FILE}"))
    );
    assert_eq!(cross_stage_run_table(&run)["pairs"], json!(2));
    assert_eq!(
        run_provenance(&run)["run"],
        json!(RUN),
        "a run's provenance is copied from its own record, not written here"
    );
    assert_eq!(
        stage_rows["rows"].as_array().map(Vec::len),
        Some(NAMED_STAGE_PAIRS.len())
    );
}

/// §13's instruction not to invent data: a pair this build cannot measure is still a row,
/// with the reason it has no number.
#[test]
fn every_named_stage_pair_is_a_row_even_without_a_path() {
    let tables = cross_stage_tables(&[]);
    let rows = tables[STAGE_PAIRS_FILE]["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(Vec::new);
    assert_eq!(rows.len(), NAMED_STAGE_PAIRS.len());
    assert_eq!(rows.len(), 11, "§13 names eleven pairs");
    for row in &rows {
        let status = row["status"].as_str().unwrap_or_default();
        assert!(
            [
                PAIR_MEASURED,
                PAIR_NO_DUPLICATES,
                PAIR_NO_ASKS_OBSERVED,
                PAIR_STAGE_ABSENT
            ]
            .contains(&status),
            "an unpublished status would read as a missing row: {row}"
        );
        assert!(
            row["why"].is_string(),
            "every row carries its reason, including a measured zero: {row}"
        );
    }
    let absent = rows
        .iter()
        .filter(|row| row["status"] == json!(PAIR_STAGE_ABSENT))
        .count();
    assert_eq!(
        absent, 3,
        "the two `opportunity` rows and the execution-preparation row are this build's \
         not_applicable cases"
    );
}

/// §26's rule at the reader's end: analysing rows is a fold over the rows given. The
/// end-to-end half of this gate — an instrumented run against a baseline run, same RPC
/// count — lives in `tests/cross_stage_evidence.rs`, because only a run can answer it.
#[test]
fn analysis_adds_no_ask_and_invents_no_row() {
    let rows = vec![
        at_pin(SLOT_RESERVES, 1, 1_000),
        storage_at(
            POOL_A,
            SLOT_RESERVES,
            NEXT_BLOCK,
            "opportunity_detection",
            2,
            2_000,
        ),
        ask(
            "eth_getCode",
            &json!([POOL_A, PINNED]),
            "simulation",
            "codes: touched_contracts",
            3,
            3_000,
        ),
    ];
    let analysis = analysis_of(&rows);
    assert_eq!(analysis.asks, rows.len(), "one input row is one ask");
    assert_eq!(
        analysis.categories.values().sum::<usize>(),
        rows.len(),
        "every ask lands in exactly one §9 category"
    );
    assert_eq!(
        analysis.asks_by_stage.values().sum::<usize>(),
        rows.len(),
        "and in exactly one stage row, including the asks that have no identity"
    );
    let ids: Vec<u64> = rows
        .iter()
        .filter_map(|row| row["rpc_id"].as_u64())
        .collect();
    for pair in &analysis.pairs {
        assert!(
            ids.contains(&pair.producer.rpc_id.unwrap_or_default())
                && ids.contains(&pair.consumer.rpc_id.unwrap_or_default()),
            "both sides of a pair are rows that were given: {pair:?}"
        );
    }
    assert_eq!(
        analysis.pairs.len(),
        analysis.counts().values().sum::<usize>(),
        "the tally is a fold over the pairs, so a dropped or doubled pair would show"
    );
    assert_eq!(
        analysis.target_groups, analysis.first_asks,
        "one target group is one first ask, by the rule's own definition"
    );
    assert_eq!(
        analysis.asks_with_identity + analysis.asks_without_identity,
        analysis.asks,
        "every ask is counted on exactly one side of the identity line"
    );
    assert!(
        analysis.identity_groups >= analysis.target_groups,
        "a target group holds one or more identities, never fewer: {} over {}",
        analysis.identity_groups,
        analysis.target_groups
    );
}

/// §15's row schema: a candidate names its two rows, its two blocks, the identity that made
/// them one ask, and the verdicts as three separate fields.
#[test]
fn candidate_rows_name_their_two_rows() {
    let producer = at_pin(SLOT_RESERVES, 1, 1_000);
    let consumer = storage_at(POOL_A, SLOT_RESERVES, PINNED, "simulation", 2, 2_000);
    let row = only_pair(&[producer, consumer]).to_json();
    assert_eq!(row["run"], json!(RUN));
    assert_eq!(row["candidate_id"], json!("run-unit-001:rpc2"));
    assert_eq!(row["producer"]["rpc_id"], json!(1));
    assert_eq!(row["consumer"]["rpc_id"], json!(2));
    assert_eq!(row["producer"]["stage"], json!("opportunity_detection"));
    assert_eq!(row["consumer"]["stage"], json!("simulation"));
    assert_eq!(row["producer"]["category"], json!(READ_CATEGORY_STATE_READ));
    assert_eq!(row["duplicate"], json!(true));
    assert_eq!(row["duplicate_type"], json!(DUPLICATE_EXACT));
    assert_eq!(row["reusable"], json!("unknown"));
    assert_eq!(row["safe_to_reuse"], json!(false));
    assert_eq!(row["same_block"], json!(true));
    assert_eq!(row["block_relation"], json!("same_block"));
    assert_eq!(row["producer_block"], json!(PINNED_DECIMAL));
    assert_eq!(row["consumer_block"], json!(PINNED_DECIMAL));
    assert_eq!(row["producer_block_form"], json!(BLOCK_FORM_NUMBER));
    assert_eq!(row["identity"]["address"], json!(POOL_A));
    assert_eq!(row["identity"]["slot"], json!(SLOT_PADDED));
    assert_eq!(row["identity"]["block_number"], json!(PINNED_DECIMAL));
    assert_eq!(row["identity"]["block_tag"], Value::Null);
    assert_eq!(row["identity"]["chain_id"], json!(CHAIN));
    assert_eq!(row["scope"], json!(SCOPE_CROSS_STAGE));
}

/// A row whose fields disagree with its own key is reported as a disagreement, not averaged
/// into an identity: two sources for one term is how a table gets a figure no field supports.
#[test]
fn a_row_that_disagrees_with_its_key_is_counted_not_resolved() {
    let honest = at_pin(SLOT_RESERVES, 1, 1_000);
    let lying = with(&honest, "target", json!(POOL_B));
    assert!(key_of(&honest).row_matches_key);
    let key = key_of(&lying);
    assert!(!key.row_matches_key);
    assert_eq!(
        key.address.as_deref(),
        Some(POOL_B),
        "the row's own target field is what §15 publishes as `address`…"
    );
    assert!(
        key.identity()
            .unwrap_or_default()
            .contains(&format!("address={POOL_A}")),
        "…while the identity comes from the recorded key, which still names the first pool"
    );
    let analysis = analysis_of(&[honest, lying]);
    assert_eq!(analysis.rows_disagreeing_with_key, 1);
}

/// The `eth_call` target is the contract being asked, and §15's identity block has to name
/// it: a read of `address` that came back empty for 76 of this corpus's rows would look like
/// a contract read with no contract.
#[test]
fn eth_call_identity_names_the_contract_it_asked() {
    let call = ask(
        "eth_call",
        &json!([{ "to": POOL_A, "data": GET_RESERVES }, PINNED]),
        "opportunity_detection",
        "reserves of both venues, at the pin",
        1,
        1_000,
    );
    let key = key_of(&call);
    assert_eq!(key.address.as_deref(), Some(POOL_A));
    assert_eq!(key.terms.get("to").map(String::as_str), Some(POOL_A));
    assert_eq!(key.identity_source, IDENTITY_FROM_RECORDED_KEY);
    assert_eq!(identity_json(&key)["address"], json!(POOL_A));
}

/// §8's two block fields against the one term the record carries: whichever form the ask
/// used fills its own half, and the other stays null rather than a copy.
#[test]
fn a_tag_fills_the_tag_field_and_a_height_the_number_field() {
    let height = key_of(&at_pin(SLOT_RESERVES, 1, 1_000));
    let tag = key_of(&storage_at(
        POOL_A,
        SLOT_RESERVES,
        "finalized",
        "opportunity_detection",
        2,
        2_000,
    ));
    assert_eq!(
        identity_json(&height)["block_number"],
        json!(PINNED_DECIMAL)
    );
    assert_eq!(identity_json(&height)["block_tag"], Value::Null);
    assert_eq!(identity_json(&tag)["block_tag"], json!("finalized"));
    assert_eq!(identity_json(&tag)["block_number"], Value::Null);
}
