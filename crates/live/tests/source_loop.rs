//! §3.1, §4, §5 and §50 tested at the layer that owns them: one ingestion loop,
//! driven against a scripted [`HeadReader`].
//!
//! These are the source-layer tests of §54's four layers. What they check is the
//! loop's contract, not the chain's: the arrival order of blocks is never taken
//! as their order (§4), a number the provider does not have is retried and then
//! named rather than skipped (§5, §8), a transport that stops answering is
//! reported and given up on within a budget (§53), and a queue that is full stops
//! the source instead of losing a block (§50).
//!
//! The reader here is a script, not a simulated node. Nothing in these tests
//! invents market data for an acceptance claim (§43, §63): no pool, no reserve,
//! no opportunity appears in this file. Block headers with a number and a hash
//! are the only facts, because the ordering rules this layer enforces are entirely
//! about numbers — and the real-data half of the same code path is
//! `crates/pipeline/tests/recorded_loop.rs`, which runs recorded GIWA blocks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;

use evm_chain::{ChainBlock, ChainError, HeadReader};
use evm_core::{BlockNumber, ChainId};
use evm_live::{
    EndedBy, LiveError, LiveResult, MarketDataSource, MarketEvent, PollingSource, RunReport,
    SourceConfig, SourceKind, SourceStatus, TrackerPolicy,
};

const CHAIN: ChainId = ChainId(91342);

/// A block whose identity is a function of its number, so two blocks with the
/// same number can only disagree by being deliberately given different hashes.
fn block(number: u64) -> ChainBlock {
    ChainBlock {
        chain_id: CHAIN,
        number: BlockNumber(number),
        hash: hash_of(number),
        parent_hash: hash_of(number.saturating_sub(1)),
        timestamp: 1_700_000_000 + number,
        transaction_count: 3,
    }
}

fn hash_of(number: u64) -> B256 {
    B256::left_padding_from(&number.to_be_bytes())
}

/// The scripted provider. Every field is a knob one test turns; the defaults
/// describe an ordinary node with a contiguous supply of blocks.
#[derive(Debug, Default)]
struct Script {
    /// Answer to `head()` once the scripted sequence runs out.
    head: u64,
    /// Head answers to give in order, one per read. Empty means `head` every time.
    heads: VecDeque<u64>,
    blocks: BTreeMap<u64, ChainBlock>,
    /// Numbers answered as "not here yet" (`Ok(None)`) forever.
    absent: BTreeSet<u64>,
    /// Numbers whose read returns an error.
    block_errors: BTreeSet<u64>,
    /// How many `head()` reads fail before the provider starts answering. A very
    /// large number means "always fails".
    fail_head_reads: usize,
    /// Notification payloads handed over per cycle, in the order drained.
    hints: VecDeque<Vec<u64>>,
    /// A recording has a last block; a node does not (§41's `Exhausted`).
    finite: bool,
    head_reads: usize,
    block_reads: Vec<u64>,
}

impl Script {
    fn with(head: u64, numbers: &[u64]) -> Self {
        let mut script = Self {
            head,
            ..Self::default()
        };
        for number in numbers {
            script.blocks.insert(*number, block(*number));
        }
        script
    }
}

#[async_trait]
impl HeadReader for Script {
    fn transport(&self) -> &'static str {
        "scripted"
    }

    async fn head(&mut self) -> Result<BlockNumber, ChainError> {
        self.head_reads += 1;
        if self.head_reads <= self.fail_head_reads {
            return Err(ChainError::Rpc("provider did not answer".to_string()));
        }
        Ok(BlockNumber(self.heads.pop_front().unwrap_or(self.head)))
    }

    async fn block_at(&mut self, number: BlockNumber) -> Result<Option<ChainBlock>, ChainError> {
        self.block_reads.push(number.0);
        if self.block_errors.contains(&number.0) {
            return Err(ChainError::Rpc(format!(
                "block {}: transport error",
                number.0
            )));
        }
        if self.absent.contains(&number.0) {
            return Ok(None);
        }
        Ok(self.blocks.get(&number.0).cloned())
    }

    fn take_hints(&mut self) -> Vec<BlockNumber> {
        self.hints
            .pop_front()
            .map(|numbers| numbers.into_iter().map(BlockNumber).collect())
            .unwrap_or_default()
    }

    fn is_finite(&self) -> bool {
        self.finite
    }
}

