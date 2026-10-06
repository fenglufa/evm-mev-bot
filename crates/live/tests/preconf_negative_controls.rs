//! M9.4 §29: the twelve negative controls, one test each, named `nc1_…` … `nc12_…`.
//!
//! A negative control is not a second copy of a matrix row. The matrix asks "does the
//! radar do the thing"; each of these asks "the thing that must *not* have happened —
//! did it not happen?", and answers against the field that would carry it: a duplicate
//! that re-emitted a pool, a gap that invented a height, a reverted receipt that named
//! an emitter, a late frame that moved a closed view. Where the transport cannot answer
//! the question at all (§29's NC2 and NC3 both assume a frame index, and the measured
//! endpoint provides none — audit §2.3), the control asserts that the code *says* it
//! cannot answer, rather than reporting a clean result.
//!
//! Two controls (NC5, NC7) are driven through the link rather than the radar, because the
//! defect they name lives in the transport: a malformed payload at the decode boundary,
//! and a session boundary.

mod preconf_support;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::mpsc;

use preconf_support::*;

use evm_core::BlockNumber;
use evm_live::{
    frame_from_pending_value, AffectedReason, LinkConfig, LinkEndedBy, LinkRunReport, PreconfError,
    PreconfLink, PreconfStage, RadarEvent, RadarInput, ReconciliationVerdict,
};

const POOL_A: u8 = 0xa1;
const POOL_C: u8 = 0xb2;
const FOREIGN: u8 = 0xb7;

#[test]
fn nc1_duplicate_frame_is_not_applied_twice() {
    // §29's sequence `0,1,2,2,3`. The index in that literal is not a wire field on this
    // endpoint, so the same shape arrives as the same *view hash* at the same height —
    // which is what a re-read of an unchanged pending block actually looks like.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    for number in [10u64, 11] {
        offer(
            &mut radar,
            frame_of(number, number as u8, &[POOL_A]),
            &mut clock,
        );
    }
    assert!(
        radar.view(BlockNumber(12)).is_none(),
        "height 12 has not been read yet"
    );

    let frame = frame_of(12, 0x12, &[POOL_A]);
    offer(&mut radar, frame.clone(), &mut clock);
    let first_pools = radar
        .view(BlockNumber(12))
        .expect("held")
        .affected_pools
        .clone();
    let emitted_after_first = radar.counters().affected_pools_emitted;

    let events = offer(&mut radar, frame.clone(), &mut clock);
    let after = radar.view(BlockNumber(12)).expect("still held");

    assert_eq!(refusals(&events), Vec::new(), "a repeat is not an error");
    assert!(
        events.iter().any(|event| matches!(
            event,
            RadarEvent::FrameAccepted {
                number: 12,
                duplicate_view_hash: true,
                ..
            }
        )),
        "and it is labelled as a repeat: {events:?}"
    );
    assert_eq!(
        radar.counters().affected_pools_emitted,
        emitted_after_first,
        "the second application emitted no pool finding"
    );
    assert_eq!(
        after.affected_pools, first_pools,
        "the held view's affected set is what the first frame made"
    );
    assert_eq!(radar.counters().frames_duplicate, 1);
    assert_eq!(
        after.transaction_hashes,
        tx_hashes(12, 1),
        "the transaction list was replaced by the same one-item list, not appended to"
    );

    // …and the run continues after the duplicate: 13 is still accepted.
    let events = offer(&mut radar, frame_of(13, 0x13, &[POOL_A]), &mut clock);
    assert_eq!(accepted_heights(&events), vec![13]);
}

