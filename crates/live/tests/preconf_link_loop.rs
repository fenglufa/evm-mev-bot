//! §42 (concurrency), §43 (ordering), §10/§11 (the machine and the reconnect), §25
//! (failure policy), §34 (no new production RPC) and §49 (evidence from a failed run),
//! tested at the layer that owns them: the link, driven against a scripted
//! [`FrameSource`].
//!
//! Two of these tests are load-bearing for claims made outside this file:
//!
//! * the read counter is §34's witness — a run performs one read per payload it applies
//!   and one read that discovers the supply ended, so an RPC count in the evidence tables
//!   can be derived from the script rather than asserted;
//! * the determinism test replays the same script through the same caller-supplied clock
//!   twice and compares the serialized events *and* the run report, which is what lets a
//!   replayed recording count as the same evidence as the live run (§41).
//!
//! No pool state, no reserves, no opportunity appears here (§43's ban on fabricated market
//! data). Pool addresses are fixture bytes, and the only claims made about them are which
//! ones a frame named.

mod preconf_support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;
use tokio::sync::mpsc;

use evm_core::BlockNumber;
use evm_live::{
    CanonicalDigest, EarlyRadar, LinkConfig, LinkEndedBy, LinkRunReport, LinkStage, PoolSet,
    PreconfError, PreconfLink, RadarConfig, RadarEvent, ReconciliationVerdict,
};

use preconf_support::{
    accepted_heights, addr, b256, count, digest, drain, hex32_disp, labels, pending, swap_topic,
    transaction, PacedSource, ScriptedSource, Step, StepClock, CHAIN, ENDPOINT,
};

/// A radar with two registered pools, so an affected-pool claim has something to be true
/// about without inventing a market.
fn radar() -> EarlyRadar {
    EarlyRadar::new(
        CHAIN,
        ENDPOINT,
        PoolSet::new(CHAIN, [addr(0xa1), addr(0xa2)]),
        RadarConfig::default(),
    )
}

/// A link with no sleeping: these tests are about ordering and state, and a sleep would
/// only make them slower.
fn link_with(
    script: Vec<Step>,
    receipts_supported: bool,
    config: LinkConfig,
) -> PreconfLink<ScriptedSource> {
    PreconfLink::new(
        ScriptedSource::new(script, receipts_supported),
        radar(),
        config,
    )
}

fn fast_config() -> LinkConfig {
    LinkConfig {
        read_interval_ms: 0,
        ..LinkConfig::default()
    }
}

/// One pending height, growing: the shape the measured endpoint actually produces. The
/// view hash and the transaction hashes are functions of the count, so a digest can be
/// written to match a specific shape exactly.
fn frame_payload(number: u64, tx_count: usize) -> serde_json::Value {
    pending(
        number,
        b256(0x90),
        Some(b256(0x10 + tx_count as u8)),
        (0..tx_count)
            .map(|index| transaction(index as u8 + 1, Some(addr(0xa1)), index as u64))
            .collect(),
    )
}

fn frame(number: u64, tx_count: usize) -> Step {
    Step::frame(frame_payload(number, tx_count))
}

/// The transaction hashes `frame_payload` put in a block of this shape.
fn payload_hashes(tx_count: usize) -> Vec<alloy_primitives::B256> {
    (1..=tx_count as u8).map(b256).collect()
}

/// The stage moves a run emitted, as `(from, to)` pairs in emission order.
fn stage_moves(events: &[RadarEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::LinkStageChanged { from, to, .. } => {
                Some(((*from).to_string(), (*to).to_string()))
            }
            _ => None,
        })
        .collect()
}

/// The label -> stage map, total over the labels the machine can emit. An unknown label is
/// a test failure rather than a silent pass, which is what makes the edge-table invariant
/// below worth asserting.
fn stage(name: &str) -> LinkStage {
    match name {
        "disconnected" => LinkStage::Disconnected,
        "connecting" => LinkStage::Connecting,
        "connected" => LinkStage::Connected,
        "receiving" => LinkStage::Receiving,
        "validating" => LinkStage::Validating,
        "recovering" => LinkStage::Recovering,
        "reset" => LinkStage::Reset,
        other => unreachable!("`{other}` is not a link stage label"),
    }
}

