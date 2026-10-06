//! M9.4 §10/§11/§25/§42/§44: the link that turns provider answers into radar input.
//!
//! The radar (`crate::preconf_radar`) is a pure state machine: it holds views, names
//! affected pools, and closes them against a canonical digest; it never touches a clock
//! or a socket. This module is the other half — the one place in M9.4 that awaits,
//! counts failures, and decides what happens when a provider stops answering.
//!
//! Four properties the task book asks for by name, and how each is structural here:
//!
//! * **§42 — ordered processing, one consumer.** There is exactly one loop and one
//!   owner of the radar. A payload is read, decoded and ingested in read order, and the
//!   events a call produced are forwarded in the order the radar returned them. No
//!   second task can touch the same `PreconfirmationState`, so the failure the task book
//!   names ("Flashblock 2 / Flashblock 1 applied in parallel to one preconfirmation
//!   state") has no code path. Parallelising would require giving the radar a second
//!   owner, which is the point: concurrency stays a decision, not an accident.
//! * **§10 — an explicit machine, not bool soup.** [`LinkStage`] and its edge table
//!   carry the transport's state; every move is emitted as
//!   [`RadarEvent::LinkStageChanged`], and a move the table forbids leaves the stage
//!   alone and is recorded instead of panicking (§40). The per-height machine is a
//!   different machine (`PreconfStage`, in the radar) and is deliberately not merged in:
//!   a socket has no block number.
//! * **§34 + §44 — no new production RPC, no second canonical pipeline.** The link reads
//!   exactly one thing, [`FrameSource::next_view`], which is the `pending` read the
//!   canonical path already performs. Canonical blocks *arrive* as [`CanonicalDigest`]s
//!   on a channel fed by that path; the link never calls a method to close a view, and
//!   emits nothing but radar events. §58's isolation rests on that type face rather than
//!   on review.
//! * **§11/§25 — one failure policy.** A decode failure fails closed for that frame and
//!   touches nothing else; a transport failure retries against a budget, and spending it
//!   runs [`EarlyRadar::on_disconnect`] *before* the run reports, so an incomplete view
//!   cannot outlive the link that was filling it. Every terminal reason lands in
//!   `ended_by` plus counters, so a failed session still flushes evidence (§49).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::preconf::{PreconfError, PreconfReceipt, RadarCounters, RadarEvent};
use crate::preconf_decode::{frame_from_pending_value, receipts_from_value};
use crate::preconf_provider::FrameSource;
use crate::preconf_radar::{CanonicalDigest, EarlyRadar, RadarInput};

/// Cadence and budgets for the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LinkConfig {
    /// Sleep between reads. 200 ms against a chain whose canonical cadence is ~1 s:
    /// the preconfirmation side exists to watch one height grow, and a 1 s read would
    /// observe at most one shape per height. Slower than the flashblock cadence the
    /// endpoint is meant to have, faster than the chain seals.
    pub read_interval_ms: u64,
    /// Consecutive read failures before the link gives up on the transport (§25:
    /// disconnect ⇒ resync, then stop rather than spin).
    pub max_consecutive_read_failures: u32,
    /// Consecutive frames that failed to decode before the run ends. A provider that
    /// changed its wire shape is a fact about the provider; the honest response is to
    /// stop claiming to see anything, not to keep emitting an empty view.
    pub max_consecutive_decode_failures: u32,
    /// Reads that answered `null` before an idle line is emitted, so a chain with no
    /// pending block is distinguishable from a link that stopped working.
    pub idle_after_reads: u32,
    /// Upper bound on the receipts list one read may contribute, counted when it bites.
    pub max_receipts_per_read: usize,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            read_interval_ms: 200,
            max_consecutive_read_failures: 8,
            max_consecutive_decode_failures: 5,
            idle_after_reads: 10,
            max_receipts_per_read: 4096,
        }
    }
}