#[test]
fn nc2_height_skip_is_announced_and_never_reconstructed() {
    // §29 asks for the gap in `0,1,3` to be *detected*. Two answers are recorded, because
    // the endpoint supports only one of them: a skipped height is announced (§9), and a
    // missing frame *within* a height is NOT_MEASURABLE, since no frame index exists
    // whose absence could be checked (audit §2.3).
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    let events = offer(&mut radar, frame_of(12, 0x12, &[POOL_A]), &mut clock);

    let notes = sequences(&events);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("pending moved 10 -> 12")),
        "the skip is announced: {notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains("NOT_MEASURABLE")),
        "and it is measured as unmeasurable, not as zero: {notes:?}"
    );
    assert!(!radar.counters().sequence_gaps_measurable);
    assert_eq!(
        radar.held_heights(),
        vec![BlockNumber(10), BlockNumber(12)],
        "height 11 was not invented to fill the hole"
    );
    assert!(
        radar.view(BlockNumber(11)).is_none(),
        "there is no view at 11, so nothing downstream can read one"
    );
    assert_eq!(radar.counters().heights_seen, 2);
}

#[test]
fn nc3_frame_and_receipt_naming_another_block_are_both_refused() {
    // §29's "block identity 不一致" has two witnesses on this stack, and neither is a
    // frame index: a *late* frame (its height already sealed) and a receipt claiming a
    // block other than the frame it was read with.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);

    let events = offer(&mut radar, frame_of(10, 0x90, &[POOL_A]), &mut clock);
    assert_eq!(refusals(&events), vec![(10, "late_after_canonical")]);
    assert_eq!(radar.counters().frames_late, 1);
    assert!(
        affected(&events).is_empty(),
        "the wrong-block frame contributed no pool"
    );

    // The receipt side, at a live height: a receipt naming height 99 while the frame
    // being extended is 11.
    offer(&mut radar, frame_of(11, 0x11, &[POOL_A]), &mut clock);
    let foreign_receipt = receipt(
        b256(0x31),
        99,
        sealed_hash(99),
        Some(1),
        vec![log(addr(POOL_A), b256(0x31), 0, 0, false)],
    );
    let events = offer_with(
        &mut radar,
        frame_of(11, 0x15, &[POOL_A]),
        &mut clock,
        Some(vec![foreign_receipt]),
    );
    assert_eq!(radar.counters().receipts_wrong_block, 1);
    assert!(
        sequences(&events)
            .iter()
            .any(|note| note.contains("claims block 99")),
        "the mismatch is described: {:?}",
        sequences(&events)
    );
    assert_eq!(
        radar
            .view(BlockNumber(11))
            .expect("the frame is still held")
            .affected_pools
            .iter()
            .filter(|pool| pool.reason == AffectedReason::SuccessfulLogEmitter)
            .count(),
        0,
        "a receipt from another block names nothing here"
    );
}

#[test]
fn nc4_wrong_parent_invalidates_and_does_not_merge() {
    // §9 + §24: the reset has to be a reset. The check only becomes meaningful once the
    // height below is actually sealed, so the fixture seals 10 first.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);
    offer(&mut radar, frame_of(11, 0x11, &[POOL_A]), &mut clock);
    assert!(radar.held_heights().contains(&BlockNumber(11)));

    // Same height, parent claiming a block this session never sealed.
    let mut wrong = frame_of(11, 0x19, &[POOL_A]);
    wrong["parentHash"] = serde_json::Value::String(hex32(0xee));
    let events = offer(&mut radar, wrong, &mut clock);

    assert_eq!(refusals(&events), vec![(11, "wrong_parent")]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                RadarEvent::ViewInvalidated {
                    number: 11,
                    reason: "wrong_parent"
                }
            ))
            .count(),
        1,
        "the held view was thrown away, not patched"
    );
    assert!(!radar.held_heights().contains(&BlockNumber(11)));
    assert_eq!(radar.counters().frames_wrong_parent, 1);
    assert!(
        affected(&events).is_empty(),
        "and the refused frame emitted nothing on the way out"
    );
}