/// Drive a scripted link to completion and collect everything it emitted.
async fn run(
    link: &mut PreconfLink<ScriptedSource>,
    canonical: Vec<CanonicalDigest>,
) -> (LinkRunReport, Vec<RadarEvent>) {
    let (tx, rx) = mpsc::channel(256);
    let (ctx, crx) = mpsc::channel(64);
    for sealed in canonical {
        let _ = ctx.send(sealed).await;
    }
    // The canonical sender is dropped here on purpose: a digest that a test queued is the
    // only canonical input that run will ever see, and a link waiting for more would be
    // waiting for something this fixture never promised.
    drop(ctx);
    let mut clock = StepClock::new(1_000, 5);
    let mut tick = move || clock.tick();
    let report = link
        .run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
        .await
        .expect("a scripted run completes");
    (report, drain(rx).await)
}

#[tokio::test]
async fn a_replayed_window_is_applied_in_read_order_and_ends_by_exhaustion() {
    let script = vec![
        frame(10, 1),
        frame(10, 2),
        frame(11, 1),
        frame(11, 3),
        frame(12, 1),
    ];
    let mut link = link_with(script, false, fast_config());
    let (report, events) = run(&mut link, Vec::new()).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::Exhausted));
    assert_eq!(report.frames_offered_to_radar, 5);
    // §42/§43: the heights come back in the order they were read, repeats included, and the
    // local sequence numbers are 1..=5 with no gap and no reordering. One consumer applied
    // them, in order, to one state per height.
    assert_eq!(accepted_heights(&events), vec![10, 10, 11, 11, 12]);
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
    assert_eq!(sequences, vec![1, 2, 3, 4, 5]);
    assert_eq!(report.final_stage, LinkStage::Disconnected);
    assert_eq!(report.counters.frames_accepted, 5);
    assert_eq!(report.counters.frames_rejected, 0);
    // The event stream is what a table is built from, so its shape is asserted too: the run
    // opens with the machine starting and ends with it closing.
    let kinds = labels(&events);
    assert_eq!(kinds.first().copied(), Some("link_stage_changed"));
    assert_eq!(kinds.last().copied(), Some("link_stage_changed"));
}

#[tokio::test]
async fn every_link_stage_move_a_run_emits_is_an_edge_of_the_table() {
    // §10 as an invariant rather than a wish: whatever a run does, no stage transition it
    // reports may be outside the declared edge set — including the failure paths.
    let script = vec![
        frame(10, 1),
        Step::failed("provider unavailable"),
        frame(10, 2),
        Step::null(),
    ];
    let mut config = fast_config();
    config.max_consecutive_read_failures = 8;
    config.idle_after_reads = 1;
    let mut link = link_with(script, false, config);
    let (report, events) = run(&mut link, Vec::new()).await;

    let moves = stage_moves(&events);
    assert!(
        !moves.is_empty(),
        "a run that read anything must have reported its machine moving"
    );
    for (from, to) in &moves {
        assert!(
            stage(from).can_move_to(stage(to)),
            "§10 edge violated: {from} -> {to}"
        );
    }
    // The cold start is spelled out, because "Disconnected -> Connecting -> Connected"
    // being observable is what makes §10's machine testable at all.
    assert_eq!(
        &moves[..4],
        &[
            ("disconnected".to_string(), "connecting".to_string()),
            ("connecting".to_string(), "connected".to_string()),
            ("connected".to_string(), "receiving".to_string()),
            ("receiving".to_string(), "validating".to_string()),
        ]
    );
    // A read failure goes through `Recovering`, and the read that follows it is a fresh
    // `Connecting` — not a claim that the failed session continued.
    assert!(moves
        .iter()
        .any(|(from, to)| from == "connecting" && to == "recovering"));
    assert!(moves
        .iter()
        .any(|(from, to)| from == "recovering" && to == "connecting"));
    assert_eq!(report.ended_by, Some(LinkEndedBy::Exhausted));
    assert_eq!(count(&events, "link_stage_changed"), moves.len());
}