/// How the tests talk about events: by the labels the pipeline itself stamps on
/// them, so an assertion reads like the evidence line a reader would check.
fn canonical_numbers(events: &[MarketEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(MarketEvent::canonical_block)
        .map(|number| number.0)
        .collect()
}

fn statuses(events: &[MarketEvent]) -> Vec<&SourceStatus> {
    events
        .iter()
        .filter_map(|event| match event {
            MarketEvent::Status(status) => Some(status),
            _ => None,
        })
        .collect()
}

fn labels(events: &[MarketEvent]) -> Vec<&'static str> {
    events.iter().map(MarketEvent::kind_label).collect()
}

/// Run the loop to the end of one test's scenario: `until` is checked after every
/// event, and once it matches the stop flag is set, which is §49's own shutdown
/// path rather than a cancellation the test invents.
async fn drive(
    mut source: PollingSource<Script>,
    start_after: u64,
    until: impl Fn(&[MarketEvent]) -> bool,
) -> (
    Vec<MarketEvent>,
    LiveResult<RunReport>,
    PollingSource<Script>,
) {
    let capacity = source.config().event_queue_capacity.max(1);
    let (sink, mut receiver) = mpsc::channel::<MarketEvent>(capacity);
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = Arc::clone(&stop);
    let handle = tokio::spawn(async move {
        let report = source.run(BlockNumber(start_after), sink, task_stop).await;
        // `run` borrows the source, so handing it back is how a test reads the
        // counters and the capability table after the loop is over.
        (report, source)
    });

    let collecting = async {
        let mut events: Vec<MarketEvent> = Vec::new();
        loop {
            match receiver.recv().await {
                Some(event) => {
                    events.push(event);
                    if stop.load(Ordering::Relaxed) || until(&events) {
                        stop.store(true, Ordering::Relaxed);
                        // Drain what is already queued so `events_sent` and the
                        // received count are comparable at the end.
                        while let Ok(event) = receiver.try_recv() {
                            events.push(event);
                        }
                    }
                }
                None => return events,
            }
        }
    };
    let events = tokio::time::timeout(Duration::from_secs(10), collecting)
        .await
        .unwrap_or_else(|_| panic!("the source ran for 10 s without reaching its scenario"));
    let (report, source) = handle
        .await
        .unwrap_or_else(|_| panic!("the ingestion task panicked"));
    (events, report, source)
}

fn config(tweak: impl FnOnce(&mut SourceConfig)) -> SourceConfig {
    let mut config = SourceConfig {
        poll_interval_ms: 1,
        ..SourceConfig::default()
    };
    tweak(&mut config);
    config
}

fn polling(reader: Script, config: SourceConfig) -> PollingSource<Script> {
    PollingSource::new(reader, CHAIN, SourceKind::HttpPoll, config)
}

#[tokio::test]
async fn a_provider_that_volunteers_blocks_out_of_order_is_still_read_in_order() {
    // §4: the order a source's frames arrive in is not the chain's order. The
    // hints here come backwards on purpose, and the blocks themselves are only
    // ever read by number.
    let mut reader = Script::with(12, &[10, 11, 12]);
    reader.hints.push_back(vec![12, 10, 11]);
    let (events, report, _source) = drive(polling(reader, config(|_| {})), 9, |events| {
        canonical_numbers(events).len() == 3
    })
    .await;

    assert_eq!(
        canonical_numbers(&events),
        vec![10, 11, 12],
        "the pipeline is handed chain order, whatever order the notifications came in"
    );
    let run = report.expect("the run ends on the stop flag, not in an error");
    assert_eq!(run.ended_by, Some(EndedBy::StopRequested));
    assert_eq!(run.source, SourceKind::HttpPoll);
    let tracker = run
        .tracker
        .expect("a canonical source keeps an ordering table");
    assert_eq!(tracker.emitted, 3);
    assert_eq!(tracker.gaps_unrecovered, 0);
    // The announcement names the source that delivered it, so a session record can
    // say which transport a block came from (§41).
    for event in &events {
        if let MarketEvent::Canonical(announcement) = event {
            assert_eq!(announcement.source, SourceKind::HttpPoll);
            assert_eq!(announcement.chain_id, CHAIN);
            assert_eq!(announcement.hash, hash_of(announcement.number.0));
        }
    }
}

