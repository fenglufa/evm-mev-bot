//! M9.4 §46: the unit matrix the radar itself has to answer for, row by row.
//!
//! The decoder rows live in `crates/live/src/preconf_decode.rs`'s own tests and the
//! transport rows in `tests/preconf_link_loop.rs`; this file is the middle of the stack
//! — identity, sequence, duplicate, gap, wrong chain, parent mismatch, reconnect,
//! affected-pool extraction, reconciliation, canonical-wins, late frame — each driven by
//! calling the radar directly, so a failure names the rule it broke rather than the loop
//! stage that carried it.
//!
//! Every test runs on a caller-owned clock (`StepClock`) and on the single fixture
//! convention in `preconf_support` (`frame_of` / `seal_next` / `tx_hashes`): no test
//! reads a system clock and no test invents a hash to make a check pass, so an assertion
//! about a timestamp is an assertion about arithmetic the code did (§41, §43).

mod preconf_support;

use preconf_support::*;

use alloy_primitives::B256;
use evm_core::BlockNumber;
use evm_live::{
    AffectedReason, Field, PreconfStage, RadarConfig, RadarCounters, RadarEvent, RadarInput,
    ReconciliationVerdict,
};

const POOL_A: u8 = 0xa1;
const POOL_B: u8 = 0xa2;

#[test]
fn identity_carries_the_chain_the_height_and_a_locally_assigned_sequence() {
    // §8's identity, and the one part of it the wire does not supply.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    assert_eq!(count(&events, "frame_accepted"), 1);

    let state = radar
        .view(BlockNumber(10))
        .expect("the frame is held at its own height");
    let identity = &state.latest.identity;
    assert_eq!(identity.chain_id, CHAIN);
    assert_eq!(identity.block_number, BlockNumber(10));
    assert_eq!(identity.local_frame_sequence, 1, "assigned in read order");
    match &identity.wire_index {
        Field::Unknown {
            key_present,
            detail,
        } => {
            assert!(
                !key_present,
                "no such key exists on this endpoint: {detail}"
            );
            assert!(detail.contains("no frame index"), "{detail}");
        }
        Field::Known(value) => {
            panic!("the measured endpoint offers no frame index, got {value:#x}")
        }
    }
    assert_eq!(identity.view_hash.value(), Some(&b256(0x10)));
    assert_eq!(identity.parent_hash.value(), Some(&sealed_hash(9)));
    // §52: the all-zero root the endpoint answers is never a root the model knows, and
    // it is a *present* key carrying a placeholder — a different finding from a silence.
    match &state.latest.state_root {
        Field::Unknown {
            key_present,
            detail,
        } => {
            assert!(key_present, "the payload does name stateRoot");
            assert!(detail.contains("all-zero placeholder"), "{detail}");
        }
        Field::Known(root) => panic!("a placeholder root was read as a root: {root:#x}"),
    }
    assert_eq!(radar.counters().state_root_placeholders, 1);
    // The quantities the payload does supply are kept as supplied.
    assert_eq!(
        state.latest.chain_timestamp_secs.value(),
        Some(&0x6708_9d20)
    );
    assert_eq!(state.latest.endpoint_id, ENDPOINT);
}

#[test]
fn a_growing_view_stays_one_height_and_holds_the_latest_frame_not_a_merge() {
    // Audit §2.2: one height, many view hashes, a list only ever longer. The held view
    // is the last read, because merging would be a second state derivation (§17).
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    offer(&mut radar, frame_of(10, 0x11, &[POOL_A, 0]), &mut clock);
    let events = offer(&mut radar, frame_of(10, 0x12, &[POOL_A, 0, 0]), &mut clock);

    assert_eq!(count(&events, "frame_accepted"), 1);
    assert_eq!(
        radar.held_heights(),
        vec![BlockNumber(10)],
        "one height only"
    );
    let state = radar.view(BlockNumber(10)).expect("held");
    assert_eq!(state.frames_observed, 3);
    assert_eq!(state.distinct_view_hashes, 3);
    assert_eq!(
        state.transaction_hashes,
        tx_hashes(10, 3),
        "exactly the latest frame, not a union of the three"
    );
    assert_eq!(state.latest.identity.local_frame_sequence, 3);
    assert_eq!(radar.counters().heights_seen, 1);
    assert_eq!(radar.counters().heights_multi_view, 1);
    assert_eq!(radar.counters().heights_ge_three_views, 1);
    assert_eq!(state.stage, PreconfStage::Streaming);
    assert_eq!(
        radar.counters().transactions_observed,
        1 + 2 + 3,
        "every read counted, so a replaced list is still an accounting of the frames"
    );
}

#[test]
fn a_duplicate_frame_is_recorded_as_a_repeat_and_derives_nothing_new() {
    // §9's duplicate — `0,1,2,2,3` in the task book, i.e. the same view hash twice. The
    // read still happened, so the frame is accepted: with the duplicate flag on, and
    // with the affected set untouched.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let first = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    let second = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);

    assert_eq!(count(&first, "pool_affected"), 1);
    assert_eq!(
        count(&second, "pool_affected"),
        0,
        "a re-read is not a new finding"
    );
    match second.first().expect("the duplicate is accepted") {
        RadarEvent::FrameAccepted {
            duplicate_view_hash,
            local_frame_sequence,
            transaction_count,
            ..
        } => {
            assert!(*duplicate_view_hash, "and says so");
            assert_eq!(*local_frame_sequence, 2, "a new read, the same view");
            assert_eq!(*transaction_count, 1);
        }
        other => panic!("expected an acceptance, got {other:?}"),
    }
    let state = radar.view(BlockNumber(10)).expect("held");
    assert_eq!(state.frames_observed, 2);
    assert_eq!(state.distinct_view_hashes, 1);
    assert_eq!(radar.counters().frames_duplicate, 1);
    assert_eq!(radar.counters().frames_accepted, 2);
    assert_eq!(radar.counters().affected_pools_emitted, 1);
    // The stamps stay the first-seen ones: a duplicate does not re-date the view.
    assert_eq!(state.timestamps.sequence_validated_at_unix_ms, Some(1_015));
}