#[test]
fn nc5_malformed_payload_fails_closed_at_the_decoder() {
    // §29 + §6: the decoder refuses; it does not substitute a default. Every case here is
    // a payload a node could answer, and each must return an error rather than a frame
    // with a hole filled in.
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "number missing entirely",
            serde_json::json!({"hash": hex32(1)}),
        ),
        (
            "number is not a quantity",
            serde_json::json!({"number": "0xzz", "hash": hex32(1)}),
        ),
        (
            "transactions is a hash-only list",
            serde_json::json!({"number": "0xa", "transactions": [hex32(1)]}),
        ),
        (
            "transactions is not an array",
            serde_json::json!({"number": "0xa", "transactions": "0x"}),
        ),
        ("payload is not an object", serde_json::json!(["pending"])),
    ];
    for (label, raw) in cases {
        let outcome = frame_from_pending_value(CHAIN, &raw, 1, 1_005, ENDPOINT);
        assert!(
            matches!(
                outcome,
                Err(PreconfError::Decode(_) | PreconfError::HashOnlyTransaction)
            ),
            "{label}: got {outcome:?}, expected a fail-closed error"
        );
    }

    // The honest counterpart: the same fields present and well-formed do decode, so the
    // refusals above are about missing data, not about a parser that never works.
    let good = frame_from_pending_value(CHAIN, &frame_of(10, 0x10, &[POOL_A]), 1, 1_005, ENDPOINT)
        .expect("the fixture decodes");
    assert_eq!(good.number(), BlockNumber(10));
}

#[tokio::test]
async fn nc5_continued_a_malformed_read_does_not_end_the_run_or_clear_the_view() {
    // The same defect at the transport boundary: one good frame, one malformed payload,
    // one more good frame. The point is the third step — a decode failure is counted and
    // the run carries on with the view it already held.
    let script = vec![
        Step::frame(frame_of(20, 0x20, &[POOL_A])),
        Step::malformed(),
        Step::frame(frame_of(20, 0x21, &[POOL_A])),
        Step::null(),
    ];
    let (report, events) = run_link(script).await;

    assert_eq!(
        report.decode_failures, 1,
        "the bad payload is a counted failure"
    );
    assert_eq!(
        report.read_failures, 0,
        "a bad payload is not a broken transport"
    );
    assert_eq!(
        report.ended_by,
        Some(LinkEndedBy::Exhausted),
        "the run finished by itself, not on a failure budget: {events:?}"
    );
    assert_eq!(
        report.counters.frames_accepted, 2,
        "both good frames reached the radar"
    );
    assert_eq!(
        report.frames_offered_to_radar, 2,
        "the malformed payload was never offered"
    );
    assert_eq!(
        report.counters.frames_offered, 2,
        "so the radar's own denominator stays honest about what it saw"
    );
}

#[test]
fn nc6_wrong_chain_is_refused_before_anything_is_held() {
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let raw = frame_of(10, 0x10, &[POOL_A]);
    let foreign = frame_from_pending_value(OTHER_CHAIN, &raw, 1, 1_005, ENDPOINT)
        .expect("the same bytes, another chain id");
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
    assert_eq!(radar.counters().heights_seen, 0);
    assert_eq!(radar.counters().affected_pools_emitted, 0);
    assert_eq!(
        radar.counters().transactions_observed,
        0,
        "not even a transaction count was credited to it"
    );
}