#[tokio::test]
async fn the_link_performs_one_read_per_payload_it_applies() {
    // §34's witness, measured rather than asserted: five payloads and the one read that
    // discovers the recording is empty. Nothing else was asked of the provider.
    let script = vec![
        frame(10, 1),
        frame(10, 2),
        frame(11, 1),
        frame(11, 3),
        frame(12, 1),
    ];
    let mut link = link_with(script, false, fast_config());
    let (report, _) = run(&mut link, Vec::new()).await;
    assert_eq!(link.source().reads, 6);
    assert_eq!(report.reads_answered, 5);
    assert_eq!(report.reads_empty, 1);
    assert_eq!(report.cycles, 6);
    // A live-shaped source (no receipts) decodes none either: the extra method is not
    // called because it is not offered, which is the §34 rule rather than an oversight.
    assert_eq!(report.receipt_lists_decoded, 0);
}

#[tokio::test]
async fn a_digest_already_sealed_before_any_frame_closes_as_no_view_and_later_frames_are_discarded()
{
    // §19 + NC9: canonical first. The frames that follow must be compared and thrown away,
    // never merged into the sealed block.
    let script = vec![frame(10, 1), frame(10, 2)];
    let mut link = link_with(script, false, fast_config());
    let sealed = digest(10, b256(0x12), b256(0x90), payload_hashes(2), 2_000);
    let (report, events) = run(&mut link, vec![sealed]).await;

    assert_eq!(report.canonical_digests_consumed, 1);
    assert_eq!(report.counters.reconciled_no_view, 1);
    assert_eq!(count(&events, "reconciled"), 1);
    assert_eq!(report.counters.frames_late, 2);
    assert_eq!(report.counters.frames_accepted, 0);
    assert_eq!(accepted_heights(&events), Vec::<u64>::new());
    let verdicts: Vec<ReconciliationVerdict> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::Reconciled { verdict, .. } => Some(*verdict),
            _ => None,
        })
        .collect();
    assert_eq!(verdicts, vec![ReconciliationVerdict::NoView]);
    // §17 in code: every verdict class answers "who wins" the same way.
    assert!(verdicts.iter().all(ReconciliationVerdict::canonical_wins));
    assert!(link.radar().held_heights().is_empty());
    // The discarded frames are recorded as compared, not as silently ignored.
    assert_eq!(report.counters.late_frames_compared, 2);
}

#[tokio::test]
async fn a_digest_that_arrives_between_frames_closes_the_view_it_seals() {
    // The paced source is what makes this expressible at all: a canonical block has to
    // arrive *while* a height is being watched, not before the run or after it.
    let (source, steps) = PacedSource::channel();
    let mut link = PreconfLink::new(source, radar(), fast_config());
    let (tx, mut rx) = mpsc::channel(256);
    let (ctx, crx) = mpsc::channel(64);

    let handle = tokio::spawn(async move {
        let mut clock = StepClock::new(1_000, 5);
        let mut tick = move || clock.tick();
        link.run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
            .await
    });

    let _ = steps.send(frame(10, 1)).await;
    let _ = steps.send(frame(10, 2)).await;
    // Handshake on the event stream, not on a wall clock: the two views are held once their
    // acceptances have been delivered, and the link is then blocked waiting for a step.
    let mut accepted = 0;
    while accepted < 2 {
        let event = rx.recv().await.expect("the link is producing");
        if matches!(event, RadarEvent::FrameAccepted { number: 10, .. }) {
            accepted += 1;
        }
    }
    let _ = ctx
        .send(digest(10, b256(0x11), b256(0x90), payload_hashes(2), 2_000))
        .await;
    drop(steps);
    drop(ctx);

    let report = handle
        .await
        .expect("the task ran")
        .expect("a paced run completes");
    let mut rest = Vec::new();
    while let Ok(event) = rx.try_recv() {
        rest.push(event);
    }

    assert_eq!(report.ended_by, Some(LinkEndedBy::Exhausted));
    assert_eq!(report.canonical_digests_consumed, 1);
    assert_eq!(report.counters.canonical_closures, 1);
    assert_eq!(report.counters.reconciled_content_equal, 1);
    assert_eq!(report.counters.frames_accepted, 2);
    let reconciled = rest
        .iter()
        .find(|event| matches!(event, RadarEvent::Reconciled { .. }))
        .expect("the closing event was delivered");
    match reconciled {
        RadarEvent::Reconciled {
            number,
            verdict,
            hash_matched,
            latencies,
        } => {
            assert_eq!(*number, 10);
            assert_eq!(*verdict, ReconciliationVerdict::ContentEqual);
            // True here because this fixture deliberately spells the sealed hash the same as
            // the last view hash. On the measured endpoint that never happens (0/48): the
            // pending object's `hash` is the parent block's, so `hash_matched` is structurally
            // false for every real closure while content still matches 23/48 (§2.2).
            assert!(
                *hash_matched,
                "the fixture names the sealed hash as a view hash"
            );
            // §27's headline from the two stamps this run wrote: canonical detected at
            // 2000 ms, the first frame of this height at 1005 ms.
            assert_eq!(latencies.flashblocks_lead_ms, Some(2_000 - 1_005));
            assert_eq!(latencies.lead_is_positive(), Some(true));
        }
        other => panic!("expected a reconciliation, got {other:?}"),
    }
}