#[test]
fn a_height_that_was_never_pending_is_announced_and_stays_unmeasurable() {
    // §9's gap question, answered with what this transport can prove: the skipped height
    // is named, and the *class* of finding is "not measurable", because no frame index
    // exists whose absence could be checked (audit §2.3).
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(11, 0x11, &[POOL_A]), &mut clock);
    let events = offer(&mut radar, frame_of(13, 0x13, &[POOL_A]), &mut clock);

    let notes = sequences(&events);
    assert_eq!(notes.len(), 1, "the skip is announced: {notes:?}");
    assert!(
        notes[0].contains("pending moved 11 -> 13"),
        "note says: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("1 height(s) were never observed as pending"),
        "note says: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("GAP_DETECTED by block number"),
        "and §9's own class name is on it: {}",
        notes[0]
    );
    assert!(
        notes[0].contains("NOT_MEASURABLE"),
        "while the part the transport could not confirm says so: {}",
        notes[0]
    );
    assert!(!radar.counters().sequence_gaps_measurable);
    assert_eq!(
        radar.counters().frames_rejected,
        0,
        "no frame is refused for this"
    );
    assert_eq!(accepted_heights(&events), vec![13]);
    assert_eq!(radar.held_heights(), vec![BlockNumber(11), BlockNumber(13)]);
}

#[test]
fn a_frame_from_another_chain_is_refused_and_touches_nothing() {
    // §9's identity rule at the radar boundary, and NC6's requirement that a refusal
    // leaves no trace behind: no height, no pool, no stage, no counter but the refusal.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let raw = frame_of(10, 0x10, &[POOL_A]);
    let foreign = evm_live::frame_from_pending_value(OTHER_CHAIN, &raw, 1, 1_005, ENDPOINT)
        .expect("the same fixture, on another chain id");
    let mut stamp = move || clock.tick();
    let events = radar.ingest(
        RadarInput {
            frame: foreign,
            receipts: None,
            decoded_at_unix_ms: 1_010,
        },
        &mut stamp,
    );

    assert_eq!(refusals(&events), vec![(10, "chain_id_mismatch")]);
    assert!(radar.held_heights().is_empty());
    assert_eq!(events.len(), 1, "a refusal, not a discussion");
    assert_eq!(radar.counters().frames_offered, 1);
    assert_eq!(radar.counters().frames_rejected, 1);
    assert_eq!(radar.counters().frames_wrong_chain, 1);
    assert_eq!(radar.counters().frames_accepted, 0);
    assert_eq!(radar.counters().heights_seen, 0);
    assert_eq!(radar.counters().affected_pools_emitted, 0);
    assert_eq!(radar.counters().transactions_observed, 0);
}

#[test]
fn a_pending_view_that_moved_backwards_is_evidence_about_the_source() {
    // §9: a regressed height is recorded, and the frame is still held — refusing it
    // would throw away the only witness that the source rewound.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(12, 0x12, &[POOL_A]), &mut clock);
    let events = offer(&mut radar, frame_of(11, 0x11, &[POOL_A]), &mut clock);

    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("moved backwards: 11 after 12")),
        "{notes:?}"
    );
    assert_eq!(accepted_heights(&events), vec![11]);
    assert_eq!(radar.counters().frames_regressed, 1);
    assert_eq!(radar.counters().frames_rejected, 0);
    // Continuity is kept against the highest number seen, not the last one read.
    assert_eq!(radar.held_heights(), vec![BlockNumber(11), BlockNumber(12)]);
    let events = offer(&mut radar, frame_of(13, 0x13, &[POOL_A]), &mut clock);
    assert!(
        sequences(&events).is_empty(),
        "continuity is measured from 12, the head, so 13 is the next height and not a gap: {:?}",
        sequences(&events)
    );
    assert_eq!(
        radar.counters().frames_regressed,
        1,
        "the rewind was counted once, when it happened, and 13 does not look like one"
    );
}

#[test]
fn a_parent_that_does_not_continue_the_sealed_block_below_invalidates_the_view() {
    // §9 + §24: a wrong identity invalidates, it does not merge.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);

    let wrong = pending(
        11,
        b256(0x77),
        Some(b256(0x11)),
        vec![transaction(tx_byte(11, 0), Some(addr(POOL_A)), 0)],
    );
    let events = offer(&mut radar, wrong, &mut clock);

    assert_eq!(refusals(&events), vec![(11, "wrong_parent")]);
    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("does not continue the sealed 10")
                && note.contains("thrown away rather than patched")),
        "{notes:?}"
    );
    assert_eq!(radar.counters().frames_wrong_parent, 1);
    assert_eq!(radar.counters().frames_accepted, 1, "only the frame at 10");
    assert!(!radar.held_heights().contains(&BlockNumber(11)));

    let continues = pending(
        11,
        sealed_hash(10),
        Some(b256(0x11)),
        vec![transaction(tx_byte(11, 0), Some(addr(POOL_A)), 0)],
    );
    let events = offer(&mut radar, continues, &mut clock);
    assert_eq!(count(&events, "frame_accepted"), 1);
    assert_eq!(radar.counters().parent_checks_passed, 1);
    assert_eq!(
        radar.counters().frames_rejected,
        1,
        "the conflict is the only refusal"
    );
}