#[tokio::test]
async fn the_same_head_read_twice_does_not_deliver_a_block_twice() {
    // §4's dedup half: the head is re-read every cycle, and a block already
    // delivered must not become a second state update downstream.
    let (events, report, _source) = drive(
        polling(Script::with(11, &[10, 11]), config(|_| {})),
        9,
        |events| events.len() >= 6,
    )
    .await;

    let numbers = canonical_numbers(&events);
    assert_eq!(
        numbers.iter().filter(|n| **n == 10).count(),
        1,
        "block 10 delivered {numbers:?}"
    );
    assert_eq!(numbers, vec![10, 11], "one delivery per number, in order");
    let tracker = report
        .expect("a quiet head is not an error")
        .tracker
        .expect("a canonical source keeps an ordering table");
    assert_eq!(tracker.emitted, 2);
    assert!(
        tracker.announced >= 2,
        "the head was volunteered more often than the blocks were emitted: {:?}",
        tracker.announced
    );
}

#[tokio::test]
async fn a_hole_the_provider_never_fills_ends_the_run_and_names_the_block() {
    // §5 and §8: a missing number is re-probed within a budget, and when the
    // budget runs out the run says which block is missing instead of continuing
    // past it. Block 13 exists and is deliberately not delivered.
    let mut reader = Script::with(13, &[10, 11, 13]);
    reader.absent.insert(12);
    let (events, report, _source) = drive(
        polling(
            reader,
            config(|config| {
                config.tracker = TrackerPolicy {
                    max_pending_ahead: 8,
                    max_gap_attempts: 3,
                };
            }),
        ),
        9,
        |_events| false,
    )
    .await;

    let error = match report {
        Err(error) => error,
        Ok(run) => panic!("a run that walked past a hole reported success: {run:?}"),
    };
    match &error {
        LiveError::GapUnrecovered { from, attempts, .. } => {
            assert_eq!(*from, 12, "{error}");
            assert_eq!(*attempts, 3, "the probe budget is the one configured");
        }
        other => panic!("expected a named gap, got {other}"),
    }
    assert_eq!(
        canonical_numbers(&events),
        vec![10, 11],
        "the block after the hole is not handed downstream as if nothing were missing"
    );
    assert!(
        labels(&events).contains(&"gap_detected"),
        "the hole is stated as an event before it ends the run: {events:?}"
    );
}

#[tokio::test]
async fn a_provider_that_does_not_answer_is_said_and_retried() {
    // §53's transport class, seen from the loop: two failed head reads, then the
    // provider recovers and the blocks still arrive. The failures are events, not
    // silence, and a success resets the count.
    let mut reader = Script::with(11, &[10, 11]);
    reader.fail_head_reads = 2;
    let (events, report, _source) = drive(polling(reader, config(|_| {})), 9, |events| {
        canonical_numbers(events).len() == 2
    })
    .await;

    assert_eq!(canonical_numbers(&events), vec![10, 11]);
    let disconnections = statuses(&events)
        .iter()
        .filter(|status| matches!(status, SourceStatus::Disconnected { .. }))
        .count();
    assert_eq!(
        disconnections, 2,
        "one status line per failed read: {events:?}"
    );
    for status in statuses(&events) {
        if let SourceStatus::Disconnected { reason, .. } = status {
            assert!(reason.contains("consecutive failure"), "{reason}");
        }
    }
    let run = report.expect("a recovered provider is not a failed run");
    assert_eq!(run.read_failures, 2);
    assert_eq!(run.ended_by, Some(EndedBy::StopRequested));
}