#[tokio::test]
async fn a_spent_read_budget_invalidates_every_open_view_before_it_reports() {
    // §11 + §25 + NC7: a link that gives up on the transport throws its incomplete views
    // away *first*, so no cross-session state can be resumed from them. The fifth payload is
    // deliberately never read: stopping is the finding, not finishing the batch.
    let script = vec![
        frame(10, 1),
        frame(11, 2),
        Step::failed("connection reset"),
        Step::failed("connection reset"),
        frame(12, 1),
    ];
    let mut config = fast_config();
    config.max_consecutive_read_failures = 2;
    let mut link = link_with(script, false, config);
    let (tx, rx) = mpsc::channel(256);
    let (_ctx, crx) = mpsc::channel(4);
    let mut clock = StepClock::new(1_000, 5);
    let mut tick = move || clock.tick();
    let report = link
        .run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
        .await
        .expect("a budget spent is a report, not an error");
    let events = drain(rx).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::ReadFailures));
    assert_eq!(report.link_resets, 1);
    assert_eq!(report.read_failures, 2);
    assert_eq!(report.counters.views_invalidated, 2);
    assert_eq!(count(&events, "view_invalidated"), 2);
    assert_eq!(
        link.source().reads,
        4,
        "the fifth payload was never asked for"
    );
    assert!(link.radar().held_heights().is_empty());
    assert!(stage_moves(&events).iter().any(|(_, to)| to == "reset"));
    // The local sequence keeps counting across the failure (§10: it is a session counter),
    // so the frames that *were* applied are still identifiable after the reset.
    assert_eq!(report.counters.frames_accepted, 2);
}

#[tokio::test]
async fn malformed_frames_fail_closed_and_the_held_view_survives_them() {
    // §24 + §25 + NC5: a payload that cannot be decoded is refused on its own. It does not
    // clear the view, does not become an empty view, and after enough of them the run ends
    // rather than reporting "nothing happened" forever.
    let script = vec![
        frame(10, 2),
        Step::malformed(),
        Step::malformed(),
        Step::malformed(),
    ];
    let mut config = fast_config();
    config.max_consecutive_decode_failures = 3;
    let mut link = link_with(script, false, config);
    let (report, events) = run(&mut link, Vec::new()).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::DecodeFailures));
    assert_eq!(report.decode_failures, 3);
    assert_eq!(report.counters.frames_accepted, 1);
    assert_eq!(
        report.counters.frames_offered, 1,
        "a frame that never decoded was never offered to the radar"
    );
    assert_eq!(accepted_heights(&events), vec![10]);
    // Fail-closed means the bad payloads touched nothing, including the good view.
    assert_eq!(link.radar().held_heights(), vec![BlockNumber(10)]);
    assert!(stage_moves(&events).iter().any(|(_, to)| to == "reset"));
    assert_eq!(report.frames_offered_to_radar, 1);
}

