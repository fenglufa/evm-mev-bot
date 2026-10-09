//! M9.4 §47: the twelve deterministic fixtures, one per named payload class.
//!
//! The matrix asks what the radar does across combinations, and the negative controls
//! ask whether the forbidden thing happened. This file is the third artefact the task
//! book names: a fixed, re-derivable payload for each class, carried with all five
//! labels §47 demands (`source`, `capture timestamp`, `chain`, `block`, `protocol
//! version`). A fixture nobody can re-run a week later is not a fixture, so the label
//! is checked as fields rather than as prose, and `the_label_gate_can_fail` proves the
//! check is a gate and not a description.
//!
//! All twelve are CONTROLLED — synthesised by `preconf_support`, on the measured chain
//! id and the committed endpoint digest, and keeping the endpoint's own measured
//! characteristics: an all-zero `stateRoot`, no wire frame index, full transaction
//! objects rather than a hash list. `malformed` and `wrong chain` are the shapes the
//! real capture produced (audit §2), not shapes invented for a convenient assertion.
//!
//! Every fixture runs twice through `twice`, which is §41's claim in miniature: the same
//! payloads give the same event classes in the same order, the same per-height
//! projection, and the same decoder verdicts.

mod preconf_support;

use preconf_support::*;

use serde_json::{json, Value};

use evm_core::BlockNumber;
use evm_live::{
    frame_from_pending_value, AffectedPool, AffectedReason, EarlyRadar, PreconfError, PreconfStage,
    RadarEvent, RadarInput, ReconciliationVerdict,
};

const POOL_A: u8 = 0xa1;
const POOL_B: u8 = 0xa2;
/// An address the pool set does not contain: §47's "unrelated tx".
const FOREIGN: u8 = 0xb7;

/// The twelve class names, in §47's own order and wording.
const CLASSES: [&str; 12] = [
    "valid sequence",
    "duplicate",
    "gap",
    "wrong block",
    "wrong chain",
    "wrong parent",
    "malformed",
    "late flashblock",
    "canonical mismatch",
    "multiple pool changes",
    "reverted tx",
    "unrelated tx",
];

/// One fixture: the payloads it feeds the radar, in order. The third argument is where a
/// fixture records what no event can carry — a decoder verdict, for `malformed` — so the
/// determinism double-run compares those witnesses too rather than losing them.
type Fixture = fn(&mut EarlyRadar, &mut StepClock, &mut Vec<String>) -> Vec<RadarEvent>;

fn once(fixture: Fixture) -> (EarlyRadar, Vec<RadarEvent>, Vec<String>) {
    let mut radar = radar([addr(POOL_A), addr(POOL_B)]);
    let mut clock = StepClock::new(1_000, 5);
    let mut witness = Vec::new();
    let events = fixture(&mut radar, &mut clock, &mut witness);
    (radar, events, witness)
}

/// §41 + §43: replay the fixture on a second fresh radar and require the two runs to
/// agree on everything the radar claims. `record_shape` deliberately excludes the
/// stamps: they are clock data, and the projection is what a table read later returns.
fn twice(fixture: Fixture) -> (EarlyRadar, Vec<RadarEvent>) {
    let (radar, events, witness) = once(fixture);
    let (again, again_events, again_witness) = once(fixture);
    assert_eq!(
        labels(&again_events),
        labels(&events),
        "the replay changed the event order"
    );
    assert_eq!(
        again_witness, witness,
        "the replay changed a decoder witness"
    );
    assert_eq!(
        record_shape(&again),
        record_shape(&radar),
        "the replay changed the per-height projection"
    );
    (radar, events)
}