/// §10's transport machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStage {
    /// Nothing is being read: the run has not started, or it ended.
    Disconnected,
    /// A read has been asked for and has not answered yet.
    Connecting,
    /// The transport answers. Nothing arrived in this cycle.
    Connected,
    /// A payload arrived and is being turned into a frame.
    Receiving,
    /// The frame decoded; its identity fields are being checked and it is being held.
    Validating,
    /// A read or decode failed and the budget is not yet spent.
    Recovering,
    /// A budget was spent. Held views were invalidated (§11) before the read-side
    /// `Reset` was entered, so a link that reached it did not silently keep a stale view.
    Reset,
}

impl LinkStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Receiving => "receiving",
            Self::Validating => "validating",
            Self::Recovering => "recovering",
            Self::Reset => "reset",
        }
    }

    /// §10's edge table. `Disconnected` is reachable from every stage because ending a
    /// run is always legal; the read-side edges are not.
    pub const fn can_move_to(self, next: Self) -> bool {
        use LinkStage::{
            Connected, Connecting, Disconnected, Receiving, Recovering, Reset, Validating,
        };
        match self {
            Disconnected => matches!(next, Connecting | Reset | Disconnected),
            Connecting => matches!(next, Connected | Recovering | Reset | Disconnected),
            // A completed cycle hands the transport straight back to the next read.
            Connected => matches!(
                next,
                Connecting | Receiving | Recovering | Reset | Disconnected
            ),
            Receiving => matches!(next, Validating | Recovering | Reset | Disconnected),
            Validating => matches!(next, Connected | Recovering | Reset | Disconnected),
            // Two failures in a row are one recovery in progress, not a new transition;
            // the caller stays put and only counts.
            Recovering => matches!(next, Connecting | Reset | Disconnected),
            Reset => matches!(next, Disconnected | Connecting),
        }
    }
}

/// How the run ended. `None` means the loop was still going when the report was taken,
/// which is only reachable through [`PreconfLink::report`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkEndedBy {
    StopRequested,
    /// The pipeline dropped the event sink. Reported as an error *as well as* here,
    /// because "we stopped delivering" must never look like a short clean run.
    EventQueueClosed,
    ReadFailures,
    DecodeFailures,
    /// A finite transport — a recording — ran out. Everything it holds was delivered.
    Exhausted,
}

/// What one run did, and what it never got to do. Built on every exit path, including
/// the error path (§49).
#[derive(Clone, Debug, Serialize)]
pub struct LinkRunReport {
    pub endpoint_id: String,
    pub transport: &'static str,
    pub chain_id: u64,
    pub cycles: u64,
    pub reads_answered: u64,
    /// Reads that answered `null`: the provider replied and had no pending block.
    pub reads_empty: u64,
    pub read_failures: u64,
    pub decode_failures: u64,
    pub link_resets: u64,
    /// Frames handed to the radar — the denominator for its own accepted/rejected
    /// counters, so a table can say "the link offered N, the radar kept M".
    pub frames_offered_to_radar: u64,
    /// Receipt lists read and decoded (§34: nonzero only for a replay, since a live
    /// source answers `false` to `supports_pending_receipts`).
    pub receipt_lists_decoded: u64,
    pub receipts_dropped_by_bound: u64,
    pub canonical_digests_consumed: u64,
    /// Deepest canonical backlog seen. The producer can be briefly held by it; nothing
    /// is dropped, which is why this is a high-water mark and not a loss counter.
    pub canonical_backlog_high_water: usize,
    pub events_sent: u64,
    pub ended_by: Option<LinkEndedBy>,
    pub final_stage: LinkStage,
    /// The radar's counters, carried so the run report and the tables cannot disagree.
    pub counters: RadarCounters,
}

/// What one read produced, before any state was touched.
enum ReadOutcome {
    Failed(String),
    /// The provider answered `null`.
    Empty,
    Raw(serde_json::Value),
}

enum CycleOutcome {
    /// A frame was decoded and handed to the radar.
    Produced,
    /// The provider answered `null`.
    Quiet,
    /// A read or decode failed and the budget remains.
    Retried,
    Ended(LinkEndedBy),
}

/// The run's three borrowed collaborators, in one place so `cycle` does not need six
/// arguments: the sink, the canonical channel, and the caller's clock.
struct Session<'a, C> {
    sink: &'a mpsc::Sender<RadarEvent>,
    canonical: &'a mut mpsc::Receiver<CanonicalDigest>,
    clock: &'a mut C,
    report: LinkRunReport,
}