#[test]
fn a_parent_the_payload_does_not_name_is_recorded_unconfirmed_not_checked() {
    // §6: the absence of a field is `Unknown`, and a check that could not run is not a
    // pass. The frame is still held — an unnamed parent is the endpoint's silence, not
    // a contradiction.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    seal_next(&mut radar, 10, Vec::new(), &mut clock);
    let mut bare = frame_of(11, 0x11, &[POOL_A]);
    bare.as_object_mut()
        .expect("an object")
        .remove("parentHash");
    let events = offer(&mut radar, bare, &mut clock);

    assert_eq!(count(&events, "frame_accepted"), 1, "still held");
    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("continuity is UNCONFIRMED")
                && note.contains("not the same claim as 'it continues'")),
        "{notes:?}"
    );
    assert_eq!(radar.counters().parent_unconfirmed, 1);
    assert_eq!(radar.counters().parent_checks_passed, 0);
    assert_eq!(radar.counters().frames_rejected, 0);
    match &radar
        .view(BlockNumber(11))
        .expect("held")
        .latest
        .identity
        .parent_hash
    {
        Field::Unknown { key_present, .. } => assert!(!key_present, "the key was absent"),
        Field::Known(parent) => panic!("an absent parentHash became {parent:#x}"),
    }
}

#[test]
fn a_parent_check_that_could_not_run_is_counted_as_uncheckable() {
    // The third answer to §9's parent question: nothing sealed below yet, so continuity
    // is not checkable *in this session* — a statement about the session, which must not
    // be tallied as a pass.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    assert_eq!(count(&events, "frame_accepted"), 1);
    assert_eq!(radar.counters().parent_checks_uncheckable, 1);
    assert_eq!(radar.counters().parent_checks_passed, 0);
    assert_eq!(radar.counters().parent_unconfirmed, 0);
    assert!(
        sequences(&events).is_empty(),
        "uncheckable is a tally, not an alarm: nothing is announced for it"
    );
}

#[test]
fn a_frame_for_a_height_that_already_sealed_is_compared_and_discarded() {
    // §19: compare for the record, then throw away — never merge into the closed view.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);
    let before = radar
        .view(BlockNumber(10))
        .expect("closed view kept")
        .clone();

    // The late frame names a transaction the sealed block does not have.
    let late = pending(
        10,
        sealed_hash(9),
        Some(b256(0x13)),
        vec![transaction(0x77, Some(addr(POOL_A)), 0)],
    );
    let events = offer(&mut radar, late, &mut clock);

    assert_eq!(refusals(&events), vec![(10, "late_after_canonical")]);
    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("compared by content")
                && note.contains("discarded, never merged")),
        "{notes:?}"
    );
    assert_eq!(radar.counters().frames_late, 1);
    assert_eq!(radar.counters().late_frames_compared, 1);
    let after = radar.view(BlockNumber(10)).expect("still kept").clone();
    assert_eq!(
        after, before,
        "the late frame changed nothing, in either direction"
    );
    assert_eq!(after.stage, PreconfStage::Reconciled);
    assert_eq!(after.frames_observed, 1);
    assert_eq!(
        radar.counters().affected_pools_emitted,
        1,
        "only the first frame's"
    );
}

#[test]
fn affected_pools_name_the_call_and_the_log_emitter_as_two_different_strengths() {
    // §12/§13/§14: a transaction sent *to* a pool is a call; a log from a successful
    // receipt is an effect. Both are reported, each labelled as what it is, and nothing
    // in between is inferred.
    let mut radar = radar([addr(POOL_A), addr(POOL_B)]);
    let mut clock = StepClock::new(1_000, 5);
    let raw = pending(
        10,
        sealed_hash(9),
        Some(b256(0x10)),
        vec![
            transaction(tx_byte(10, 0), Some(addr(POOL_A)), 0),
            transaction(tx_byte(10, 1), Some(addr(0xf0)), 1),
        ],
    );
    let receipts = vec![receipt(
        b256(tx_byte(10, 1)),
        10,
        sealed_hash(10),
        Some(1),
        vec![log(addr(POOL_B), b256(tx_byte(10, 1)), 0, 1, false)],
    )];
    let events = offer_with(&mut radar, raw, &mut clock, Some(receipts));
    let pools = affected(&events);

    assert_eq!(pools.len(), 2);
    assert_eq!(pools[0].pool, addr(POOL_A));
    assert_eq!(pools[0].reason, AffectedReason::TransactionTarget);
    assert_eq!(pools[0].transaction_hash, b256(tx_byte(10, 0)));
    assert_eq!(pools[1].pool, addr(POOL_B));
    assert_eq!(pools[1].reason, AffectedReason::SuccessfulLogEmitter);
    // §13's field list, kept honest about what backs each entry.
    assert_eq!(pools[0].log_index.state_label(), "unknown");
    assert_eq!(pools[0].topic0.state_label(), "unknown");
    assert_eq!(pools[1].log_index.state_label(), "known");
    assert_eq!(pools[1].log_index.value(), Some(&0));
    assert_eq!(pools[1].topic0.value(), Some(&b256(0xd0)));
    assert_eq!(pools[1].local_frame_sequence, pools[0].local_frame_sequence);
    assert_eq!(radar.counters().affected_pools_by_target, 1);
    assert_eq!(radar.counters().affected_pools_by_log, 1);
    assert_eq!(
        radar.counters().affected_pools_emitted,
        radar.counters().affected_pools_by_target + radar.counters().affected_pools_by_log
    );
    let state = radar.view(BlockNumber(10)).expect("held");
    assert_eq!(state.affected_pools.len(), 2);
    assert!(
        state
            .affected_pools
            .iter()
            .all(|pool| pool.chain_id == CHAIN && pool.block_number == 10),
        "a finding carries the chain and the height it was made on"
    );
}