/// §47's five labels, as fields. Anything missing comes back as a name.
fn label_gaps(meta: &FixtureMeta) -> Vec<&'static str> {
    let mut gaps = Vec::new();
    if meta.name.is_empty() {
        gaps.push("name");
    }
    if meta.source.is_empty() {
        gaps.push("source");
    }
    if meta.captured_at_unix_ms == 0 {
        gaps.push("capture timestamp");
    }
    if meta.chain_id == 0 {
        gaps.push("chain");
    }
    if meta.block_number == 0 {
        gaps.push("block");
    }
    if meta.protocol_version.is_empty() {
        gaps.push("protocol version");
    }
    let json = meta.as_json();
    for key in [
        "fixture",
        "source",
        "capture_timestamp_unix_ms",
        "chain_id",
        "block",
        "protocol_version",
    ] {
        if json.get(key).is_none_or(Value::is_null) {
            gaps.push("serialised label");
        }
    }
    gaps
}

/// Every fixture test starts by labelling itself, so a test that forgot §47 cannot pass.
fn labelled(class: &'static str, block: u64) -> FixtureMeta {
    let meta = FixtureMeta::controlled(class, block);
    assert_eq!(
        label_gaps(&meta),
        Vec::<&'static str>::new(),
        "{class} is not fully labelled"
    );
    assert_eq!(meta.chain_id, CHAIN.0, "{class} is on the measured chain");
    assert_eq!(meta.protocol_version, PROTOCOL_VERSION);
    meta
}

fn decode_class(outcome: &Result<evm_live::PreconfirmationFrame, PreconfError>) -> String {
    match outcome {
        Ok(_) => "accepted".to_string(),
        Err(PreconfError::Decode(field)) => format!("refused:missing_or_bad:{field}"),
        Err(PreconfError::HashOnlyTransaction) => "refused:hash_only_list".to_string(),
        Err(PreconfError::UnexpectedTransactionEntry { kind, .. }) => {
            format!("refused:unexpected_entry:{kind}")
        }
        Err(PreconfError::Transport(_)) | Err(PreconfError::EventQueueClosed { .. }) => {
            "refused:transport".to_string()
        }
    }
}

// — §47's twelve fixtures. —

fn fx_valid_sequence(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    for number in 10..13u64 {
        // The view byte is deliberately offset from `sealed_hash(number)`: a fixture
        // whose view hash equalled the sealed block's hash would make `hash_matched`
        // true by accident, and the claim the field carries would stop being measurable.
        events.extend(offer(
            radar,
            frame_of(number, 0x80 + number as u8, &[POOL_A, POOL_B]),
            clock,
        ));
    }
    events.extend(seal_next(radar, 10, tx_hashes(10, 2), clock));
    events
}

fn fx_duplicate(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(13, 0x13, &[POOL_A]), clock));
    let repeat = frame_of(14, 0x14, &[POOL_A]);
    events.extend(offer(radar, repeat.clone(), clock));
    events.extend(offer(radar, repeat, clock));
    events.extend(offer(radar, frame_of(15, 0x15, &[POOL_A]), clock));
    events
}

fn fx_gap(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(20, 0x20, &[POOL_A]), clock));
    events.extend(offer(radar, frame_of(22, 0x22, &[POOL_A]), clock));
    events
}

/// §9's "block 100 / index 3, block 101 / index 4": while a block's sequence is not
/// known to be complete, the next block's payloads must not be spliced onto it. With no
/// wire index (§2.3), the two checks the transport CAN make are that a height keeps its
/// own transaction list, and that a receipt naming another block is refused.
fn fx_wrong_block(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(100, 0x64, &[POOL_A, POOL_B]), clock));
    let held = radar
        .view(BlockNumber(100))
        .expect("height 100 is held")
        .transaction_hashes
        .clone();
    events.extend(offer(radar, frame_of(101, 0x65, &[POOL_A]), clock));

    // A receipt that names block 99 while being read against height 101.
    let strays = vec![receipt(
        b256(tx_byte(99, 0)),
        99,
        sealed_hash(99),
        Some(1),
        vec![log(addr(POOL_B), b256(tx_byte(99, 0)), 0, 0, false)],
    )];
    witness.push(format!(
        "receipts_wrong_block={}",
        radar.counters().receipts_wrong_block
    ));
    events.extend(offer_with(
        radar,
        frame_of(101, 0x66, &[POOL_A]),
        clock,
        Some(strays),
    ));
    witness.push(format!(
        "receipts_wrong_block={}",
        radar.counters().receipts_wrong_block
    ));
    let after = radar
        .view(BlockNumber(100))
        .expect("still held")
        .transaction_hashes
        .clone();
    assert_eq!(held, after, "the first height kept its own list");
    events
}

