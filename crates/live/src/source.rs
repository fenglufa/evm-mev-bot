//! One ingestion loop, parameterized by how a provider is asked about blocks.
//!
//! The HTTP poller, the WebSocket source and the recorded directory all run
//! this code. They differ only in the [`evm_chain::HeadReader`] underneath them
//! and in the [`SourceKind`] they stamp on an announcement, which is what makes
//! §32's Replay==Live claim structural instead of aspirational: there is no
//! live-specific ordering path to diverge.
//!
//! Backpressure is §50's, and it is a single mechanism: `sink.send().await` on a
//! bounded channel. A slow pipeline stops the source rather than losing events,
//! so "market events silently dropped" has no code path here — the only way an
//! event disappears is a channel the pipeline closed, and that ends the run with
//! [`LiveError::QueueClosed`] instead of a shorter session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use evm_chain::{ChainBlock, HeadReader};
use evm_core::{BlockNumber, ChainId};

use crate::error::{LiveError, LiveResult};
use crate::event::{now_unix_ms, BlockAnnouncement, MarketEvent, SourceKind, SourceStatus};
use crate::tracker::{BlockTracker, TrackerPolicy, TrackerStats};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceConfig {
    /// How often the head is asked for. 900 ms against this chain's measured
    /// 1 s canonical cadence: slower than the chain and the run falls behind by
    /// construction, faster and it costs a round trip that answers nothing.
    pub poll_interval_ms: u64,
    /// Bounded work per cycle, so a long hole cannot monopolize the loop.
    pub max_blocks_per_cycle: usize,
    /// §50. Small on purpose: the queue is a shock absorber, not a backlog store.
    pub event_queue_capacity: usize,
    /// Cycles with nothing new before an `Idle` status is emitted, so a quiet
    /// chain is distinguishable from a dead source.
    pub idle_after_cycles: u32,
    /// Consecutive read failures before the source gives up on the transport.
    pub max_consecutive_read_failures: u32,
    pub tracker: TrackerPolicy,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 900,
            max_blocks_per_cycle: 8,
            event_queue_capacity: 32,
            idle_after_cycles: 5,
            max_consecutive_read_failures: 10,
            tracker: TrackerPolicy::default(),
        }
    }
}

/// How a source's run ended. A run that "completed" is not a success claim
/// unless the reason says it was asked to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum EndedBy {
    StopRequested,
    QueueClosed,
    ReadFailures,
    GapUnrecovered,
    /// A finite transport — a recording — ran out. Everything it holds was
    /// delivered, so there is no next block to wait for.
    Exhausted,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    pub source: SourceKind,
    pub cycles: u64,
    pub read_failures: u64,
    pub events_sent: u64,
    pub ended_by: Option<EndedBy>,
    /// The canonical sources' ordering counts. `None` for a source that never
    /// orders blocks — a candidate source has no tracker, and reporting a table
    /// of zeros for it would read like "no gaps observed".
    pub tracker: Option<TrackerStats>,
    /// What this kind of source actually counted, for the session record (§48).
    pub detail: serde_json::Value,
}

#[async_trait]
pub trait MarketDataSource: Send {
    fn kind(&self) -> SourceKind;

    /// The head, read *before* any state work begins (§7: snapshot the head
    /// first, then subscribe, then replay forward).
    async fn head(&mut self) -> LiveResult<BlockNumber>;

    /// Emit ordered events until `stop` is set or the run ends.
    async fn run(
        &mut self,
        start_after: BlockNumber,
        sink: mpsc::Sender<MarketEvent>,
        stop: Arc<AtomicBool>,
    ) -> LiveResult<RunReport>;

    /// What the endpoint could actually do, as measured rather than assumed
    /// (§41, §73 and the supplement's "do not guess the protocol"). The default
    /// says nothing was probed, which is itself a fact worth a report.
    fn capability(&self) -> serde_json::Value {
        serde_json::json!({"probed": false})
    }

    /// The counters from the most recent `run`, available even when that run
    /// ended in an error. §49 asks a failed session to flush evidence too, and
    /// a report that only survives success is evidence about successes only.
    fn report(&self) -> Option<RunReport> {
        None
    }
}

fn announcement(block: &ChainBlock, source: SourceKind) -> BlockAnnouncement {
    BlockAnnouncement {
        chain_id: block.chain_id,
        number: block.number,
        hash: block.hash,
        parent_hash: block.parent_hash,
        chain_timestamp_secs: block.timestamp,
        transaction_count: block.transaction_count as u64,
        observed_at_unix_ms: now_unix_ms(),
        source,
    }
}