#[test]
fn a_receipt_naming_another_block_contributes_nothing_and_is_counted() {
    // §9's wrong-block check on the receipt side: the receipts describing a pending
    // frame must describe *that* frame. One from a sealed block is not evidence about
    // the view being held.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let raw = pending(
        10,
        sealed_hash(9),
        Some(b256(0x10)),
        vec![transaction(tx_byte(10, 0), None, 0)],
    );
    let strayed = receipt(
        b256(tx_byte(10, 0)),
        9,
        sealed_hash(9),
        Some(1),
        vec![log(addr(POOL_A), b256(tx_byte(10, 0)), 0, 0, false)],
    );
    let events = offer_with(&mut radar, raw, &mut clock, Some(vec![strayed]));

    assert_eq!(count(&events, "pool_affected"), 0);
    assert_eq!(radar.counters().receipts_wrong_block, 1);
    let notes = sequences(&events);
    assert!(
        notes.iter().any(|note| note.contains("claims block 9")
            && note.contains("the receipt contributes nothing")),
        "{notes:?}"
    );
    assert_eq!(
        radar.counters().frames_accepted,
        1,
        "the frame itself is fine"
    );
}

#[test]
fn a_removed_log_is_excluded_from_the_affected_set_and_announced() {
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let raw = pending(
        10,
        sealed_hash(9),
        Some(b256(0x10)),
        vec![transaction(tx_byte(10, 0), None, 0)],
    );
    let retracted = receipt(
        b256(tx_byte(10, 0)),
        10,
        sealed_hash(10),
        Some(1),
        vec![log(addr(POOL_A), b256(tx_byte(10, 0)), 3, 0, true)],
    );
    let events = offer_with(&mut radar, raw, &mut clock, Some(vec![retracted]));

    assert_eq!(count(&events, "pool_affected"), 0);
    assert_eq!(radar.counters().receipts_removed_logs, 1);
    let notes = sequences(&events);
    assert!(
        notes.iter().any(|note| note.contains("marked removed")
            && note.contains("excluded from the affected set")),
        "{notes:?}"
    );
}

/// One reconciliation case: the transactions the *view* held, and what closing it has to
/// produce.
struct Closure {
    number: u64,
    view_bytes: Vec<u8>,
    sealed_bytes: Vec<u8>,
    verdict: ReconciliationVerdict,
    stage: PreconfStage,
    counter: fn(&RadarCounters) -> u64,
}