#[tokio::test]
async fn nc7_giving_up_on_the_transport_leaves_no_state_to_resume() {
    // §11 + §29: the link's own session boundary is the read budget being spent. At that
    // point every open view is discarded *before* the run reports, so nothing downstream
    // can resume a state that spans two sessions — and no event may claim frame
    // continuity across the boundary, because this transport has no cursor to prove it.
    let mut config = LinkConfig {
        read_interval_ms: 0,
        ..LinkConfig::default()
    };
    config.max_consecutive_read_failures = 2;
    let script = vec![
        Step::frame(frame_of(30, 0x30, &[POOL_A])),
        Step::frame(frame_of(31, 0x31, &[POOL_A])),
        Step::failed("connection reset"),
        Step::failed("connection reset"),
        Step::frame(frame_of(32, 0x32, &[POOL_A])),
    ];
    let (report, events) = run_link_with(script, config).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::ReadFailures));
    assert_eq!(report.link_resets, 1, "one reset, described once");
    assert_eq!(report.read_failures, 2);
    assert_eq!(
        count(&events, "view_invalidated"),
        2,
        "both open views discarded: {events:?}"
    );
    assert_eq!(report.counters.views_invalidated, 2);
    assert!(
        sequences(&events)
            .iter()
            .any(|note| note.contains("no cursor exists on this transport")),
        "and the record says *why* a resume cannot be trusted"
    );
    assert!(
        !sequences(&events)
            .iter()
            .any(|note| note.contains("pending moved")),
        "no cross-boundary continuity is claimed: {:?}",
        sequences(&events)
    );
    assert_eq!(
        report.counters.frames_accepted, 2,
        "the third frame was never read — stopping is the finding"
    );
}

#[tokio::test]
async fn nc7_continued_a_transient_read_failure_is_not_dressed_up_as_a_reset() {
    // The other half of the same rule, and the half that is easy to get wrong in the
    // opposite direction: an HTTP poll has no session to break, so one failed read inside
    // the budget is a *retry*. Fabricating a reset there would discard live views — a
    // cross-session claim in the other direction, dressed as caution.
    let script = vec![
        Step::frame(frame_of(30, 0x30, &[POOL_A])),
        Step::failed("connection reset"),
        Step::frame(frame_of(31, 0x31, &[POOL_A])),
        Step::null(),
    ];
    let (report, events) = run_link(script).await;

    assert_eq!(report.read_failures, 1);
    assert_eq!(report.link_resets, 0, "one failure is not a spent budget");
    assert_eq!(
        count(&events, "view_invalidated"),
        0,
        "so no view was thrown away: {events:?}"
    );
    assert_eq!(
        report.ended_by,
        Some(LinkEndedBy::Exhausted),
        "and the run finished normally"
    );
    assert_eq!(
        report.counters.frames_accepted, 2,
        "the height-31 frame after the failure was applied"
    );
    // The local sequence is explicitly a *session-local counter* (§8), so it keeps
    // counting across the failure without ever claiming to be a provider field.
    let sequences: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::FrameAccepted {
                local_frame_sequence,
                ..
            } => Some(*local_frame_sequence),
            _ => None,
        })
        .collect();
    assert_eq!(
        sequences,
        vec![1, 2],
        "assigned in read order, gap included"
    );
}

#[test]
fn nc8_canonical_mismatch_makes_canonical_win_with_no_merge() {
    // §17: the verdict classes differ only in what is recorded about the view, and none of
    // them lets the view rewrite the block.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    // The view holds the fixture's two transactions for height 10; the sealed block holds
    // the first plus a hash the view never named — divergent by content, not by order.
    offer(
        &mut radar,
        frame_of(10, 0x10, &[POOL_A, POOL_A]),
        &mut clock,
    );
    let sealed = vec![b256(tx_byte(10, 0)), b256(0x99)];
    let events = seal_next(&mut radar, 10, sealed.clone(), &mut clock);

    assert!(
        matches!(
            events
                .iter()
                .find(|event| event.kind_label() == "reconciled")
                .expect("the closure is reported"),
            RadarEvent::Reconciled {
                verdict: ReconciliationVerdict::ContentDivergent {
                    only_in_view: 1,
                    only_in_canonical: 1
                },
                hash_matched: false,
                ..
            }
        ),
        "{events:?}"
    );
    let state = radar.view(BlockNumber(10)).expect("kept for the record");
    assert_eq!(state.stage, PreconfStage::Superseded, "canonical won");
    assert_eq!(
        state.transaction_hashes,
        tx_hashes(10, 2),
        "the view was NOT rewritten to look like the block — no merge"
    );
    assert!(
        events
            .iter()
            .all(|event| event.kind_label() != "pool_affected"),
        "closing a view never emits a new pool finding"
    );
}