fn fx_wrong_chain(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let raw = frame_of(30, 0x1e, &[POOL_A]);
    let observed = clock.tick();
    let sequence = radar.next_local_sequence();
    let foreign = frame_from_pending_value(OTHER_CHAIN, &raw, sequence, observed, ENDPOINT)
        .expect("the same bytes, another chain id");
    let decoded = clock.tick();
    radar.ingest(
        RadarInput {
            frame: foreign,
            receipts: None,
            decoded_at_unix_ms: decoded,
        },
        &mut || clock.tick(),
    )
}

fn fx_wrong_parent(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(10, 0x10, &[POOL_A]), clock));
    events.extend(seal_next(radar, 10, tx_hashes(10, 1), clock));
    events.extend(offer(radar, frame_of(11, 0x11, &[POOL_A]), clock));
    let mut wrong = frame_of(11, 0x19, &[POOL_A]);
    wrong["parentHash"] = Value::String(hex32(0xee));
    events.extend(offer(radar, wrong, clock));
    events
}

fn fx_malformed(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let cases: Vec<(&str, Value)> = vec![
        ("no number", Value::Object(serde_json::Map::new())),
        (
            "number is not a quantity",
            json!({"number": "0xzz", "hash": hex32(1)}),
        ),
        (
            "transactions is a hash-only list",
            json!({"number": "0xa", "transactions": [hex32(1)]}),
        ),
        (
            "transactions is not an array",
            json!({"number": "0xa", "transactions": "0x"}),
        ),
        (
            "payload is not an object",
            Value::Array(vec![json!("pending")]),
        ),
    ];
    for (label, raw) in cases {
        let outcome = frame_from_pending_value(CHAIN, &raw, 1, 1_005, ENDPOINT);
        witness.push(format!("{label} => {}", decode_class(&outcome)));
    }
    // The decoder refusing is not the run ending: the next good payload is held.
    offer(radar, frame_of(40, 0x28, &[POOL_A]), clock)
}

fn fx_late_flashblock(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(10, 0x10, &[POOL_A]), clock));
    events.extend(seal_next(radar, 10, tx_hashes(10, 1), clock));
    events.extend(offer(radar, frame_of(10, 0x77, &[POOL_B]), clock));
    events
}

fn fx_canonical_mismatch(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let mut events = Vec::new();
    events.extend(offer(radar, frame_of(10, 0x10, &[POOL_A, POOL_A]), clock));
    events.extend(seal_next(
        radar,
        10,
        vec![b256(tx_byte(10, 0)), b256(0x99)],
        clock,
    ));
    events
}

fn fx_multiple_pool_changes(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let tx = b256(tx_byte(40, 2));
    let emitter = vec![receipt(
        tx,
        40,
        sealed_hash(40),
        Some(1),
        vec![log(addr(POOL_B), tx, 0, 2, false)],
    )];
    offer_with(
        radar,
        frame_of(40, 0x28, &[POOL_A, POOL_B, POOL_A]),
        clock,
        Some(emitter),
    )
}

fn fx_reverted_tx(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    let tx = b256(tx_byte(10, 0));
    let reverted = receipt(
        tx,
        10,
        sealed_hash(10),
        Some(0),
        vec![log(addr(POOL_A), tx, 0, 0, false)],
    );
    let silent = receipt(
        b256(0x42),
        10,
        sealed_hash(10),
        None,
        vec![log(addr(POOL_A), b256(0x42), 0, 0, false)],
    );
    offer_with(radar, frame_of(10, 0x10, &[0]), clock, Some(vec![reverted]))
        .into_iter()
        .chain(offer_with(
            radar,
            frame_of(10, 0x11, &[0]),
            clock,
            Some(vec![silent]),
        ))
        .collect()
}