#[tokio::test]
async fn a_provider_with_nothing_to_show_is_idle_rather_than_dead() {
    // §49: a quiet chain and a stopped link must be distinguishable from the record alone.
    let script = vec![Step::null(), Step::null(), Step::null()];
    let mut config = fast_config();
    config.idle_after_reads = 2;
    let mut link = link_with(script, false, config);
    let (report, events) = run(&mut link, Vec::new()).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::Exhausted));
    assert_eq!(
        report.reads_empty, 3,
        "every null the provider answered is counted, including the one after which the \
         recording was found to be spent"
    );
    assert_eq!(report.reads_answered, 0);
    assert_eq!(report.read_failures, 0);
    let idle_lines: Vec<&RadarEvent> = events
        .iter()
        .filter(|event| match event {
            RadarEvent::Sequence { detail } => detail.contains("This is idle, not a dead link"),
            _ => false,
        })
        .collect();
    assert_eq!(idle_lines.len(), 1);
}

#[tokio::test]
async fn a_closed_event_sink_ends_the_run_as_an_error_and_leaves_the_report_behind() {
    // §49: the one failure reported as an error must still leave evidence. A run that
    // "completed" with zero events would otherwise read like a quiet chain.
    let script = vec![frame(10, 1), frame(10, 2)];
    let mut link = link_with(script, false, fast_config());
    let (tx, rx) = mpsc::channel(4);
    let (_ctx, crx) = mpsc::channel(4);
    drop(rx);
    let mut clock = StepClock::new(1_000, 5);
    let mut tick = move || clock.tick();
    let error = link
        .run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
        .await
        .expect_err("a sink nobody reads is an error, not a short run");
    assert!(matches!(error, PreconfError::EventQueueClosed { .. }));
    let report = link.report().expect("the failed run still has a report");
    assert_eq!(report.events_sent, 0);
    assert_eq!(report.ended_by, None, "the run did not end by choice");
    assert_eq!(
        report.reads_answered, 0,
        "the first delivery attempt came first"
    );
}

#[tokio::test]
async fn a_slow_consumer_stops_the_link_without_losing_an_event() {
    // §42's backpressure: a capacity-1 channel, with the collector only able to read while
    // the link is blocked. Nothing is dropped — the accepted count is the script's count.
    let script = vec![frame(10, 1), frame(11, 1), frame(12, 1), frame(13, 1)];
    let mut link = link_with(script, false, fast_config());
    let (tx, rx) = mpsc::channel(1);
    let (_ctx, crx) = mpsc::channel(4);
    let mut clock = StepClock::new(1_000, 5);
    let collecting = tokio::spawn(drain(rx));
    let report = {
        let mut tick = move || clock.tick();
        link.run(tx, crx, &mut tick, Arc::new(AtomicBool::new(false)))
            .await
            .expect("backpressure is not failure")
    };
    let events = collecting.await.expect("the collector ran");

    assert_eq!(report.frames_offered_to_radar, 4);
    assert_eq!(accepted_heights(&events), vec![10, 11, 12, 13]);
    assert_eq!(report.events_sent, events.len() as u64);
}

#[tokio::test]
async fn the_same_recording_through_the_same_clock_produces_the_same_evidence_twice() {
    // §41/§43: byte equality at the artifact layer, not "the numbers look the same". The
    // script deliberately mixes a growing height, a malformed payload and a null answer so a
    // difference in any of the three paths would show up.
    let script = vec![
        frame(10, 1),
        frame(10, 2),
        frame(11, 1),
        Step::malformed(),
        frame(11, 2),
        Step::null(),
    ];
    let first = replay_once(script.clone()).await;
    let second = replay_once(script).await;
    assert_eq!(
        serde_json::to_string(&first.0).expect("a report serializes"),
        serde_json::to_string(&second.0).expect("a report serializes"),
        "the run report must not depend on anything but the script"
    );
    assert_eq!(
        serde_json::to_string(&first.1).expect("events serialize"),
        serde_json::to_string(&second.1).expect("events serialize")
    );
    assert_eq!(
        first.2, second.2,
        "the radar's counters are the same table's source"
    );
    // A non-empty result, so equality above is not two runs that did nothing.
    assert!(first.0.frames_offered_to_radar >= 4);
    assert!(!first.1.is_empty());
}