impl<C: FnMut() -> u64> Session<'_, C> {
    fn now(&mut self) -> u64 {
        (self.clock)()
    }

    /// §42/§43: deliver in the order produced, with bounded-channel backpressure as the
    /// only mechanism. A slow consumer stops the link rather than losing events; a
    /// closed consumer ends the run with an error, not with a shorter session (§49).
    async fn deliver(&mut self, events: Vec<RadarEvent>) -> Result<(), PreconfError> {
        for event in events {
            if self.sink.send(event).await.is_err() {
                return Err(PreconfError::EventQueueClosed {
                    events_sent: self.report.events_sent,
                });
            }
            self.report.events_sent += 1;
        }
        Ok(())
    }
}

pub struct PreconfLink<F> {
    source: F,
    radar: EarlyRadar,
    config: LinkConfig,
    endpoint_id: String,
    stage: LinkStage,
    last_report: Option<LinkRunReport>,
}

impl<F: FrameSource> PreconfLink<F> {
    pub fn new(source: F, radar: EarlyRadar, config: LinkConfig) -> Self {
        let endpoint_id = radar.endpoint_id().to_string();
        Self {
            source,
            radar,
            config,
            endpoint_id,
            stage: LinkStage::Disconnected,
            last_report: None,
        }
    }

    pub const fn config(&self) -> LinkConfig {
        self.config
    }