fn fx_unrelated_tx(
    radar: &mut EarlyRadar,
    clock: &mut StepClock,
    _witness: &mut Vec<String>,
) -> Vec<RadarEvent> {
    offer(radar, frame_of(50, 0x32, &[FOREIGN, 0, FOREIGN]), clock)
}

// — the twelve tests —

#[test]
fn valid_sequence_is_held_height_by_height_and_closes_on_canonical() {
    let meta = labelled("valid sequence", 10);
    let (radar, events) = twice(fx_valid_sequence);

    assert_eq!(accepted_heights(&events), vec![10, 11, 12]);
    assert!(refusals(&events).is_empty());
    let notes = sequences(&events);
    assert!(
        notes.iter().all(|note| !note.contains("GAP_DETECTED")),
        "contiguous numbers are not a skip: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.contains("affected-pool identity at 10 is UNVERIFIED")),
        "and the closure still says what it could not check: {notes:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RadarEvent::Reconciled {
                verdict: ReconciliationVerdict::ContentEqual,
                hash_matched: false,
                ..
            }
        )),
        "content equal, and the sealed hash is not one of the view's hashes by construction"
    );
    assert_eq!(
        radar.held_heights(),
        vec![BlockNumber(10), BlockNumber(11), BlockNumber(12)]
    );
    assert_eq!(radar.counters().frames_accepted, 3);
    assert_eq!(radar.counters().reconciled_content_equal, 1);
    assert_eq!(
        radar.view(BlockNumber(10)).expect("kept").stage,
        PreconfStage::Reconciled,
        "the sealed height closed, at block {}",
        meta.block_number
    );
    assert_eq!(
        radar.view(BlockNumber(12)).expect("held").stage,
        PreconfStage::Streaming
    );
    assert_eq!(
        affected(&events).len(),
        radar.counters().affected_pools_emitted as usize,
        "two registered pools per frame, and the seal emitted nothing new"
    );
    for record in radar.records() {
        assert_eq!(record.endpoint_id, ENDPOINT);
        assert!(
            !record.state_root_known,
            "height {} carries the measured all-zero root, not an invented one (§52)",
            record.number
        );
        assert!(
            !record.wire_index_known,
            "and no wire frame index, which is why gaps are reported as unmeasurable"
        );
    }
}

#[test]
fn duplicate_is_applied_once_and_the_run_continues() {
    let _meta = labelled("duplicate", 14);
    let (radar, events) = twice(fx_duplicate);

    assert_eq!(
        accepted_heights(&events),
        vec![13, 14, 14, 15],
        "§9's 0,1,2,2,3"
    );
    assert!(refusals(&events).is_empty());
    assert_eq!(radar.counters().frames_duplicate, 1);
    assert_eq!(radar.counters().frames_accepted, 4);
    let state = radar.view(BlockNumber(14)).expect("held");
    assert_eq!(state.frames_observed, 2);
    assert_eq!(state.distinct_view_hashes, 1);
    assert_eq!(state.transaction_hashes, tx_hashes(14, 1));
    assert_eq!(
        radar.counters().affected_pools_emitted,
        3,
        "the repeat added no finding"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RadarEvent::FrameAccepted {
                number: 14,
                duplicate_view_hash: true,
                ..
            }
        )),
        "and it is labelled as a repeat"
    );
}