#[test]
fn the_four_content_verdicts_differ_only_in_what_they_record_about_the_view() {
    // §15–§18: compare by content, because the measurement says hashes never match
    // (0/48) while contents do (23/48). Each class closes the view, and every one of
    // them yields to canonical.
    let sealed_bytes = vec![0x41u8, 0x42, 0x43];
    let cases = [
        Closure {
            number: 20,
            view_bytes: sealed_bytes.clone(),
            sealed_bytes: sealed_bytes.clone(),
            verdict: ReconciliationVerdict::ContentEqual,
            stage: PreconfStage::Reconciled,
            counter: |counters| counters.reconciled_content_equal,
        },
        Closure {
            number: 21,
            view_bytes: vec![0x41],
            sealed_bytes: sealed_bytes.clone(),
            verdict: ReconciliationVerdict::ContentPrefix { held: 1, sealed: 3 },
            stage: PreconfStage::Reconciled,
            counter: |counters| counters.reconciled_content_prefix,
        },
        Closure {
            number: 22,
            view_bytes: vec![0x41, 0x42, 0x43, 0x44],
            sealed_bytes: sealed_bytes.clone(),
            verdict: ReconciliationVerdict::ContentSuperset { held: 4, sealed: 3 },
            stage: PreconfStage::Superseded,
            counter: |counters| counters.reconciled_content_superset,
        },
        Closure {
            number: 23,
            view_bytes: vec![0x41, 0x99],
            sealed_bytes: sealed_bytes.clone(),
            verdict: ReconciliationVerdict::ContentDivergent {
                only_in_view: 1,
                only_in_canonical: 2,
            },
            stage: PreconfStage::Superseded,
            counter: |counters| counters.reconciled_content_divergent,
        },
    ];

    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    for case in cases {
        let raw = pending(
            case.number,
            sealed_hash(case.number - 1),
            Some(b256(0x60 + case.view_bytes.len() as u8)),
            case.view_bytes
                .iter()
                .enumerate()
                .map(|(index, &byte)| transaction(byte, Some(addr(POOL_A)), index as u64))
                .collect(),
        );
        offer(&mut radar, raw, &mut clock);
        let sealed: Vec<B256> = case.sealed_bytes.iter().map(|&byte| b256(byte)).collect();
        let events = seal(
            &mut radar,
            digest(
                case.number,
                sealed_hash(case.number),
                sealed_hash(case.number - 1),
                sealed,
                clock.tick(),
            ),
            &mut clock,
        );

        let (verdict, hash_matched) = events
            .iter()
            .find_map(|event| match event {
                RadarEvent::Reconciled {
                    verdict,
                    hash_matched,
                    ..
                } => Some((*verdict, *hash_matched)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("height {}: no reconciliation event", case.number));
        assert_eq!(verdict, case.verdict, "height {}", case.number);
        assert!(
            !hash_matched,
            "height {}: a view hash is not a block hash",
            case.number
        );
        assert!(
            case.verdict.canonical_wins(),
            "height {}: §17 is not a branch",
            case.number
        );
        assert_eq!(
            radar
                .view(BlockNumber(case.number))
                .expect("closed view kept")
                .stage,
            case.stage,
            "height {}",
            case.number
        );
        assert_eq!(
            (case.counter)(radar.counters()),
            1,
            "height {}",
            case.number
        );
    }

    let counters = *radar.counters();
    assert_eq!(counters.canonical_closures, 4);
    assert_eq!(counters.frames_accepted, 4);
    assert_eq!(counters.reconciled_hash_match, 0);
    assert_eq!(
        counters.reconciled_content_equal
            + counters.reconciled_content_prefix
            + counters.reconciled_content_superset
            + counters.reconciled_content_divergent,
        4,
        "the four classes partition the four closures, none of them landing in no_view"
    );
    assert_eq!(counters.reconciled_no_view, 0);
}

#[test]
fn every_verdict_class_yields_to_canonical_including_the_no_view_one() {
    // §17 stated as a property over all six classes rather than over the four that
    // needed arithmetic: `canonical_wins()` is a constant, and a verdict that could
    // return false would be a merge waiting to be written.
    let classes = [
        ReconciliationVerdict::ContentEqual,
        ReconciliationVerdict::ContentPrefix { held: 1, sealed: 2 },
        ReconciliationVerdict::ContentSuperset { held: 2, sealed: 1 },
        ReconciliationVerdict::ContentDivergent {
            only_in_view: 1,
            only_in_canonical: 2,
        },
        ReconciliationVerdict::NoView,
        ReconciliationVerdict::LateFrameDiscarded {
            number: 10,
            sealed_before_unix_ms: 1_500,
        },
    ];
    for verdict in classes {
        assert!(verdict.canonical_wins(), "{verdict:?}");
        assert!(!verdict.as_str().is_empty(), "{verdict:?}");
    }

    // A closure with nothing held is `NoView`, and it still yields.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = seal_next(&mut radar, 30, tx_hashes(30, 1), &mut clock);
    let reconciled = events
        .iter()
        .find_map(|event| match event {
            RadarEvent::Reconciled { verdict, .. } => Some(*verdict),
            _ => None,
        })
        .expect("the closure is reported");
    assert_eq!(reconciled, ReconciliationVerdict::NoView);
    assert!(reconciled.canonical_wins());
    assert_eq!(radar.counters().reconciled_no_view, 1);
    assert!(
        radar.held_heights().is_empty(),
        "nothing was held, so nothing was closed"
    );
}

#[test]
fn the_affected_pool_identity_is_checked_and_an_absent_list_is_not_agreement() {
    // §16's third outcome must not read as a pass: the canonical side naming no pool
    // list is `UNVERIFIED`, which §28 forbids recording as an empty set.
    let mut radar = radar([addr(POOL_A), addr(POOL_B)]);
    let mut clock = StepClock::new(1_000, 5);
    let cases = [
        (Some(vec![addr(POOL_A)]), "matched"),
        (Some(vec![addr(POOL_A), addr(POOL_B)]), "diverged"),
        (None, "unverified"),
    ];
    for (index, (canonical_pools, expectation)) in cases.into_iter().enumerate() {
        let number = 31 + index as u64;
        // The view names one pool: POOL_A, targeted by the height's first transaction.
        offer(
            &mut radar,
            frame_of(number, 0x30 + index as u8, &[POOL_A]),
            &mut clock,
        );
        let mut sealed = digest(
            number,
            sealed_hash(number),
            sealed_hash(number - 1),
            tx_hashes(number, 1),
            clock.tick(),
        );
        sealed.affected_pools = canonical_pools;
        let events = seal(&mut radar, sealed, &mut clock);
        let notes = sequences(&events);

        match expectation {
            "matched" => {
                assert_eq!(radar.counters().reconciled_pools_matched, 1);
                assert!(
                    !notes
                        .iter()
                        .any(|note| note.contains("affected pools differ")),
                    "an agreement is not an alarm: {notes:?}"
                );
            }
            "diverged" => {
                assert_eq!(radar.counters().reconciled_pools_diverged, 1);
                assert!(
                    notes
                        .iter()
                        .any(|note| note.contains("Canonical is the record")
                            && note.contains("not carried forward")),
                    "{notes:?}"
                );
            }
            _ => {
                assert_eq!(radar.counters().reconciled_pools_unverified, 1);
                assert!(
                    notes
                        .iter()
                        .any(|note| note.contains("UNVERIFIED")
                            && note.contains("never an empty set")),
                    "{notes:?}"
                );
            }
        }
    }
    let counters = *radar.counters();
    assert_eq!(
        counters.reconciled_pools_matched
            + counters.reconciled_pools_diverged
            + counters.reconciled_pools_unverified,
        3,
        "one witness per case, and never the same bucket twice"
    );
}

#[test]
fn a_canonical_digest_from_another_chain_closes_nothing() {
    // §9's identity check applies to the closing side too: a digest from elsewhere must
    // not seal a height here, and must not advance the head the late-frame rule reads.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    let mut foreign = digest(10, sealed_hash(10), sealed_hash(9), tx_hashes(10, 1), 1_500);
    foreign.chain_id = OTHER_CHAIN;
    let events = seal(&mut radar, foreign, &mut clock);

    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("refused by a radar on chain")),
        "{notes:?}"
    );
    assert_eq!(count(&events, "reconciled"), 0);
    assert_eq!(radar.counters().canonical_closures, 0);
    assert_eq!(events.len(), 1);
    // The view is still open, because nothing sealed it.
    assert_eq!(
        radar.view(BlockNumber(10)).expect("held").stage,
        PreconfStage::Streaming
    );
    // And the head did not move, so a second frame at 10 is still a live view rather
    // than a late one.
    let events = offer(&mut radar, frame_of(10, 0x11, &[POOL_A]), &mut clock);
    assert_eq!(count(&events, "frame_accepted"), 1);
    assert_eq!(radar.counters().frames_late, 0);
}