#[tokio::test]
async fn the_source_gives_up_after_its_failure_budget_instead_of_spinning() {
    let mut reader = Script::with(11, &[10, 11]);
    reader.fail_head_reads = usize::MAX;
    let (events, report, _source) = drive(
        polling(
            reader,
            config(|config| config.max_consecutive_read_failures = 3),
        ),
        9,
        |events| labels(events).contains(&"source_failed"),
    )
    .await;

    let run = report.expect("giving up is a reportable end, not an error of the run");
    assert_eq!(run.ended_by, Some(EndedBy::ReadFailures));
    assert_eq!(run.cycles, 3, "one cycle per failure the budget allowed");
    assert_eq!(run.read_failures, 3);
    assert_eq!(
        canonical_numbers(&events),
        Vec::<u64>::new(),
        "a provider that never answered produced no blocks"
    );
    assert!(
        matches!(
            statuses(&events).last(),
            Some(SourceStatus::Failed { reason, .. }) if reason.contains("3 consecutive reads")
        ),
        "the last word is the reason the source stopped: {events:?}"
    );
}

#[tokio::test]
async fn a_full_queue_stops_the_source_rather_than_losing_a_block() {
    // §50's promise, measured: with a queue of one and a consumer that empties it
    // as it can, every block the provider holds still reaches the pipeline, in
    // order, and the source's own count of what it sent equals what was received.
    let reader = Script::with(15, &(10..=15).collect::<Vec<_>>());
    let (events, report, _source) = drive(
        polling(
            reader,
            config(|config| {
                config.event_queue_capacity = 1;
                config.max_blocks_per_cycle = 8;
            }),
        ),
        9,
        |events| canonical_numbers(events).len() == 6,
    )
    .await;

    assert_eq!(canonical_numbers(&events), vec![10, 11, 12, 13, 14, 15]);
    let run = report.expect("backpressure is not an error");
    assert_eq!(
        run.events_sent as usize,
        events.len(),
        "the source counted {} sends and the pipeline received {}",
        run.events_sent,
        events.len()
    );
    assert_eq!(
        run.tracker
            .expect("a canonical source keeps a table")
            .emitted,
        6
    );
}

#[tokio::test]
async fn a_sink_that_was_closed_ends_the_run_loudly() {
    // §51: an event cannot vanish. The only way a delivery fails is a pipeline
    // that has gone away, and that is an error the run returns.
    let (sink, receiver) = mpsc::channel::<MarketEvent>(2);
    drop(receiver);
    let mut source = PollingSource::new(
        Script::with(12, &[10, 11, 12]),
        CHAIN,
        SourceKind::HttpPoll,
        config(|_| {}),
    );
    let report = source
        .run(BlockNumber(9), sink, Arc::new(AtomicBool::new(false)))
        .await;
    assert!(
        matches!(report, Err(LiveError::QueueClosed)),
        "{:?}",
        report.err()
    );
}

#[tokio::test]
async fn a_quiet_chain_is_reported_as_idle_and_not_as_a_finished_run() {
    // A source that is alive and has nothing to say must be tellable apart from one
    // that has stopped: §3.1C's heartbeat question, answered with an event.
    let reader = Script::with(9, &[8]);
    let (events, report, _source) = drive(
        polling(reader, config(|config| config.idle_after_cycles = 2)),
        9,
        |events| labels(events).contains(&"idle"),
    )
    .await;

    assert_eq!(canonical_numbers(&events), Vec::<u64>::new());
    let idle = statuses(&events)
        .iter()
        .find_map(|status| match status {
            SourceStatus::Idle { head, .. } => Some(*head),
            _ => None,
        })
        .expect("an idle status line");
    assert_eq!(idle, Some(BlockNumber(10)), "the head it is waiting past");
    let run = report.expect("idle is not a failure");
    assert_eq!(run.ended_by, Some(EndedBy::StopRequested));
    assert!(run.cycles >= 2);
}

#[tokio::test]
async fn a_recording_that_runs_out_ends_the_run_by_itself() {
    // §41's `Exhausted`: a finite transport has no next block, so waiting for one
    // would be a timeout pretending to be a finished session.
    let mut reader = Script::with(11, &[10, 11]);
    reader.finite = true;
    // Nothing stops this run from outside: the scenario is that the recording
    // runs out and the loop notices, so the driver waits for the channel to close.
    let (events, report, _source) = drive(polling(reader, config(|_| {})), 9, |_| false).await;

    assert_eq!(canonical_numbers(&events), vec![10, 11]);
    let run = report.expect("a recording that ended is not an error");
    assert_eq!(run.ended_by, Some(EndedBy::Exhausted));
    assert_eq!(run.source, SourceKind::HttpPoll);
}