#[test]
fn gap_is_named_as_a_skip_and_never_filled_in() {
    let _meta = labelled("gap", 22);
    let (radar, events) = twice(fx_gap);

    let notes = sequences(&events);
    assert_eq!(notes.len(), 1, "the skip is announced: {notes:?}");
    assert!(notes[0].contains("pending moved 20 -> 22"), "{}", notes[0]);
    assert!(
        notes[0].contains("GAP_DETECTED by block number"),
        "§9's own class name is on it: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("NOT_MEASURABLE"),
        "while the part the transport cannot see says so: {}",
        notes[0]
    );
    assert!(!radar.counters().sequence_gaps_measurable);
    assert_eq!(radar.held_heights(), vec![BlockNumber(20), BlockNumber(22)]);
    assert!(
        radar.view(BlockNumber(21)).is_none(),
        "the skipped height was not reconstructed"
    );
}

#[test]
fn a_receipt_naming_another_block_is_refused_and_never_spliced() {
    let _meta = labelled("wrong block", 101);
    let (radar, events) = twice(fx_wrong_block);

    assert_eq!(
        radar.counters().receipts_wrong_block,
        1,
        "the witness the fixture recorded is the counter the radar kept"
    );
    assert_eq!(
        radar.held_heights(),
        vec![BlockNumber(100), BlockNumber(101)],
        "the two blocks stay two views"
    );
    assert_eq!(
        radar
            .view(BlockNumber(100))
            .expect("held")
            .transaction_hashes,
        tx_hashes(100, 2)
    );
    assert_eq!(
        radar
            .view(BlockNumber(101))
            .expect("held")
            .transaction_hashes,
        tx_hashes(101, 1),
        "and the height the stray receipt was read under did not absorb it either"
    );
    let pools = affected(&events);
    assert!(
        pools
            .iter()
            .all(|pool| pool.reason == AffectedReason::TransactionTarget),
        "the refused receipt emitted no log finding: {pools:?}"
    );
    assert_eq!(
        refusals(&events),
        Vec::new(),
        "a stray receipt is not a frame refusal"
    );
}

#[test]
fn wrong_chain_is_refused_before_the_radar_holds_anything() {
    let _meta = labelled("wrong chain", 30);
    let (radar, events) = twice(fx_wrong_chain);

    assert_eq!(refusals(&events), vec![(30, "chain_id_mismatch")]);
    assert_eq!(events.len(), 1, "a refusal, not a discussion");
    assert!(radar.held_heights().is_empty());
    assert_eq!(radar.counters().frames_wrong_chain, 1);
    assert_eq!(radar.counters().frames_accepted, 0);
    assert_eq!(radar.counters().transactions_observed, 0);
    assert_eq!(radar.counters().affected_pools_emitted, 0);
}

#[test]
fn wrong_parent_invalidates_the_view_instead_of_merging() {
    let _meta = labelled("wrong parent", 11);
    let (radar, events) = twice(fx_wrong_parent);

    assert_eq!(refusals(&events), vec![(11, "wrong_parent")]);
    assert_eq!(radar.counters().frames_wrong_parent, 1);
    assert!(
        events.iter().any(|event| matches!(
            event,
            RadarEvent::ViewInvalidated {
                number: 11,
                reason: "wrong_parent"
            }
        )),
        "{events:?}"
    );
    assert_eq!(
        radar.counters().affected_pools_emitted,
        2,
        "one finding per accepted frame (heights 10 and 11); the refused frame emitted \
         nothing on the way out"
    );
    assert_eq!(
        affected(&events).len(),
        2,
        "and the refused frame is not among them"
    );
    assert!(
        !radar.held_heights().contains(&BlockNumber(11)),
        "the conflicting view was thrown away"
    );
    assert_eq!(
        radar
            .view(BlockNumber(10))
            .expect("the sealed height is untouched")
            .stage,
        PreconfStage::Reconciled
    );
}