#[test]
fn a_canonical_reorg_next_to_the_view_is_noted_and_changes_nothing() {
    // §15: a canonical-side reorg is the pipeline's business. The radar records the
    // discontinuity it can see — a digest whose parent is not what it sealed below — and
    // takes no authority.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    seal_next(&mut radar, 10, Vec::new(), &mut clock);
    let reorganised = digest(11, sealed_hash(11), b256(0x77), Vec::new(), clock.tick());
    let events = seal(&mut radar, reorganised, &mut clock);

    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("a canonical-side reorg")
                && note.contains("The radar changes nothing and stays authoritative-free")),
        "{notes:?}"
    );
    assert_eq!(radar.counters().canonical_closures, 2, "both still seal");
    assert_eq!(radar.counters().frames_rejected, 0);
    assert_eq!(count(&events, "reconciled"), 1);
}

#[test]
fn a_reconnect_invalidates_every_open_view_and_starts_a_new_observed_sequence() {
    // §11 + NC7: no cursor and no frame index exist on this transport, so a resumed read
    // cannot be shown to continue the same view. Everything open goes, and the number
    // continuity it was compared against goes with it.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    offer(&mut radar, frame_of(11, 0x11, &[POOL_A]), &mut clock);

    let events = radar.on_disconnect("read budget spent after 8 consecutive failures");
    let invalidated: Vec<(u64, &'static str)> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::ViewInvalidated { number, reason } => Some((*number, *reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        invalidated,
        vec![(10, "transport_reconnect"), (11, "transport_reconnect")]
    );
    assert!(radar.held_heights().is_empty());
    assert_eq!(radar.counters().views_invalidated, 2);
    assert!(radar.records().is_empty(), "no printable view survives");
    let stages: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::StageChanged { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        stages,
        vec![("streaming", "invalidated"), ("streaming", "invalidated")]
    );

    // The next read is a new session's sequence, not a continuation: 10 again is not a
    // regression against a number the reconnect cleared, and it starts a fresh view.
    let events = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    assert_eq!(count(&events, "frame_accepted"), 1);
    assert_eq!(radar.counters().frames_regressed, 0);
    assert_eq!(
        radar.counters().views_replaced_after_closure,
        0,
        "the invalidated view was dropped, not kept and re-opened"
    );
    let state = radar.view(BlockNumber(10)).expect("a fresh view");
    assert_eq!(state.frames_observed, 1);
    assert_eq!(state.stage, PreconfStage::Streaming);
    assert_eq!(
        state.latest.identity.local_frame_sequence, 3,
        "the session counter keeps running — it never claimed to be a provider field"
    );
}

#[test]
fn a_view_ages_out_on_the_cycle_budget_and_a_frame_after_that_starts_a_new_view() {
    // §30's expiry, answered locally because no payload field answers it, and §10's rule
    // that a closed stage cannot be re-opened — the replacement is counted.
    let config = RadarConfig {
        expiry_cycles: 2,
        ..RadarConfig::default()
    };
    let mut radar = radar_with([addr(POOL_A)], config);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);

    assert!(radar.tick().is_empty(), "one cycle is inside the budget");
    assert!(radar.tick().is_empty(), "two cycles is still inside it");
    let events = radar.tick();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RadarEvent::ViewExpired {
                number: 10,
                frames_observed: 1
            }
        )),
        "{events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        RadarEvent::StageChanged {
            number: 10,
            from: "streaming",
            to: "expired"
        }
    )));
    assert_eq!(radar.counters().views_expired, 1);
    assert_eq!(
        radar
            .view(BlockNumber(10))
            .expect("kept so a later frame has something to be compared against")
            .stage,
        PreconfStage::Expired
    );

    let events = offer(&mut radar, frame_of(10, 0x14, &[POOL_A]), &mut clock);
    assert_eq!(radar.counters().views_replaced_after_closure, 1);
    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("had already closed as expired")
                && note.contains("starts a fresh view instead of extending it")),
        "{notes:?}"
    );
    let state = radar.view(BlockNumber(10)).expect("the fresh view");
    assert_eq!(state.frames_observed, 1, "not extended");
    assert_eq!(state.stage, PreconfStage::Streaming);
}

#[test]
fn the_hold_bound_names_the_view_it_threw_away() {
    // A memory guard that silently truncated would turn §51's honesty into a number
    // nobody can check, so the eviction is both counted and described.
    let config = RadarConfig {
        max_heights_held: 2,
        ..RadarConfig::default()
    };
    let mut radar = radar_with([addr(POOL_A)], config);
    let mut clock = StepClock::new(1_000, 5);
    let mut last = Vec::new();
    for (index, number) in [10u64, 11, 12].into_iter().enumerate() {
        last = offer(
            &mut radar,
            frame_of(number, 0x10 + index as u8, &[POOL_A]),
            &mut clock,
        );
    }
    assert_eq!(radar.held_heights(), vec![BlockNumber(11), BlockNumber(12)]);
    assert_eq!(radar.counters().views_invalidated, 1);
    let notes = sequences(&last);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("height 10 dropped after 1 frame(s)")
                && note.contains("not recoverable from the radar")),
        "{notes:?}"
    );
    assert!(last.iter().any(|event| matches!(
        event,
        RadarEvent::ViewInvalidated {
            number: 10,
            reason: "hold_bound_exceeded"
        }
    )));
}