/// The loop. See the module comment for why one loop serves three providers.
pub struct PollingSource<R> {
    reader: R,
    chain_id: ChainId,
    kind: SourceKind,
    config: SourceConfig,
    capability: serde_json::Value,
    last_report: Option<RunReport>,
}

impl<R: HeadReader> PollingSource<R> {
    pub fn new(reader: R, chain_id: ChainId, kind: SourceKind, config: SourceConfig) -> Self {
        let transport = reader.transport();
        Self {
            reader,
            chain_id,
            kind,
            config,
            capability: serde_json::json!({"transport": transport, "probed": true}),
            last_report: None,
        }
    }

    pub fn reader(&mut self) -> &mut R {
        &mut self.reader
    }

    pub fn set_capability(&mut self, capability: serde_json::Value) {
        self.capability = capability;
    }

    pub const fn config(&self) -> SourceConfig {
        self.config
    }

    async fn forward(
        sink: &mpsc::Sender<MarketEvent>,
        events: Vec<MarketEvent>,
        sent: &mut u64,
    ) -> LiveResult<()> {
        for event in events {
            sink.send(event).await.map_err(|_| LiveError::QueueClosed)?;
            *sent += 1;
        }
        Ok(())
    }

    /// One cycle: ask the head, register what is owed, read it in order.
    ///
    /// `failures` is the streak the give-up budget is measured against and
    /// `failed_reads` the lifetime count the report carries; a cycle that reads
    /// without error breaks the streak, which is why the reset lives in the
    /// caller and not here. A provider whose head answers and whose *bodies* never
    /// do would otherwise retry one failing read forever without classifying it.
    async fn cycle(
        &mut self,
        tracker: &mut BlockTracker,
        sink: &mpsc::Sender<MarketEvent>,
        sent: &mut u64,
        failures: &mut u32,
        failed_reads: &mut u64,
        stop: &AtomicBool,
    ) -> LiveResult<CycleOutcome> {
        let mut produced = 0u64;
        // Subscription notifications, if this transport has any. They only
        // ever say "this head exists"; the block itself is still read through
        // the reader, in order, so §4's rule holds even for a pushing provider.
        let hints = self.reader.take_hints();
        for hint in hints {
            Self::forward(sink, tracker.note_head(hint), sent).await?;
        }
        let head = match self.reader.head().await {
            Ok(head) => head,
            Err(error) => {
                *failures += 1;
                *failed_reads += 1;
                Self::forward(
                    sink,
                    vec![MarketEvent::Status(SourceStatus::Disconnected {
                        source: self.kind,
                        reason: format!("{error} (consecutive failure {})", *failures),
                    })],
                    sent,
                )
                .await?;
                return Ok(CycleOutcome::ReadFailed);
            }
        };
        Self::forward(sink, tracker.note_head(head), sent).await?;
        let owed = tracker.owed(head, self.config.max_blocks_per_cycle);
        for number in owed {
            if stop.load(Ordering::Relaxed) {
                // The unread part of the batch stays owed in the tracker: a stop
                // request abandons work that was never started, never a block that
                // was already delivered (§51). Checking only between cycles let a
                // catch-up cycle read its whole batch of historical blocks after the
                // pipeline had asked it to stop, past §49's shutdown budget.
                break;
            }
            match self.reader.block_at(number).await {
                Ok(Some(block)) => {
                    produced += 1;
                    Self::forward(sink, tracker.observe(announcement(&block, self.kind)), sent)
                        .await?;
                }
                // Not a failure: this provider does not have that number yet.
                // This is the gap-recovery probe of §5, retried by the tracker.
                Ok(None) => {
                    Self::forward(sink, tracker.observe_absent(number), sent).await?;
                }
                Err(error) => {
                    *failures += 1;
                    *failed_reads += 1;
                    Self::forward(
                        sink,
                        vec![MarketEvent::Status(SourceStatus::Disconnected {
                            source: self.kind,
                            reason: format!(
                                "block {}: {error} (consecutive failure {})",
                                number.0, *failures
                            ),
                        })],
                        sent,
                    )
                    .await?;
                    return Ok(CycleOutcome::ReadFailed);
                }
            }
        }
        // A recording with nothing delivered this cycle and nothing owed has no
        // next block to wait for. A hole does not qualify: the block still sits in
        // the tracker while the gap retries run (§5).
        if produced == 0 && self.reader.is_finite() && tracker.outstanding().is_empty() {
            return Ok(CycleOutcome::Exhausted);
        }
        Ok(CycleOutcome::Produced(produced))
    }
}