#[test]
fn malformed_payloads_are_each_refused_by_name() {
    let _meta = labelled("malformed", 40);
    let (radar, events) = twice(fx_malformed);

    let (radar_again, _, witness) = once(fx_malformed);
    assert_eq!(witness.len(), 5, "one line per malformed payload");
    assert_eq!(
        record_shape(&radar_again),
        record_shape(&radar),
        "including the decoder's verdicts"
    );
    for line in &witness {
        assert!(
            line.contains("refused:"),
            "a malformed payload that decoded would be a schema guess: {line}"
        );
    }
    assert!(
        witness.iter().any(|line| line.contains("hash_only_list")),
        "the hash-list case is its own class, not a decode failure: {witness:?}"
    );
    assert_eq!(accepted_heights(&events), vec![40], "and the run goes on");
    assert_eq!(radar.counters().frames_accepted, 1);
}

#[test]
fn late_flashblock_is_compared_then_discarded() {
    let _meta = labelled("late flashblock", 10);
    let (radar, events) = twice(fx_late_flashblock);

    assert_eq!(refusals(&events), vec![(10, "late_after_canonical")]);
    assert_eq!(radar.counters().frames_late, 1);
    assert_eq!(radar.counters().late_frames_compared, 1);
    let state = radar.view(BlockNumber(10)).expect("kept after closure");
    assert_eq!(
        state.stage,
        PreconfStage::Reconciled,
        "the verdict the block produced is not relabelled by a frame that arrived after it"
    );
    assert_eq!(state.transaction_hashes, tx_hashes(10, 1));
    assert_eq!(state.frames_observed, 1);
    assert_eq!(
        radar.counters().affected_pools_emitted,
        1,
        "the pre-seal frame's finding only"
    );
    let pools: Vec<alloy_primitives::Address> =
        state.affected_pools.iter().map(|pool| pool.pool).collect();
    assert_eq!(pools, vec![addr(POOL_A)], "not the late frame's pool");
}

#[test]
fn canonical_mismatch_wins_with_no_merge() {
    let _meta = labelled("canonical mismatch", 10);
    let (radar, events) = twice(fx_canonical_mismatch);

    let reconciled = events
        .iter()
        .find(|event| event.kind_label() == "reconciled")
        .expect("the closure is reported");
    assert!(
        matches!(
            reconciled,
            RadarEvent::Reconciled {
                verdict: ReconciliationVerdict::ContentDivergent {
                    only_in_view: 1,
                    only_in_canonical: 1
                },
                hash_matched: false,
                ..
            }
        ),
        "{reconciled:?}"
    );
    let state = radar.view(BlockNumber(10)).expect("kept for the record");
    assert_eq!(state.stage, PreconfStage::Superseded, "canonical won");
    assert_eq!(
        state.transaction_hashes,
        tx_hashes(10, 2),
        "the view was not rewritten to look like the block"
    );
    assert_eq!(radar.counters().reconciled_content_divergent, 1);
}

#[test]
fn multiple_pool_changes_in_one_frame_keep_their_own_identities() {
    let _meta = labelled("multiple pool changes", 40);
    let (radar, events) = twice(fx_multiple_pool_changes);

    let pools = affected(&events);
    assert_eq!(pools.len(), 4, "{pools:?}");
    let keys: Vec<_> = pools.iter().map(AffectedPool::identity_key).collect();
    let unique: std::collections::BTreeSet<_> = keys.iter().cloned().collect();
    assert_eq!(unique.len(), keys.len(), "no two findings collapse");
    assert_eq!(
        pools
            .iter()
            .filter(|pool| pool.reason == AffectedReason::TransactionTarget)
            .count(),
        3
    );
    assert_eq!(
        pools
            .iter()
            .filter(|pool| pool.reason == AffectedReason::SuccessfulLogEmitter)
            .count(),
        1,
        "and the log-backed finding is labelled as the stronger evidence it is"
    );
    let distinct_pools: std::collections::BTreeSet<_> =
        pools.iter().map(|pool| pool.pool).collect();
    assert_eq!(distinct_pools.len(), 2, "two pools moved");
    assert_eq!(
        radar
            .view(BlockNumber(40))
            .expect("held")
            .affected_pools
            .len(),
        4
    );
}