#[test]
fn the_transaction_bound_is_announced_and_the_excess_excluded_from_every_derivation() {
    // §51 again, on the other bound: entries past the cap are named as excluded, and they
    // are excluded from the held list *and* the affected scan, so a pool targeted past
    // the bound is not silently missing.
    let config = RadarConfig {
        max_transactions_per_frame: 2,
        ..RadarConfig::default()
    };
    let mut radar = radar_with([addr(POOL_B)], config);
    let mut clock = StepClock::new(1_000, 5);
    let raw = pending(
        10,
        sealed_hash(9),
        Some(b256(0x10)),
        vec![
            transaction(tx_byte(10, 0), Some(addr(0xf0)), 0),
            transaction(tx_byte(10, 1), Some(addr(0xf0)), 1),
            transaction(tx_byte(10, 2), Some(addr(POOL_B)), 2),
        ],
    );
    let events = offer(&mut radar, raw, &mut clock);

    assert_eq!(radar.counters().frames_truncated, 1);
    assert_eq!(radar.counters().transactions_observed, 2);
    assert_eq!(count(&events, "pool_affected"), 0);
    let notes = sequences(&events);
    assert!(
        notes.iter().any(
            |note| note.contains("holds 3 transactions, over the bound of 2")
                && note.contains("1 entry(ies) past it are counted here")
        ),
        "{notes:?}"
    );
    let state = radar.view(BlockNumber(10)).expect("held");
    assert_eq!(state.transaction_hashes, tx_hashes(10, 2));
    assert_eq!(
        state.latest.transaction_count, 3,
        "what the payload said is kept alongside what the bound allowed"
    );
    assert_eq!(state.latest.transactions.len(), 3);
}

#[test]
fn stamps_are_first_seen_per_height_and_latencies_follow_from_them_alone() {
    // §26's six stamps and §27's derived latencies, on one height, with every value
    // coming from the caller's clock.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);

    let record = radar
        .height_record(BlockNumber(10))
        .expect("the held height has a record");
    // observed 1005, decoded 1010, validated 1015, first pool 1020 — all this clock.
    assert_eq!(
        record.timestamps.flashblock_received_at_unix_ms,
        Some(1_005)
    );
    assert_eq!(record.timestamps.flashblock_decoded_at_unix_ms, Some(1_010));
    assert_eq!(record.timestamps.sequence_validated_at_unix_ms, Some(1_015));
    assert_eq!(
        record.timestamps.affected_pool_detected_at_unix_ms,
        Some(1_020)
    );
    assert_eq!(record.timestamps.chain_timestamp_secs, Some(0x6708_9d20));
    assert_eq!(record.timestamps.canonical_seen_at_unix_ms, None);
    assert_eq!(record.timestamps.reconciled_at_unix_ms, None);
    assert_eq!(record.endpoint_id, ENDPOINT);

    let latencies = record.latencies;
    assert_eq!(latencies.stream_latency_ms, Some(5));
    assert_eq!(latencies.decode_latency_ms, Some(5));
    assert_eq!(latencies.affected_pool_detection_latency_ms, Some(15));
    assert_eq!(latencies.canonical_lag_ms, None, "no canonical block yet");
    assert_eq!(latencies.reconciliation_latency_ms, None);
    assert_eq!(latencies.flashblocks_lead_ms, None);
    assert_eq!(
        latencies.view_growth_ms,
        Some(0),
        "one frame's growth window is genuinely zero, and `frames_observed: 1` is the \
         field that says why — a None here would read as 'not measured'"
    );
    assert_eq!(
        latencies.lead_is_positive(),
        None,
        "missing is None, never 0 (§28)"
    );

    // A positive lead: the canonical read happens later on the same clock.
    let events = seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);
    let (verdict, lead) = match events
        .iter()
        .find(|event| matches!(event, RadarEvent::Reconciled { .. }))
        .expect("the closure is reported")
    {
        RadarEvent::Reconciled {
            verdict, latencies, ..
        } => (*verdict, latencies.flashblocks_lead_ms),
        _ => unreachable!("matched above"),
    };
    assert_eq!(verdict, ReconciliationVerdict::ContentEqual);
    let closed = radar
        .height_record(BlockNumber(10))
        .expect("kept after closure");
    assert_eq!(lead, closed.latencies.flashblocks_lead_ms);
    assert_eq!(
        closed.timestamps.canonical_seen_at_unix_ms,
        Some(1_025),
        "the digest's detection time is this clock's next tick"
    );
    assert_eq!(closed.timestamps.reconciled_at_unix_ms, Some(1_030));
    assert_eq!(closed.latencies.flashblocks_lead_ms, Some(1_025 - 1_005));
    assert_eq!(closed.latencies.lead_is_positive(), Some(true));
    assert_eq!(closed.latencies.reconciliation_latency_ms, Some(5));
    assert_eq!(
        closed.latencies.canonical_lag_ms,
        Some(1_025 - 0x6708_9d20_i64 * 1_000),
        "chain seconds and unix milliseconds are never added"
    );
    assert_eq!(closed.transactions_in_latest, 1);
    assert_eq!(closed.affected_pools, 1);
    assert!(
        !closed.state_root_known,
        "§52: the placeholder never becomes known"
    );
    assert!(
        !closed.wire_index_known,
        "audit §2.3: no frame index on this endpoint"
    );
}

#[test]
fn a_view_read_several_times_reports_its_growth_window() {
    // Beyond §26's list, and the reason it exists: one height read many times *is* the
    // finding, and a single "received" stamp would hide it.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    for index in 0..3u64 {
        offer(
            &mut radar,
            frame_of(10, 0x10 + index as u8, &[POOL_A]),
            &mut clock,
        );
    }
    let record = radar.height_record(BlockNumber(10)).expect("held");
    // Each offer spends two ticks (observed, decoded) and every extend spends one more
    // for the validation stamp; only the first frame takes a pool stamp.
    assert_eq!(
        record.timestamps.flashblock_received_at_unix_ms,
        Some(1_005)
    );
    assert_eq!(
        record.timestamps.flashblock_last_received_at_unix_ms,
        Some(1_040)
    );
    assert_eq!(record.latencies.view_growth_ms, Some(1_040 - 1_005));
    assert_eq!(record.frames_observed, 3);
    assert_eq!(record.distinct_view_hashes, 3);
}