#[tokio::test]
async fn the_report_survives_the_run_and_the_capability_table_is_the_probed_one() {
    // §49's flush needs the source's counters after the loop, and §73 needs the
    // capability table an endpoint was measured against — including a probe that
    // came back negative, which is what the WebSocket source records for
    // `eth_subscribe` on this provider.
    let mut reader = Script::with(10, &[10]);
    reader.heads.push_back(10);
    let mut source = polling(reader, config(|_| {}));
    source.set_capability(
        json!({"transport": "websocket", "subscription": {"eth_subscribe": "BLOCKED"}}),
    );
    let (sink, mut receiver) = mpsc::channel::<MarketEvent>(8);
    let stop = Arc::new(AtomicBool::new(true));
    let drop_receiver = async move { while receiver.recv().await.is_some() {} };
    let collecting = tokio::spawn(drop_receiver);
    let run = source
        .run(BlockNumber(9), sink, Arc::clone(&stop))
        .await
        .expect("a source asked to stop before it began still reports");
    collecting.await.expect("the consumer task");

    assert_eq!(run.ended_by, Some(EndedBy::StopRequested));
    assert_eq!(
        run.cycles, 0,
        "the stop flag is checked before the first read"
    );
    assert_eq!(
        source.report().expect("the last run is kept").cycles,
        0,
        "a report is readable after the run, for the flush §49 asks for"
    );
    assert_eq!(
        source.capability()["subscription"]["eth_subscribe"],
        "BLOCKED"
    );
    assert_eq!(source.capability()["transport"], "websocket");
    assert_eq!(source.kind(), SourceKind::HttpPoll);
}

#[tokio::test]
async fn a_read_that_errors_names_the_block_it_was_asked_for() {
    // §53 keeps "this number is not here" and "the transport broke" apart. The
    // second one is a counted read failure with the block number in its reason.
    let mut reader = Script::with(11, &[10, 11]);
    reader.block_errors.insert(11);
    let (events, report, mut source) = drive(
        polling(
            reader,
            config(|config| config.max_consecutive_read_failures = 2),
        ),
        9,
        |events| labels(events).contains(&"source_failed"),
    )
    .await;

    assert_eq!(canonical_numbers(&events), vec![10]);
    let run = report.expect("the failure budget is a reportable end");
    assert_eq!(run.ended_by, Some(EndedBy::ReadFailures));
    assert!(
        source.reader().block_reads.contains(&11),
        "block 11 was asked for and its transport error is what the run reported: {run:?}"
    );
}

#[tokio::test]
async fn a_stop_request_in_the_middle_of_a_batch_stops_the_batch() {
    // §49's shutdown budget is spent in reads, not in the flag. A source catching up
    // to a distant head is owed a whole batch at once; the flag used to be looked at
    // only between cycles, so a live run over an archive window kept reading block
    // after block after the pipeline had asked it to stop, and its source report —
    // the gap table §5 and §6 ask for — was never collected in time.
    let numbers: Vec<u64> = (10..=200).collect();
    let reader = Script::with(200, &numbers);
    let (events, report, mut source) = drive(
        polling(
            reader,
            config(|config| {
                config.max_blocks_per_cycle = 64;
                // A queue with room in it lets the loop run ahead of the consumer
                // without ever waiting, which is exactly the case that hid the bug.
                config.event_queue_capacity = 2;
            }),
        ),
        9,
        |events| !canonical_numbers(events).is_empty(),
    )
    .await;

    let run = report.expect("the run ends on the stop flag, not in a read");
    assert_eq!(run.ended_by, Some(EndedBy::StopRequested));
    let reads = source.reader().block_reads.len();
    assert!(
        reads < 8,
        "the batch was 64 blocks long and the stop arrived after the first one; reads = {reads} ({:?})",
        source.reader().block_reads
    );
    let tracker = run
        .tracker
        .expect("a canonical source keeps an ordering table");
    assert_eq!(
        tracker.emitted as usize,
        canonical_numbers(&events).len(),
        "what the run says it emitted is what the pipeline was handed — an abandoned batch is unread, not dropped"
    );
}