    pub const fn stage(&self) -> LinkStage {
        self.stage
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    pub const fn radar(&self) -> &EarlyRadar {
        &self.radar
    }

    /// Mutable access for the integration layer, which is where §46's injected failures
    /// and §47's fixtures are built. The radar owns views, digests and counters and
    /// nothing that a canonical state type could be reached through.
    pub fn radar_mut(&mut self) -> &mut EarlyRadar {
        &mut self.radar
    }

    pub fn source(&self) -> &F {
        &self.source
    }

    /// The counters from the most recent run, available even when that run ended in an
    /// error (§49).
    pub fn report(&self) -> Option<LinkRunReport> {
        self.last_report.clone()
    }

    /// §10: move the transport machine, emitting the move. An illegal move is recorded
    /// and the stage is left alone, so the run continues from what it actually was
    /// rather than from what a bug asked for.
    fn move_to(&mut self, next: LinkStage, detail: &str, events: &mut Vec<RadarEvent>) {
        let from = self.stage;
        if from == next {
            return;
        }
        if !from.can_move_to(next) {
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "§10 forbids link {} -> {}; the stage stayed at {} (requested because: {detail})",
                    from.as_str(),
                    next.as_str(),
                    from.as_str()
                ),
            });
            return;
        }
        self.stage = next;
        events.push(RadarEvent::LinkStageChanged {
            from: from.as_str(),
            to: next.as_str(),
            detail: detail.to_string(),
        });
    }

    async fn read(&mut self) -> ReadOutcome {
        match self.source.next_view().await {
            Ok(Some(payload)) => ReadOutcome::Raw(payload),
            Ok(None) => ReadOutcome::Empty,
            Err(error) => ReadOutcome::Failed(error.to_string()),
        }
    }

    /// Receipts for the frame just read, if this source may have any. The bound counts
    /// what it cut rather than cutting it silently (§51). A receipt-list decode failure
    /// is a separate finding from a frame decode failure and does not feed that budget:
    /// the frame itself was readable, so the view stays, and only its log-derived pools
    /// go missing for this cycle (§28: `N/A`, never 0).
    ///
    /// A closed sink is not raised from here. The receipts are an optional extra input,
    /// and the finding that matters is the one the *frame* path reports; the next
    /// delivery in the cycle returns the error (§49).
    async fn receipts<C: FnMut() -> u64>(
        &mut self,
        session: &mut Session<'_, C>,
    ) -> Vec<PreconfReceipt> {
        if !self.source.supports_pending_receipts() {
            return Vec::new();
        }
        let raw = match self.source.next_receipts().await {
            Ok(Some(raw)) => raw,
            Ok(None) => return Vec::new(),
            Err(error) => {
                let events = vec![RadarEvent::Sequence {
                    detail: format!(
                        "receipt read failed on a source that claims to support them ({error}); the frame is still held and only its log-derived pools are missing this cycle"
                    ),
                }];
                let _ = session.deliver(events).await;
                return Vec::new();
            }
        };
        let entries = raw.as_array().map(Vec::len).unwrap_or_default();
        if entries > self.config.max_receipts_per_read {
            session.report.receipts_dropped_by_bound +=
                (entries - self.config.max_receipts_per_read) as u64;
        }
        match receipts_from_value(&raw) {
            Ok(list) => {
                session.report.receipt_lists_decoded += 1;
                list.into_iter()
                    .take(self.config.max_receipts_per_read)
                    .collect()
            }
            Err(error) => {
                session.report.decode_failures += 1;
                let events = vec![RadarEvent::Sequence {
                    detail: format!(
                        "receipt list failed to decode and was refused fail-closed ({error}); the frame is held without log-derived pools (§24)"
                    ),
                }];
                let _ = session.deliver(events).await;
                Vec::new()
            }
        }
    }

    /// Take every canonical digest that is ready, without blocking.
    ///
    /// `try_recv` rather than `recv` because the link must not be kept alive by a path
    /// it does not own (§44). Called at the top of a cycle *and* again when a read found
    /// nothing, because a digest that arrived while the link was blocked on its provider
    /// would otherwise wait a full extra cycle — and on the exhausted-reading path that
    /// cycle never comes.
    async fn drain_canonical<C: FnMut() -> u64>(
        &mut self,
        session: &mut Session<'_, C>,
    ) -> Result<(), PreconfError> {
        while let Ok(digest) = session.canonical.try_recv() {
            session.report.canonical_digests_consumed += 1;
            let closing = self.radar.note_canonical(&digest, &mut session.clock);
            session.deliver(closing).await?;
        }
        session.report.canonical_backlog_high_water = session
            .report
            .canonical_backlog_high_water
            .max(session.canonical.len());
        Ok(())
    }

    /// Drain the canonical channel without blocking, then read once and apply.
    async fn cycle<C: FnMut() -> u64>(
        &mut self,
        session: &mut Session<'_, C>,
        quiet_reads: &mut u32,
        read_streak: &mut u32,
        decode_streak: &mut u32,
        stop: &AtomicBool,
    ) -> Result<CycleOutcome, PreconfError> {
        let mut events = Vec::new();
        self.move_to(
            LinkStage::Connecting,
            "ask the provider for the next pending payload",
            &mut events,
        );
        session.deliver(events).await?;
        let outcome = self.read().await;

        // — Canonical first: a sealed block closes views, and a close must not queue
        // behind a read.
        self.drain_canonical(session).await?;

        // The read answered (or did not). `Connecting`'s counterpart is *this* move: a
        // payload is only shaped into a frame once the transport is known to have
        // answered, which is what makes `Receiving` mean something in the table.
        if !matches!(outcome, ReadOutcome::Failed(_)) {
            let mut answered = Vec::new();
            self.move_to(
                LinkStage::Connected,
                "the transport answered this read",
                &mut answered,
            );
            session.deliver(answered).await?;
        }
        match outcome {
            ReadOutcome::Failed(detail) => {
                session.report.read_failures += 1;
                *read_streak += 1;
                let events = vec![RadarEvent::Sequence {
                    detail: format!(
                        "provider read failed ({detail}); consecutive failures {read_streak}"
                    ),
                }];
                session.deliver(events).await?;
                let budget_spent = *read_streak == self.config.max_consecutive_read_failures;
                let mut events = Vec::new();
                if budget_spent {
                    self.move_to(
                        LinkStage::Recovering,
                        "the read budget is spent; §11 invalidates held views before the run ends",
                        &mut events,
                    );
                    // §11 + §25, in this order on purpose: the invalidation events are
                    // produced while the report is still being built, so a failed run's
                    // evidence shows *what was thrown away* rather than a view that
                    // quietly stopped being updated.
                    let invalidations = self.radar.on_disconnect(&format!(
                        "{detail} after {read_streak} consecutive failures"
                    ));
                    session.report.link_resets += 1;
                    self.move_to(LinkStage::Reset, "read budget spent", &mut events);
                    events.extend(invalidations);
                    session.deliver(events).await?;
                    return Ok(CycleOutcome::Ended(LinkEndedBy::ReadFailures));
                }
                // A second failure inside the same recovery is not a new transition.
                if *read_streak > 1 {
                    return Ok(CycleOutcome::Retried);
                }
                self.move_to(LinkStage::Recovering, "a read failed", &mut events);
                session.deliver(events).await?;
                Ok(CycleOutcome::Retried)
            }
            ReadOutcome::Empty => {
                *read_streak = 0;
                session.report.reads_empty += 1;
                *quiet_reads += 1;
                let mut events = Vec::new();
                if *quiet_reads >= self.config.idle_after_reads {
                    *quiet_reads = 0;
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "{} consecutive reads answered null: the provider is up and has no pending block to show. This is idle, not a dead link.",
                            self.config.idle_after_reads
                        ),
                    });
                }
                events.extend(self.radar.tick());
                session.deliver(events).await?;
                self.drain_canonical(session).await?;
                if self.source.is_finite() && self.source.finished() {
                    let mut events = Vec::new();
                    self.move_to(
                        LinkStage::Disconnected,
                        "the recording ran out; everything it holds was delivered",
                        &mut events,
                    );
                    session.deliver(events).await?;
                    return Ok(CycleOutcome::Ended(LinkEndedBy::Exhausted));
                }
                Ok(CycleOutcome::Quiet)
            }
            ReadOutcome::Raw(payload) => {
                *read_streak = 0;
                *quiet_reads = 0;
                session.report.reads_answered += 1;
                self.apply(payload, session, decode_streak, stop).await
            }
        }
    }

    /// Decode one payload and, if it survives, hold it. Split out so the read-side
    /// failure branch above stays legible.
    async fn apply<C: FnMut() -> u64>(
        &mut self,
        payload: serde_json::Value,
        session: &mut Session<'_, C>,
        decode_streak: &mut u32,
        stop: &AtomicBool,
    ) -> Result<CycleOutcome, PreconfError> {
        let mut events = Vec::new();
        self.move_to(
            LinkStage::Receiving,
            "a pending payload arrived; it becomes a frame or a failure, never a partial frame",
            &mut events,
        );
        session.deliver(events).await?;

        // §8's identity is assigned here, in read order: the local frame sequence is this
        // loop's counter, not a wire field (the measured endpoint has none), and a frame
        // that fails to decode has still consumed a read — which is why the number is
        // taken before the decoder runs rather than after it succeeds.
        let local_frame_sequence = self.radar.next_local_sequence();
        let observed_at_unix_ms = session.now();
        let chain_id = self.radar.chain_id();
        let frame = match frame_from_pending_value(
            chain_id,
            &payload,
            local_frame_sequence,
            observed_at_unix_ms,
            &self.endpoint_id,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                session.report.decode_failures += 1;
                *decode_streak += 1;
                let mut events = vec![RadarEvent::Sequence {
                    detail: format!(
                        "frame {local_frame_sequence} failed to decode and was refused fail-closed ({error}); the held view was not touched (§24)"
                    ),
                }];
                // `Receiving` is where the payload died, so the machine says so: the
                // recovery stage is entered whether or not the budget is spent.
                self.move_to(
                    LinkStage::Recovering,
                    "a frame failed to decode",
                    &mut events,
                );
                if *decode_streak == self.config.max_consecutive_decode_failures {
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "{decode_streak} consecutive frames failed to decode: the wire shape is not what this decoder was characterised against, so the run ends rather than reporting an empty view forever (§25)"
                        ),
                    });
                    self.move_to(LinkStage::Reset, "decode budget spent", &mut events);
                    session.deliver(events).await?;
                    return Ok(CycleOutcome::Ended(LinkEndedBy::DecodeFailures));
                }
                session.deliver(events).await?;
                return Ok(CycleOutcome::Retried);
            }
        };
        *decode_streak = 0;

        let decoded_at_unix_ms = session.now();
        let receipts = self.receipts(session).await;
        let mut events = Vec::new();
        self.move_to(
            LinkStage::Validating,
            "the frame decoded; identity, parent, duplication and lateness are checked",
            &mut events,
        );
        session.deliver(events).await?;

        let input = RadarInput {
            frame,
            receipts: (!receipts.is_empty()).then_some(receipts),
            decoded_at_unix_ms,
        };
        session.report.frames_offered_to_radar += 1;
        let held = self.radar.ingest(input, &mut session.clock);
        session.deliver(held).await?;
        let mut events = self.radar.tick();
        self.move_to(
            LinkStage::Connected,
            "the cycle delivered; this read is closed",
            &mut events,
        );
        session.deliver(events).await?;
        if stop.load(Ordering::Relaxed) {
            return Ok(CycleOutcome::Ended(LinkEndedBy::StopRequested));
        }
        Ok(CycleOutcome::Produced)
    }

    /// The run. `clock` is supplied by the caller and never chosen here, so a replay can
    /// hand back the stamps the original capture wrote and reproduce byte-identical
    /// evidence (§41, §43).
    /// Generic over the clock rather than `dyn FnMut` so a caller can move the run into a
    /// task: a trait object of an auto-trait-free signature is not `Send`, and "one consumer
    /// in one task" (§42) has to be expressible on a runtime that schedules tasks.
    pub async fn run<C>(
        &mut self,
        sink: mpsc::Sender<RadarEvent>,
        mut canonical: mpsc::Receiver<CanonicalDigest>,
        clock: &mut C,
        stop: Arc<AtomicBool>,
    ) -> Result<LinkRunReport, PreconfError>
    where
        C: FnMut() -> u64,
    {
        let mut session = Session {
            sink: &sink,
            canonical: &mut canonical,
            clock,
            report: LinkRunReport {
                endpoint_id: self.endpoint_id.clone(),
                transport: self.source.transport(),
                chain_id: self.radar.chain_id().0,
                cycles: 0,
                reads_answered: 0,
                reads_empty: 0,
                read_failures: 0,
                decode_failures: 0,
                link_resets: 0,
                frames_offered_to_radar: 0,
                receipt_lists_decoded: 0,
                receipts_dropped_by_bound: 0,
                canonical_digests_consumed: 0,
                canonical_backlog_high_water: 0,
                events_sent: 0,
                ended_by: None,
                final_stage: LinkStage::Disconnected,
                counters: *self.radar.counters(),
            },
        };
        let mut quiet_reads = 0u32;
        let mut read_streak = 0u32;
        let mut decode_streak = 0u32;
        let outcome = loop {
            if stop.load(Ordering::Relaxed) {
                break LinkEndedBy::StopRequested;
            }
            session.report.cycles += 1;
            match self
                .cycle(
                    &mut session,
                    &mut quiet_reads,
                    &mut read_streak,
                    &mut decode_streak,
                    stop.as_ref(),
                )
                .await
            {
                Ok(CycleOutcome::Produced) => {}
                Ok(CycleOutcome::Quiet) | Ok(CycleOutcome::Retried) => {}
                Ok(CycleOutcome::Ended(reason)) => break reason,
                Err(error) => {
                    session.report.counters = *self.radar.counters();
                    session.report.final_stage = self.stage;
                    self.last_report = Some(session.report.clone());
                    return Err(error);
                }
            }
            if self.config.read_interval_ms > 0 && !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(self.config.read_interval_ms)).await;
            }
        };
        session.report.ended_by = Some(outcome);
        let mut events = Vec::new();
        self.move_to(
            LinkStage::Disconnected,
            "the run ended; the sink is closed and no view is left being fed",
            &mut events,
        );
        // Failing to deliver the closing line is not a reason to lose the report — the
        // report is the evidence, and §49 asks a failed session to flush it too.
        let closing = session.deliver(events).await;
        session.report.counters = *self.radar.counters();
        session.report.final_stage = self.stage;
        self.last_report = Some(session.report.clone());
        closing.map(|()| session.report)
    }
}