#[test]
fn nc9_late_flashblock_does_not_pollute_the_closed_view() {
    // §19: compare for the record, then discard. The identical before/after is the whole
    // control — one mutated field here would be the pollution §51 forbids.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    offer(&mut radar, frame_of(10, 0x10, &[POOL_A]), &mut clock);
    seal_next(&mut radar, 10, tx_hashes(10, 1), &mut clock);
    let before = radar
        .view(BlockNumber(10))
        .expect("kept after closure")
        .clone();

    let events = offer(&mut radar, frame_of(10, 0x77, &[POOL_C]), &mut clock);
    let after = radar.view(BlockNumber(10)).expect("still kept").clone();

    assert_eq!(refusals(&events), vec![(10, "late_after_canonical")]);
    assert_eq!(after, before, "nothing was merged in");
    assert_eq!(radar.counters().late_frames_compared, 1);
    assert_eq!(radar.counters().frames_late, 1);
    assert_eq!(
        radar.view(BlockNumber(10)).expect("the closed view").stage,
        PreconfStage::Reconciled,
        "the verdict the canonical block produced is not relabelled by a late frame: §10's          closed stages have no outgoing edge, which is the structural half of §19"
    );
    assert!(
        affected(&events).is_empty(),
        "the late frame's pool target was not emitted either"
    );
}

#[test]
fn nc10_unrelated_transaction_produces_no_affected_pool() {
    // §12: a pool is registered or it is not. A transaction to an address the pool set
    // does not contain is traffic, not a finding.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = offer(
        &mut radar,
        frame_of(10, 0x10, &[FOREIGN, 0, FOREIGN]),
        &mut clock,
    );

    assert!(
        affected(&events).is_empty(),
        "not one entry, and not one with a null pool either"
    );
    assert_eq!(radar.counters().affected_pools_emitted, 0);
    assert_eq!(
        radar.counters().transactions_observed,
        3,
        "the transactions were still counted — the frame is real, it touches nothing"
    );
    assert_eq!(accepted_heights(&events), vec![10]);

    // …and the positive control: the same fixture with the middle entry aimed at the
    // registered pool does emit, so the zero above is about the address, not about a scan
    // that never fires.
    let mut control = preconf_support::radar([addr(POOL_A)]);
    let events = offer(
        &mut control,
        frame_of(10, 0x10, &[FOREIGN, POOL_A, FOREIGN]),
        &mut clock,
    );
    assert_eq!(affected(&events).len(), 1);
}

#[test]
fn nc11_reverted_receipt_is_never_read_as_a_state_mutation() {
    // §14 + §29: a log emitted by a registered pool inside a reverted receipt must not
    // reach the affected set, and a receipt whose status the payload never named is
    // `Unknown` — which likewise is not a confirmed effect.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let tx = b256(tx_byte(10, 0));

    let reverted = receipt(
        tx,
        10,
        sealed_hash(10),
        Some(0),
        vec![log(addr(POOL_A), tx, 0, 0, false)],
    );
    let events = offer_with(
        &mut radar,
        frame_of(10, 0x10, &[0]),
        &mut clock,
        Some(vec![reverted]),
    );
    assert!(
        affected(&events).is_empty(),
        "a reverted log is not a pool finding: {events:?}"
    );
    assert_eq!(radar.counters().reverted_receipts_skipped, 1);

    // status omitted — the payload did not say.
    let silent = receipt(
        b256(0x42),
        10,
        sealed_hash(10),
        None,
        vec![log(addr(POOL_A), b256(0x42), 0, 0, false)],
    );
    let events = offer_with(
        &mut radar,
        frame_of(10, 0x11, &[0]),
        &mut clock,
        Some(vec![silent]),
    );
    assert!(
        affected(&events).is_empty(),
        "an unspecified status is not a confirmed effect either"
    );
    assert_eq!(radar.counters().reverted_receipts_skipped, 2);

    // Positive control: the identical log under a success status does fire.
    let mut control = preconf_support::radar([addr(POOL_A)]);
    let succeeded = receipt(
        tx,
        10,
        sealed_hash(10),
        Some(1),
        vec![log(addr(POOL_A), tx, 0, 0, false)],
    );
    let events = offer_with(
        &mut control,
        frame_of(10, 0x10, &[0]),
        &mut clock,
        Some(vec![succeeded]),
    );
    let pools = affected(&events);
    assert_eq!(pools.len(), 1, "{events:?}");
    assert_eq!(
        pools[0].reason,
        AffectedReason::SuccessfulLogEmitter,
        "labelled as the strength of evidence it is"
    );
}