#[test]
fn a_reverted_or_unstatus_receipt_is_never_a_state_mutation() {
    let _meta = labelled("reverted tx", 10);
    let (radar, events) = twice(fx_reverted_tx);

    assert!(affected(&events).is_empty(), "{events:?}");
    assert_eq!(radar.counters().reverted_receipts_skipped, 2);
    assert_eq!(accepted_heights(&events), vec![10, 10]);
    assert_eq!(radar.counters().affected_pools_by_log, 0);

    // Positive control: the same log under a success status does fire, so the two
    // refusals above are about the status and not about a scan that never runs.
    let mut control = preconf_support::radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let tx = b256(tx_byte(10, 0));
    let events = offer_with(
        &mut control,
        frame_of(10, 0x10, &[0]),
        &mut clock,
        Some(vec![receipt(
            tx,
            10,
            sealed_hash(10),
            Some(1),
            vec![log(addr(POOL_A), tx, 0, 0, false)],
        )]),
    );
    assert_eq!(affected(&events).len(), 1);
    assert_eq!(control.counters().affected_pools_by_log, 1);
}

#[test]
fn an_unrelated_transaction_is_traffic_and_not_a_finding() {
    let _meta = labelled("unrelated tx", 50);
    let (radar, events) = twice(fx_unrelated_tx);

    assert!(affected(&events).is_empty());
    assert_eq!(radar.counters().affected_pools_emitted, 0);
    assert_eq!(
        radar.counters().transactions_observed,
        3,
        "the frame is still real; it just touches nothing registered"
    );
    assert_eq!(accepted_heights(&events), vec![50]);

    let mut control = preconf_support::radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = offer(
        &mut control,
        frame_of(50, 0x32, &[FOREIGN, POOL_A, FOREIGN]),
        &mut clock,
    );
    assert_eq!(affected(&events).len(), 1);
}

// — §47's own gate: the labels. —

#[test]
fn all_twelve_classes_are_shipped_and_each_carries_its_five_labels() {
    let mut metas = Vec::new();
    for (index, &class) in CLASSES.iter().enumerate() {
        let meta = FixtureMeta::controlled(class, 10 + index as u64);
        assert_eq!(
            label_gaps(&meta),
            Vec::<&'static str>::new(),
            "{class} is missing a §47 label"
        );
        let json = meta.as_json();
        assert_eq!(json["fixture"], class);
        assert_eq!(json["chain_id"], CHAIN.0);
        assert_eq!(json["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(json["block"], 10 + index as u64);
        assert!(json["source"].as_str().is_some_and(|s| !s.is_empty()));
        assert_ne!(json["capture_timestamp_unix_ms"], Value::from(0));
        metas.push(json);
    }
    assert_eq!(metas.len(), 12, "the twelve classes §47 lists");
    let names: Vec<&str> = CLASSES.to_vec();
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        unique.len(),
        12,
        "and no class is listed twice under two names"
    );
}

#[test]
fn the_label_gate_can_fail() {
    // Without this row the check above would pass on an all-empty label, which is the
    // failure mode §47 is written to prevent.
    let broken = FixtureMeta {
        name: "gap",
        source: "",
        captured_at_unix_ms: 0,
        chain_id: 0,
        block_number: 0,
        protocol_version: "",
    };
    let gaps = label_gaps(&broken);
    assert!(gaps.contains(&"source"), "{gaps:?}");
    assert!(gaps.contains(&"capture timestamp"), "{gaps:?}");
    assert!(gaps.contains(&"chain"), "{gaps:?}");
    assert!(gaps.contains(&"block"), "{gaps:?}");
    assert!(gaps.contains(&"protocol version"), "{gaps:?}");
    assert_eq!(
        label_gaps(&FixtureMeta::controlled("gap", 22)),
        Vec::<&'static str>::new(),
        "and the same check passes on a real fixture"
    );
}