/// One replay: fresh script, fresh radar, fresh clock starting at the same stamp. Returns
/// the report, the events, and the radar's counters as the run left them; the caller
/// compares the first two as serialized text, because that is the layer the evidence
/// files live at.
async fn replay_once(script: Vec<Step>) -> (LinkRunReport, Vec<RadarEvent>, String) {
    let mut link = link_with(script, false, fast_config());
    let (report, events) = run(&mut link, Vec::new()).await;
    (
        report,
        events,
        serde_json::to_string(link.radar().counters()).expect("counters serialize"),
    )
}

#[tokio::test]
async fn the_stop_flag_is_honoured_before_any_read_and_is_recorded_as_an_ending() {
    // §42's shutdown side: a requested stop is an `ended_by` value, not a silent break, so a
    // report can never be read as "the recording ran out" when it was asked to stop.
    let script = vec![frame(10, 1), frame(10, 2), frame(11, 1)];
    let mut link = link_with(script, false, fast_config());
    let (tx, rx) = mpsc::channel(256);
    let (_ctx, crx) = mpsc::channel(4);
    let stop = Arc::new(AtomicBool::new(true));
    let mut clock = StepClock::new(1_000, 5);
    let mut tick = move || clock.tick();
    let report = link
        .run(tx, crx, &mut tick, stop.clone())
        .await
        .expect("a requested stop is not an error");
    let events = drain(rx).await;

    assert_eq!(report.ended_by, Some(LinkEndedBy::StopRequested));
    assert_eq!(report.cycles, 0, "nothing was read after the stop was set");
    assert_eq!(link.source().reads, 0);
    assert_eq!(report.events_sent, 0);
    assert!(events.is_empty());
    assert_eq!(report.final_stage, LinkStage::Disconnected);
    assert!(stop.load(Ordering::Relaxed));
}

#[tokio::test]
async fn receipts_from_a_replay_source_reach_the_frame_that_was_read_with_them() {
    // §34's other half: a source that says it has pending receipts gets its list decoded and
    // attached to the frame just served; a source that says it has none is never asked, and
    // its frame still holds with only the target-derived finding.
    let receipt = json!([{
        "transactionHash": hex32_disp(b256(1)),
        "transactionIndex": "0x0",
        "blockNumber": "0xa",
        "blockHash": hex32_disp(b256(0x11)),
        "status": "0x1",
        "logs": [{
            "address": preconf_support::hex_addr(addr(0xa2)),
            "topics": [swap_topic()],
            "data": "0x",
            "transactionHash": hex32_disp(b256(1)),
            "logIndex": "0x0",
            "transactionIndex": "0x0",
            "removed": false,
        }],
    }]);
    let with_receipts = vec![Step::Answer(Some(frame_payload(10, 1)), Some(receipt))];
    let mut link = link_with(with_receipts, true, fast_config());
    let (report, events) = run(&mut link, Vec::new()).await;

    assert_eq!(report.receipt_lists_decoded, 1);
    assert_eq!(report.counters.affected_pools_by_log, 1);
    assert_eq!(
        report.counters.affected_pools_emitted, 2,
        "a call target and a log emitter are two findings"
    );
    assert_eq!(link.source().reads, 2);
    let pools: Vec<(alloy_primitives::Address, &'static str)> = events
        .iter()
        .filter_map(|event| match event {
            RadarEvent::PoolAffected(pool) => Some((pool.pool, pool.reason.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        pools,
        vec![
            (addr(0xa1), "transaction_target"),
            (addr(0xa2), "successful_log_emitter"),
        ]
    );

    let without = vec![Step::Answer(Some(frame_payload(10, 1)), None)];
    let mut link = link_with(without, false, fast_config());
    let (report, _) = run(&mut link, Vec::new()).await;
    assert_eq!(report.receipt_lists_decoded, 0);
    assert_eq!(report.counters.affected_pools_by_log, 0);
    assert_eq!(report.counters.affected_pools_emitted, 1);
    assert_eq!(
        report.counters.frames_accepted, 1,
        "no receipts is not a rejected frame"
    );
}