#[test]
fn nc12_three_changes_to_one_pool_keep_three_identities() {
    // §29's `Pool A / Pool A / Pool A` and §13's ban on a set that collapses them: the
    // dedup key carries the transaction and the log index, so three transactions are
    // three findings, and a re-read of the same ones is still three.
    let mut radar = radar([addr(POOL_A)]);
    let mut clock = StepClock::new(1_000, 5);
    let events = offer(
        &mut radar,
        frame_of(10, 0x10, &[POOL_A, POOL_A, POOL_A]),
        &mut clock,
    );
    let pools = affected(&events);

    assert_eq!(pools.len(), 3, "not deduplicated to one 'pool was touched'");
    let keys: std::collections::BTreeSet<_> =
        pools.iter().map(|pool| pool.identity_key()).collect();
    assert_eq!(
        keys.len(),
        3,
        "each entry is distinct by identity, not just by count"
    );
    let distinct_transactions: std::collections::BTreeSet<_> =
        pools.iter().map(|pool| pool.transaction_hash).collect();
    assert_eq!(distinct_transactions.len(), 3);
    assert_eq!(radar.counters().affected_pools_emitted, 3);
    assert_eq!(
        pools[0].pool, pools[2].pool,
        "and they are the same pool — the multiplicity is in the events, not the address"
    );

    // A re-read of the same frame adds nothing (NC1's rule, applied to a busy height).
    let before = radar
        .view(BlockNumber(10))
        .expect("held")
        .affected_pools
        .clone();
    let events = offer(
        &mut radar,
        frame_of(10, 0x10, &[POOL_A, POOL_A, POOL_A]),
        &mut clock,
    );
    assert!(affected(&events).is_empty());
    assert_eq!(
        radar.view(BlockNumber(10)).expect("held").affected_pools,
        before
    );

    // …and the growth case: a fourth transaction on the next view adds exactly one
    // finding, which is what a set-based dedup would have thrown away.
    let events = offer(
        &mut radar,
        frame_of(10, 0x11, &[POOL_A, POOL_A, POOL_A, POOL_A]),
        &mut clock,
    );
    let fresh = affected(&events);
    assert_eq!(fresh.len(), 1, "{fresh:?}");
    assert_eq!(fresh[0].transaction_hash, b256(tx_byte(10, 3)));
}

/// Drive a scripted source through the link over the fixture radar, and collect the run
/// report plus everything the run emitted. The canonical channel has no sender-side
/// producer here: these controls are about the preconfirmation side.
async fn run_link(script: Vec<Step>) -> (LinkRunReport, Vec<RadarEvent>) {
    run_link_with(
        script,
        LinkConfig {
            read_interval_ms: 0,
            ..LinkConfig::default()
        },
    )
    .await
}

async fn run_link_with(script: Vec<Step>, config: LinkConfig) -> (LinkRunReport, Vec<RadarEvent>) {
    let mut link = PreconfLink::new(
        ScriptedSource::new(script, false),
        radar([addr(POOL_A)]),
        config,
    );
    let (tx, rx) = mpsc::channel(256);
    let (_ctx, crx) = mpsc::channel(64);
    let mut clock = StepClock::new(1_000, 5);
    let mut tick = move || clock.tick();
    let report = link
        .run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
        .await
        .expect("a scripted run finishes");
    (report, drain(rx).await)
}