#[test]
fn a_lead_that_is_not_positive_is_recorded_as_itself() {
    // §27: the lead cannot be proven by choosing the window. Here the canonical read is
    // older than the frame describing the same height — the honest answer is a negative
    // number, and `lead_is_positive()` says false rather than hiding it.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(5_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    clock.now = 4_000;
    let events = seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);
    let lead = match events
        .iter()
        .find(|event| matches!(event, RadarEvent::Reconciled { .. }))
        .expect("the closure is reported")
    {
        RadarEvent::Reconciled { latencies, .. } => latencies.flashblocks_lead_ms,
        _ => unreachable!("matched above"),
    };
    assert_eq!(lead, Some(4_005 - 5_005));
    let record = radar.height_record(BlockNumber(10)).expect("kept");
    assert_eq!(record.latencies.lead_is_positive(), Some(false));
    assert_eq!(
        record.latencies.canonical_lag_ms,
        Some(4_005 - 0x6708_9d20_i64 * 1_000)
    );
}

#[test]
fn records_come_out_in_chain_order_whatever_order_the_frames_arrived_in() {
    // §41/§43: the sequence table's order is the chain's, not a hash map's. Frames are
    // offered high-to-low and the table comes out low-to-high, twice.
    let mut first = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    for (index, number) in [13u64, 11, 12].into_iter().enumerate() {
        offer(
            &mut first,
            frame_of(number, 0x10 + index as u8, &[POOL_A]),
            &mut clock,
        );
    }
    let heights: Vec<u64> = first.records().iter().map(|record| record.number).collect();
    assert_eq!(heights, vec![11, 12, 13]);

    let mut second = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    for (index, number) in [12u64, 13, 11].into_iter().enumerate() {
        offer(
            &mut second,
            frame_of(number, 0x10 + index as u8, &[POOL_A]),
            &mut clock,
        );
    }
    // The order-proof is on `record_shape`'s projection: the stamps are deliberately
    // not in it, because they are this process's wall clock and are read-order
    // dependent by construction, and a table that claimed otherwise would be claiming
    // the clock is the chain's.
    assert_eq!(
        record_shape(&second),
        record_shape(&first),
        "the table depends on the set of heights, not on the order they were read"
    );
    // The counters are not the same claim: they count *transitions*, and a different
    // read order crosses them in a different order — so the equality that matters here
    // is on the height-keyed table above, and this asserts the counter that does differ.
    assert_ne!(
        serde_json::to_string(first.counters()).expect("counters serialize"),
        serde_json::to_string(second.counters()).expect("counters serialize"),
        "the order-sensitive counters are visibly order-sensitive, not hidden"
    );
}

#[test]
fn a_full_window_from_pending_frames_to_a_sealed_block_moves_through_every_stage_once() {
    // §46's integration row: frames grow, the block seals, the view closes. Stage
    // changes are the machine's edges, and each height passes through them once.
    let mut radar = radar([addr(POOL_A), addr(POOL_B)]);
    let mut clock = StepClock::new(1_000, 5);
    let opened = offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    let stages: Vec<(&str, &str)> = opened
        .iter()
        .filter_map(|event| match event {
            RadarEvent::StageChanged { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(
        stages,
        vec![("empty", "streaming")],
        "the first frame opens it"
    );

    let grown = offer(
        &mut radar,
        frame_of(10, 0x11, &[POOL_A, POOL_B]),
        &mut clock,
    );
    assert_eq!(count(&grown, "pool_affected"), 1, "the second pool is new");
    assert_eq!(count(&grown, "stage_changed"), 0, "already streaming");

    let events = seal_next(&mut radar, 10, tx_hashes(10, 2), &mut clock);
    let stages: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::StageChanged { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(stages, vec![("streaming", "reconciled")]);
    let record = radar.height_record(BlockNumber(10)).expect("kept");
    assert_eq!(record.stage, PreconfStage::Reconciled);
    assert_eq!(record.frames_observed, 2);

    // And the refused path leaves the machine alone: a wrong-parent frame cannot move a
    // stage, because it never becomes a view.
    let events = offer(
        &mut radar,
        pending(11, b256(0x77), Some(b256(0x12)), Vec::new()),
        &mut clock,
    );
    assert_eq!(refusals(&events), vec![(11, "wrong_parent")]);
    assert_eq!(count(&events, "stage_changed"), 0);
    assert!(!radar.held_heights().contains(&BlockNumber(11)));

    // §10's edge set, closed over: no stage moves to itself, and a closed stage moves
    // nowhere at all — which is why the assertions above are structure, not luck.
    let all = [
        PreconfStage::Empty,
        PreconfStage::Streaming,
        PreconfStage::Reconciled,
        PreconfStage::Superseded,
        PreconfStage::Invalidated,
        PreconfStage::Expired,
        PreconfStage::DiscardedLate,
    ];
    for from in all {
        assert!(
            !from.can_move_to(from),
            "{} is not an edge to itself",
            from.as_str()
        );
        for to in all {
            if from.is_closed() {
                assert!(
                    !from.can_move_to(to),
                    "{} -> {} must be refused",
                    from.as_str(),
                    to.as_str()
                );
            } else if from == PreconfStage::Empty {
                assert_eq!(
                    from.can_move_to(to),
                    to == PreconfStage::Streaming,
                    "empty only opens by streaming"
                );
            }
        }
    }
}