enum CycleOutcome {
    Produced(u64),
    ReadFailed,
    /// A finite provider that has nothing left to give: every block it holds has
    /// been delivered, nothing is owed, and no further head can appear. Only a
    /// recording can answer this — a node's next block simply has not happened
    /// yet, which is the `Idle` case instead.
    Exhausted,
}

#[async_trait]
impl<R: HeadReader + Send> MarketDataSource for PollingSource<R> {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    async fn head(&mut self) -> LiveResult<BlockNumber> {
        self.reader
            .head()
            .await
            .map_err(|e| LiveError::Head(e.to_string()))
    }

    async fn run(
        &mut self,
        start_after: BlockNumber,
        sink: mpsc::Sender<MarketEvent>,
        stop: Arc<AtomicBool>,
    ) -> LiveResult<RunReport> {
        let mut tracker = BlockTracker::new(self.chain_id, start_after, self.config.tracker);
        let mut report = RunReport {
            source: self.kind,
            cycles: 0,
            read_failures: 0,
            events_sent: 0,
            ended_by: None,
            tracker: None,
            detail: serde_json::Value::Null,
        };
        let mut quiet_cycles = 0u32;
        let mut failures = 0u32;
        loop {
            if stop.load(Ordering::Relaxed) {
                report.ended_by = Some(EndedBy::StopRequested);
                break;
            }
            report.cycles += 1;
            match self
                .cycle(
                    &mut tracker,
                    &sink,
                    &mut report.events_sent,
                    &mut failures,
                    &mut report.read_failures,
                    stop.as_ref(),
                )
                .await
            {
                Ok(CycleOutcome::Produced(produced)) if produced > 0 => {
                    quiet_cycles = 0;
                    failures = 0;
                }
                Ok(CycleOutcome::Produced(_)) => {
                    // A cycle that read everything it asked for breaks the
                    // failure streak, even when the chain offered nothing new.
                    failures = 0;
                    quiet_cycles += 1;
                    if quiet_cycles >= self.config.idle_after_cycles {
                        quiet_cycles = 0;
                        let head = tracker.next_expected();
                        Self::forward(
                            &sink,
                            vec![MarketEvent::Status(SourceStatus::Idle {
                                source: self.kind,
                                head: Some(head),
                            })],
                            &mut report.events_sent,
                        )
                        .await?;
                    }
                }
                Ok(CycleOutcome::Exhausted) => {
                    report.ended_by = Some(EndedBy::Exhausted);
                    break;
                }
                Ok(CycleOutcome::ReadFailed) => {
                    if failures >= self.config.max_consecutive_read_failures {
                        Self::forward(
                            &sink,
                            vec![MarketEvent::Status(SourceStatus::Failed {
                                source: self.kind,
                                reason: format!(
                                    "{failures} consecutive reads failed; the source stopped rather than spinning"
                                ),
                            })],
                            &mut report.events_sent,
                        )
                        .await?;
                        report.ended_by = Some(EndedBy::ReadFailures);
                        break;
                    }
                }
                Err(error) => {
                    report.tracker = Some(tracker.stats());
                    self.last_report = Some(report.clone());
                    return Err(error);
                }
            }
            if let Some((from, to)) = tracker.unrecovered_gap() {
                let detail = format!(
                    "block {from} was never produced by {} (transport {})",
                    self.chain_id.0,
                    self.reader.transport()
                );
                report.tracker = Some(tracker.stats());
                report.ended_by = Some(EndedBy::GapUnrecovered);
                self.last_report = Some(report.clone());
                return Err(LiveError::GapUnrecovered {
                    from,
                    to,
                    attempts: self.config.tracker.max_gap_attempts,
                    detail,
                });
            }
            tokio::time::sleep(Duration::from_millis(self.config.poll_interval_ms)).await;
        }
        report.tracker = Some(tracker.stats());
        self.last_report = Some(report.clone());
        Ok(report)
    }

    fn capability(&self) -> serde_json::Value {
        self.capability.clone()
    }

    fn report(&self) -> Option<RunReport> {
        self.last_report.clone()
    }
}
