//! The run: §7's bootstrap order, §4's one ordered stream, §49's shutdown.
//!
//! Three rules decide the shape of this file.
//!
//! **Bootstrap (§7/§8).** The head is read *before* a subscription is asked for
//! and before any block is processed, because a source that subscribes first and
//! snapshots second can miss the block that landed between the two. From the head
//! the run moves forward only. If a number in between never arrives, the source
//! reports an unrecovered gap and this file stops the run — §8's 「不得假装同步
//! 完成」 is implemented here as `PipelineError::GapUnrecovered`, not as a shorter
//! session that reads like a complete one.
//!
//! **One stream (§9).** Every producer — WebSocket, HTTP polling, the recorded
//! directory, the candidate watcher — writes [`MarketEvent`]s into one bounded
//! channel, and exactly one consumer reads them. Canonical blocks advance state
//! through [`crate::engine::MarketEngine`]; candidates advance nothing. A source
//! that got to keep its own state path would be the second pipeline §9 forbids.
//!
//! **Shutdown (§49).** `stop` is set on the duration elapsing, on `max_blocks`
//! being reached, on Ctrl-C and on SIGTERM. Then the senders are dropped in the
//! order that lets each stage leave its `recv`, the source tasks are joined so
//! their capability tables survive, the simulation workers are joined so their
//! per-thread counts survive, and only then are `metrics.json` and
//! `live-session.json` written — through a temporary name and a rename, so the
//! file a crash leaves behind is the previous complete one rather than half of
//! this one.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use evm_chain::{
    ChainAdapter, EndpointPurpose, HeadFreshnessPolicy, HttpChainAdapter, Readiness, ReadinessGate,
    RecordedChainAdapter,
};
use evm_core::BlockNumber;
use evm_execution::{
    journal_stamp, AttemptProvenance, ExecutionJournal, ExecutionStage, Freshness, SenderFunding,
};
use evm_live::{
    now_unix_ms, BlockAnnouncement, FlashblockSource, MarketDataSource, MarketEvent, PollingSource,
    SourceKind, SourceStatus, WebSocketSource,
};
use evm_metrics::{Clock, LatencyTrace, PipelineTiming, Stage, TraceSource};
use evm_opportunity::Opportunity;
use evm_protocol::Registry;
use evm_replay::StateChange;
use evm_risk::RiskPolicy;
use evm_simulation::StateDump;
use evm_state::UpdatePosition;

use crate::config::{CanonicalSource, PipelineConfig};
use crate::engine::{BlockOutcome, MarketEngine};
use crate::error::{PipelineError, Result};
use crate::evidence::{session_record, EvidenceFile, EvidenceWriter};
use crate::latency::{git_revision, record_lifecycle, LatencyEvidence, TraceRecorder};
use crate::sim::{
    decline_line, plan_job, Decline, JobPlan, SimOutcome, SimRun, SimulationJob, SimulationPool,
    WorkerReport,
};

/// What a source task had to say once it stopped: its counters, and the table of
/// what its endpoint could actually do (§41, §73).
#[derive(Clone, Debug, serde::Serialize)]
pub struct SourceCompletion {
    pub source: &'static str,
    pub capability: Value,
    pub report: Option<Value>,
    pub error: Option<String>,
}

/// The run's outcome, in the shape the CLI prints.
#[derive(Clone, Debug)]
pub struct SessionReport {
    pub session_id: String,
    pub ended_by: String,
    pub blocks: u64,
    pub events: u64,
    pub simulations: u64,
    pub accepts: u64,
    pub rejects: u64,
    pub unknowns: u64,
    /// M6's two boundaries, as counts: attempts the lane was given, and attempts
    /// whose bytes reached a node. Both zero for a run with no lane.
    pub executions: u64,
    pub sent: u64,
    pub evidence_dir: PathBuf,
    pub metrics: Value,
    pub session: Value,
    pub sources: Vec<SourceCompletion>,
    pub workers: Vec<WorkerReport>,
}

/// The stages and the record of one session.
///
/// Holding the engine and the evidence writer together is what makes the
/// per-stage writes unconditional: a caller cannot process a block without going
/// through the method that records it (§51).
struct Session<'a> {
    config: &'a PipelineConfig,
    engine: MarketEngine,
    evidence: EvidenceWriter,
    blocks: u64,
    events: u64,
    simulations: u64,
    accepts: u64,
    rejects: u64,
    unknowns: u64,
    last_block: Option<BlockNumber>,
    /// Findings handed to a worker whose answer has not come back yet. §49's
    /// shutdown drain waits on exactly this number.
    jobs_in_flight: u64,
    /// M6's lane, when the run asked for one. `None` is M5's run: the loop ends
    /// at the risk decision and nothing downstream is even constructed.
    execution: Option<ExecutionStage>,
    /// Attempts the lane was asked about, and how many of them let bytes leave
    /// the process. Two numbers because §2.2's whole point is that they are not
    /// one number: an accepted finding with a built transaction is an attempt that
    /// sent nothing.
    executions: u64,
    sent: u64,
    /// M8.1's latency traces, when the run was asked for them: one `traces.jsonl` line
    /// per finding's lifecycle, assembled from stamps this run already took (§15).
    /// `None` is M5–M7's run, and the difference between the two is one extra file —
    /// no stage moves, waits, or re-reads anything.
    latency: Option<LatencyEvidence>,
    /// §8: the reason a run must be reported as failed rather than as finished.
    fatal: Option<PipelineError>,
}

impl<'a> Session<'a> {
    fn new(
        config: &'a PipelineConfig,
        engine: MarketEngine,
        evidence: EvidenceWriter,
        execution: Option<ExecutionStage>,
        latency: Option<LatencyEvidence>,
    ) -> Self {
        Self {
            config,
            engine,
            evidence,
            blocks: 0,
            events: 0,
            simulations: 0,
            accepts: 0,
            rejects: 0,
            unknowns: 0,
            last_block: None,
            jobs_in_flight: 0,
            execution,
            executions: 0,
            sent: 0,
            latency,
            fatal: None,
        }
    }

    /// §75's human-readable line, when the run was asked to be readable.
    /// Everything printed here is also in an evidence file; the reverse is not
    /// true, and stdout is never the only place a fact lives.
    fn say(&self, line: impl std::fmt::Display) {
        if self.config.progress {
            println!("{line}");
        }
    }

    /// M6's part of the session record: what the run was allowed to do after the
    /// risk decision, which wallet it could have moved (§19's question), and what
    /// it actually did with the two.
    ///
    /// A lane-less run still gets the whole object, with the counts read from
    /// fields that were never touched rather than from a `null` a reader has to
    /// interpret: "no executions were attempted, because there was no lane" is the
    /// sentence §48's record has to be able to say on its own.
    fn execution_summary(&self) -> Value {
        json!({
            "configuration": self.config.execution_description(),
            "attempts": self.executions,
            "signed_bytes_to_a_node": self.sent,
            "lifecycle_records": self
                .execution
                .as_ref()
                .map(|stage| stage.ledger().len()),
            "signer": self.execution.as_ref().map(|stage| match stage.signer_address() {
                Ok(address) => format!("{address:#x}"),
                // The mode's own reason it has no account — which for `build-only`
                // is the point, stated by the signer rather than inferred from an
                // empty field.
                Err(reason) => reason.to_string(),
            }),
        })
    }

    /// How many findings one block may be handed on. Bounded by the queue's own
    /// capacity, so the number in the config and the number in force cannot
    /// drift apart (§50).
    fn max_jobs_per_block(&self) -> usize {
        self.config.queues.simulation_capacity.max(1)
    }

    /// One event from any producer. Only a canonical announcement does work; the
    /// rest are recorded, counted, and in one case end the run.
    async fn on_event(
        &mut self,
        event: MarketEvent,
        sim: &mpsc::Sender<SimulationJob>,
        forward: &Option<mpsc::Sender<BlockAnnouncement>>,
    ) -> Result<()> {
        self.events += 1;
        self.engine
            .metrics_mut()
            .bump(&format!("event.{}", event.kind_label()));
        self.evidence.line(EvidenceFile::Events, &event.to_json())?;
        match event {
            MarketEvent::Canonical(announcement) => {
                // §28's mapping needs the candidate window to be told what sealed.
                // This is a hint channel, bounded, and a full hint channel costs a
                // reconciliation row — never a market event (§50).
                if let Some(sender) = forward {
                    match sender.try_send(announcement) {
                        Ok(()) => self
                            .engine
                            .metrics_mut()
                            .bump("canonical_forwarded_for_reconciliation"),
                        Err(TrySendError::Full(_)) => {
                            self.engine
                                .metrics_mut()
                                .backpressure("reconciliation_queue");
                            self.engine
                                .metrics_mut()
                                .bump("canonical_not_forwarded_queue_full");
                        }
                        Err(TrySendError::Closed(_)) => self
                            .engine
                            .metrics_mut()
                            .bump("canonical_not_forwarded_source_gone"),
                    }
                }
                let outcome = self
                    .engine
                    .on_canonical(&announcement, self.max_jobs_per_block())
                    .await?;
                self.blocks += 1;
                self.last_block = Some(announcement.number);
                self.write_block(&announcement, &outcome)?;
                self.dispatch(&outcome, sim).await?;
            }
            MarketEvent::Candidate(candidate) => {
                self.evidence
                    .line(EvidenceFile::Candidates, &json!({"candidate": candidate}))?;
            }
            MarketEvent::CandidateResolved { .. } | MarketEvent::CandidateExpired { .. } => {
                // The candidate stream's own verdicts on itself: §28 and §30.
                self.evidence
                    .line(EvidenceFile::Candidates, &event.to_json())?;
            }
            MarketEvent::GapDetected { from, to } => {
                // §5's question, asked before recovery is attempted. The gap's
                // outcome arrives later as `MarketEvent::Gap`; both rows belong in
                // the status file, because "we saw the hole" and "we filled it" are
                // separate facts and a run can have the first without the second.
                self.evidence.line(
                    EvidenceFile::Status,
                    &json!({"gap_detected": {"from": from, "to": to}}),
                )?;
            }
            MarketEvent::Gap(gap) => {
                self.evidence
                    .line(EvidenceFile::Status, &json!({"gap": gap}))?;
                if let evm_live::GapOutcome::Unrecovered {
                    from,
                    to,
                    missing,
                    attempts,
                    detail,
                } = gap
                {
                    self.fatal = Some(PipelineError::GapUnrecovered {
                        from,
                        to,
                        detail: format!(
                            "{missing} block(s) never came back after {attempts} attempts: {detail}"
                        ),
                    });
                }
            }
            MarketEvent::Status(status) => {
                self.evidence
                    .line(EvidenceFile::Status, &json!({"status": status}))?;
                if let SourceStatus::Failed { source, .. } = &status {
                    // A failed *canonical* source is how the session must be read:
                    // the fallback that kept it company is the other source's story,
                    // and both are in the record. The reason itself travels in the
                    // status.jsonl line written above (§53, not a panic).
                    self.engine
                        .metrics_mut()
                        .bump(&format!("source_failed.{}", source.as_str()));
                    if source.is_canonical() {
                        self.engine
                            .metrics_mut()
                            .bump("canonical_source_failed_during_run");
                    }
                }
            }
            MarketEvent::Duplicate { .. }
            | MarketEvent::CanonicalityConflict { .. }
            | MarketEvent::StaleAnnouncement { .. }
            | MarketEvent::Unknown { .. } => {
                // Counted above and written to events.jsonl; the labels that make
                // them interesting are already in the counter table.
            }
        }
        Ok(())
    }

    /// §41's three records for one processed block: the block itself with its
    /// stage timings, one line per accepted state change, one per finding.
    fn write_block(
        &mut self,
        announcement: &BlockAnnouncement,
        outcome: &BlockOutcome,
    ) -> Result<()> {
        let deltas: Vec<Value> = outcome
            .timing
            .deltas()
            .iter()
            .map(|(name, ms)| json!({"hop": name, "ms": ms}))
            .collect();
        self.evidence.line(
            EvidenceFile::Blocks,
            &json!({
                "announcement": announcement,
                "scanned": outcome.scanned,
                "state_version": outcome.state_version,
                "logs": outcome.report.logs,
                "sync_events": outcome.report.sync_events,
                "swap_events": outcome.report.swap_events,
                "pool_created_events": outcome.report.pool_created_events,
                "syncs_applied": outcome.report.syncs_applied,
                "registrations": outcome.report.registrations,
                "rejected_syncs": outcome.report.rejected_syncs,
                "rejections": outcome.report.rejections,
                "unclaimed_logs": outcome.report.unclaimed_logs,
                "unattested_syncs": outcome.report.unattested_syncs,
                "graph_edges": outcome.graph.as_ref().map(|build| build.graph.edge_count()),
                "graph_skipped_pools": outcome.graph.as_ref().map(|build| build.skipped.len()),
                "changed_pools": outcome
                    .changed_pools
                    .iter()
                    .map(|pool| format!("{:#x}", pool.address))
                    .collect::<Vec<_>>(),
                "invalidated": outcome
                    .invalidated
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>(),
                "opportunities": outcome
                    .detection
                    .as_ref()
                    .map(|found| found.opportunities.len()),
                "candidates": outcome.detection.as_ref().map(|found| found.candidates.len()),
                "dispatched_this_block": outcome.to_simulate.len(),
                "timing": deltas,
                "chain_to_received_ms": outcome.timing.chain_to_received_ms(),
            }),
        )?;
        for change in &outcome.report.state_changes {
            self.evidence
                .line(EvidenceFile::StateUpdates, &state_change_line(change))?;
        }
        if let (Some(found), Some(version)) = (outcome.detection.as_ref(), outcome.state_version) {
            for opportunity in &found.opportunities {
                let line = opportunity_line(opportunity, version);
                self.evidence.line(EvidenceFile::Opportunities, &line)?;
                // §75's finding line: the pools are the two hops' own addresses.
                let pools: Vec<&Value> = line["pools"].as_array().unwrap().iter().collect();
                self.say(format!(
                    "opportunity={} observed_block={} state_version={} pool_a={} pool_b={} gross={}",
                    line["opportunity_id"].as_str().unwrap(),
                    line["observed_block"],
                    line["state_version"]["block_number"],
                    pools.first().map(|p| p["pool"].as_str().unwrap()).unwrap_or("-"),
                    pools.get(1).map(|p| p["pool"].as_str().unwrap()).unwrap_or("-"),
                    line["gross_profit"].as_str().unwrap(),
                ));
            }
        }
        // §75's per-block line, with the two aggregate latencies it asks for.
        let hop = |name: &str| {
            outcome
                .timing
                .deltas()
                .iter()
                .find(|(hop, _)| *hop == name)
                .map(|(_, ms)| ms.to_string())
                .unwrap_or_else(|| "-".to_string())
        };
        self.say(format!(
            "block={} state_updates={} graph_edges={} opportunities={} latency: block_to_state={}ms block_to_opportunity={}ms chain_to_received={}ms",
            announcement.number.0,
            outcome.report.state_changes.len(),
            outcome.graph.as_ref().map_or(0, |build| build.graph.edge_count()),
            outcome.detection.as_ref().map_or(0, |found| found.opportunities.len()),
            hop("block_to_state"),
            hop("block_to_opportunity"),
            outcome.timing.chain_to_received_ms(),
        ));
        Ok(())
    }

    /// Hand this block's simmable findings to the simulation stage.
    ///
    /// The header a job is built against is read at **the finding's own height**
    /// (§18), so `latest` is not a thing this path could accidentally pass: the
    /// only block number available here is `state_version.block_number`.
    async fn dispatch(
        &mut self,
        outcome: &BlockOutcome,
        sim: &mpsc::Sender<SimulationJob>,
    ) -> Result<()> {
        let clock = self.engine.clock();
        for entry in &outcome.to_simulate {
            let pinned = entry.state_version.block_number;
            let id = entry.id;
            let observed_block = entry.opportunity.block_number;
            // A finding's state is read at its own height, and the block at that
            // height has to be the one this pipeline sealed when it priced the
            // finding. The same number under a different hash is a reorg: the
            // finding describes a chain that no longer exists, whatever the route
            // would compute on the new one.
            let header = match self.engine.state().header_at(pinned).await {
                Ok(header) if header.hash != entry.pinned_block_hash => Err(format!(
                    "block {} was sealed to this pipeline as {} and the state source now \
                     describes it as {}",
                    pinned.0, entry.pinned_block_hash, header.hash,
                )),
                Ok(header) => Ok(header),
                Err(reason) => Err(reason),
            };
            let plan = match header {
                Err(reason) => JobPlan::Declined(Decline {
                    id,
                    observed_block,
                    reason,
                    rule: "state_unavailable",
                }),
                Ok(header) => plan_job(
                    entry,
                    &header,
                    self.engine.state(),
                    self.config.wrapped_native,
                    &self.config.risk,
                    clock,
                    outcome.timing,
                ),
            };
            match plan {
                JobPlan::Ready(job) => match sim.try_send(*job) {
                    Ok(()) => {
                        self.jobs_in_flight += 1;
                        self.engine.metrics_mut().bump("simulation_enqueued");
                        self.engine.note_settled(&id);
                    }
                    // §23: a full queue is the pipeline refusing to wait for REVM.
                    // The finding stays undispatched so a later block can ask again,
                    // and the refusal is a line in declines.jsonl rather than a
                    // silence.
                    Err(TrySendError::Full(job)) => {
                        self.engine.metrics_mut().backpressure("simulation_queue");
                        self.evidence.line(
                            EvidenceFile::Declines,
                            &json!({
                                "opportunity_id": id.to_string(),
                                "observed_block": job.observed_block.0,
                                "rule": "simulation_queue_full",
                                "reason": format!(
                                    "the simulation queue holds {} jobs and ingestion will not wait \
                                     for one to free up (§23); the finding stays in the ledger and \
                                     is dispatched again if it is still simmable",
                                    self.config.queues.simulation_capacity,
                                ),
                            }),
                        )?;
                    }
                    Err(TrySendError::Closed(_)) => {
                        self.engine.metrics_mut().bump("simulation_queue_closed");
                        self.evidence.line(
                            EvidenceFile::Declines,
                            &json!({
                                "opportunity_id": id.to_string(),
                                "observed_block": observed_block.0,
                                "rule": "simulation_queue_closed",
                                "reason": "no simulation worker is reading the queue any more",
                            }),
                        )?;
                    }
                },
                JobPlan::Declined(decline) => {
                    self.engine
                        .metrics_mut()
                        .bump(&format!("decline.{}", decline.rule));
                    self.evidence
                        .line(EvidenceFile::Declines, &decline_line(&decline))?;
                    // A decline of this kind is stated once per finding. Its reason
                    // — the state this run cannot name, the asset it may not fund,
                    // the pin that does not match — is a property of the finding and
                    // the run, not of the queue, so offering the same finding again
                    // next block would only say the same sentence louder.
                    self.engine.note_settled(&decline.id);
                }
            }
        }
        Ok(())
    }

    /// §32's latency trace for one finding's lifecycle, written beside the run's own
    /// evidence when the run asked for it.
    ///
    /// Nothing here measures. Every figure [`crate::latency::record_lifecycle`] puts in
    /// the trace is a reading of an instant `PipelineTiming` already took — M5's stamps,
    /// carried on the block and then handed to the worker on the job — so a lifecycle's
    /// trace costs one map per stage and one line per finding, and asks the node nothing
    /// it was not already asked (§15). `detail` is this run's own sentence for why the
    /// stages below the stop are empty, and it lands beside each skipped row (§9).
    ///
    /// The source is the run's, not a label someone configured: a replay of recorded
    /// blocks produces `Replay` rows and an endpoint produces `Live` ones, so §45's rule
    /// that the two never blend into one percentile is a consequence of which producer
    /// answered rather than of a flag that could be set wrong.
    ///
    /// `state_source` is that same provider's name for itself, carried to §11 for the one
    /// stage whose span may hold a node inside it: a simulation that computed on state
    /// read from an endpoint did not spend that time in this process.
    fn note_lifecycle(
        &mut self,
        timing: &PipelineTiming,
        opportunity_id: &str,
        observed_block: BlockNumber,
        execution_id: Option<&str>,
        state_source: &str,
        detail: &str,
    ) -> Result<()> {
        let Some(evidence) = self.latency.as_mut() else {
            return Ok(());
        };
        let trace = LatencyTrace::new(
            match self.config.canonical_source {
                CanonicalSource::Replay { .. } => TraceSource::Replay,
                _ => TraceSource::Live,
            },
            self.engine.chain_id().0,
            Some(observed_block.0),
            Some(opportunity_id),
        );
        let mut recorder = TraceRecorder::on(self.engine.clock(), trace);
        let ladder = execution_id.and_then(|id| {
            self.execution
                .as_ref()
                .and_then(|stage| stage.ledger().get(id))
        });
        // With no lane there is no record either, so the default here is never read: the
        // ladder is empty and every stage below the decision is a `never reached` skip.
        let mode = self
            .execution
            .as_ref()
            .map(|stage| stage.mode())
            .unwrap_or_default();
        record_lifecycle(&mut recorder, timing, ladder, mode, detail);
        recorder.classify_reads(Stage::Simulation, state_source);
        let session_id = self.evidence.session_id().to_string();
        evidence.record(recorder.finish().with_session(session_id))
    }

    /// A finished run, and the only place a simulation result may become a risk
    /// decision (§24).
    async fn on_outcome(&mut self, outcome: SimOutcome) -> Result<()> {
        self.jobs_in_flight = self.jobs_in_flight.saturating_sub(1);
        self.simulations += 1;
        self.engine
            .metrics_mut()
            .record_latency("simulation_queue_wait", outcome.queue_wait_ms);
        self.engine.note_simulated(&outcome.timing);
        let mut line = simulation_line(&outcome);
        let result = match &outcome.run {
            SimRun::Executed(result) => Some(result),
            SimRun::Refused(reason) => {
                self.engine.metrics_mut().bump("simulation_refused");
                line["refused"] = Value::String(reason.clone());
                self.evidence.line(EvidenceFile::SimulationResults, &line)?;
                self.say(format!(
                    "simulation=refused opportunity={} detail={reason}",
                    outcome.id
                ));
                // §32's trace for a lifecycle that ended at the EVM's door: the stages
                // with both of M5's stamps are timed, and every stage after them is a
                // skip carrying this refusal as its reason (§9).
                let detail = format!("the simulation produced no answer: {reason}");
                self.note_lifecycle(
                    &outcome.timing,
                    &outcome.id.to_string(),
                    outcome.observed_block,
                    None,
                    &outcome.state_source,
                    &detail,
                )?;
                return Ok(());
            }
        };
        // §24/§65: the finding has to still be true of the market for its run to
        // be a decision about an executable opportunity. A stale result is
        // evidence about a state that no longer exists, so it stops here.
        if self.engine.live_finding(&outcome.id).is_none() {
            self.engine
                .metrics_mut()
                .bump("simulation_result_suppressed_stale");
            line["risk"] = json!({
                "evaluated": false,
                "detail": "the finding was invalidated while this run was in flight, so it never \
                           reached the risk layer (§24)",
            });
            self.evidence.line(EvidenceFile::SimulationResults, &line)?;
            self.say(format!(
                "simulation=suppressed_stale opportunity={} detail=invalidated while in flight, \
                 never reached the risk layer (§24)",
                outcome.id,
            ));
            // The simulation answered; the market moved on. §13's end-to-end figures
            // for this finding are therefore N/A rather than zero, and the trace says
            // which stages ran and which were never reached, in this run's own words.
            self.note_lifecycle(
                &outcome.timing,
                &outcome.id.to_string(),
                outcome.observed_block,
                None,
                &outcome.state_source,
                "the finding was invalidated while its own run was in flight, so it never \
                 reached the risk layer (§24)",
            )?;
            return Ok(());
        }
        let decision = match result {
            Some(run) => outcome.risk.evaluate(run.as_ref()),
            None => unreachable!("the refused case returned above"),
        };
        let label = match &decision {
            evm_risk::RiskDecision::Accept { .. } => "accept",
            evm_risk::RiskDecision::Reject { .. } => "reject",
            evm_risk::RiskDecision::Unknown { .. } => "unknown",
        };
        match label {
            "accept" => self.accepts += 1,
            "reject" => self.rejects += 1,
            _ => self.unknowns += 1,
        }
        self.engine.metrics_mut().bump(&format!("risk.{label}"));
        let mut timing = outcome.timing;
        timing.set_risk_decided(self.engine.clock().now_ms());
        self.engine.note_risk_decided(&timing);
        line["risk"] = json!({
            "evaluated": true,
            "decision": label,
            "thresholds": outcome.risk,
            "deltas": timing
                .deltas()
                .iter()
                .map(|(name, ms)| json!({"hop": name, "ms": ms}))
                .collect::<Vec<_>>(),
        });
        self.evidence.line(EvidenceFile::SimulationResults, &line)?;
        self.evidence.line(
            EvidenceFile::RiskDecisions,
            &json!({
                "opportunity_id": outcome.id.to_string(),
                "observed_block": outcome.observed_block.0,
                "state_version": outcome.state_version,
                "decision": decision,
                "decision_line": decision.to_string(),
                "no_broadcast": evm_risk::NO_BROADCAST,
            }),
        )?;
        // §75's closing line for one finding: what the chain code said it cost,
        // what it said it made, and what the risk layer did with that.
        let hop = |name: &str| {
            timing
                .deltas()
                .iter()
                .find(|(hop, _)| *hop == name)
                .map(|(_, ms)| ms.to_string())
                .unwrap_or_else(|| "-".to_string())
        };
        let (status, gas_used, net_profit) = match result {
            Some(run) => (
                format!("{:?}", run.status),
                run.gas_used().to_string(),
                format!("{:?}", run.net_profit),
            ),
            None => ("not executed".to_string(), "-".to_string(), "-".to_string()),
        };
        self.say(format!(
            "simulation={status} gas={gas_used} net_profit={net_profit} risk={decision} {} \
             latency: block_to_simulation={}ms block_to_risk={}ms queue_wait={}ms",
            label.to_uppercase(),
            hop("block_to_simulation"),
            hop("block_to_risk"),
            outcome.queue_wait_ms,
        ));
        let execution_id = self
            .on_execution(&outcome, &decision, result.map(|run| run.as_ref()))
            .await?;
        // §32's trace for a lifecycle that reached the risk layer. Why the stages below
        // the decision are empty is this run's own sentence, and the three cases say
        // different things: a declined finding never asked for a lane, an accepted
        // finding in a lane-less run had no lane to ask for, and an attempt that did
        // reach one is described by the ladder's stamps rather than by this string.
        let detail = match (decision.accepted(), self.execution.is_some()) {
            (true, true) => "the lane took this finding, and every stage of the ladder it \
                             never reached has no stamp to read"
                .to_string(),
            (true, false) => "this run has no execution lane, so the risk decision is where \
                              the lifecycle ends (§31)"
                .to_string(),
            (false, _) => format!("the risk layer declined this finding ({label})"),
        };
        self.note_lifecycle(
            &timing,
            &outcome.id.to_string(),
            outcome.observed_block,
            execution_id.as_deref(),
            &outcome.state_source,
            &detail,
        )
    }

    /// M6's lane, and the only place in this crate that reaches past a risk
    /// decision. A run without a configured lane returns here having done nothing,
    /// which is M5's behaviour kept as the default rather than as a flag that
    /// changes semantics somewhere else (§20).
    ///
    /// Three rules decide what crosses, and this file re-decides none of them:
    ///
    /// - the input is a `RiskDecision` and a `SimulationResult`, never an
    ///   `Opportunity` (§45);
    /// - a finding the risk layer did not accept is counted and not written —
    ///   `executions.jsonl` is the file of *attempts*, and §51 forbids the silent
    ///   drop rather than the loud skip;
    /// - whether the sender was funded by the chain or by a simulation override is
    ///   quoted out of the request that applied it, because §34's whole question —
    ///   buildable, or submittable — turns on that one fact.
    ///
    /// What the lane answers is written to three files and printed as one line:
    /// the attempt with its §29 lifecycle record, the signed envelope (§52) when
    /// bytes were made, and what the endpoint said (§53) when it was asked.
    ///
    /// The answer also carries the one thing M8.1 needs and cannot invent: the id of the
    /// ladder record this attempt made, if it made one. `None` is a fact about the
    /// attempt — no record, so no execution-lifecycle stamps to read — and the caller
    /// records the execution half as the skips it was rather than as spans this build
    /// never measured (§15).
    async fn on_execution(
        &mut self,
        outcome: &SimOutcome,
        decision: &evm_risk::RiskDecision,
        run: Option<&evm_simulation::SimulationResult>,
    ) -> Result<Option<String>> {
        let Some(stage) = self.execution.as_mut() else {
            return Ok(None);
        };
        if !decision.accepted() {
            self.engine
                .metrics_mut()
                .bump("execution_skipped_not_accepted");
            return Ok(None);
        }
        let Some(run) = run else {
            // The refused path wrote its own line and returned before the risk
            // layer was reached, so there is no run here to hand over.
            unreachable!("a refused simulation never reaches the execution lane");
        };
        let opportunity_id = outcome.id.to_string();
        let state_fingerprint = outcome.state_version.to_string();
        let funding = match &outcome.sender_override {
            Some(reason) => SenderFunding::Overridden {
                detail: reason.clone(),
            },
            None => SenderFunding::RealState {
                source: outcome.state_source.clone(),
            },
        };
        // §31's freshness leg is stated rather than assumed: this method returned
        // early above the moment the engine stopped tracking this finding, so the
        // lifecycle's own answer at this point is `active`.
        let provenance = AttemptProvenance {
            opportunity_id: &opportunity_id,
            state_fingerprint: &state_fingerprint,
            funding,
            freshness: Freshness::Active,
        };
        let report = stage
            .on_risk_decision(run, decision, provenance, self.engine.metrics_mut())
            .await;
        self.executions += 1;
        if report.sent {
            self.sent += 1;
        }
        self.evidence
            .line(EvidenceFile::Executions, &report.to_json())?;
        if let Some(signed) = &report.signed {
            self.evidence.line(
                EvidenceFile::SignedTransactions,
                &serde_json::to_value(signed).expect("the signed envelope serializes"),
            )?;
        }
        if let Some(submission) = &report.submission {
            self.evidence.line(
                EvidenceFile::Submissions,
                &serde_json::to_value(submission).expect("the submission row serializes"),
            )?;
        }
        self.say(report.line());
        Ok(report.execution_id)
    }
}

/// §11's audit line, in the order the task lists it.
fn state_change_line(change: &StateChange) -> Value {
    json!({
        "block_number": change.block_number.0,
        "tx_hash": change.tx_hash.to_string(),
        "tx_index": change.tx_index.0,
        "log_index": change.log_index.0,
        "pool": format!("{:#x}", change.pool.address),
        "event": change.source.as_str(),
        "written": change.written.as_str(),
        "before": change.before,
        "after": change.after,
    })
}

/// §41's finding record: the identity, the state version it was priced against,
/// and the two pools' own attested numbers.
fn opportunity_line(opportunity: &Opportunity, version: UpdatePosition) -> Value {
    let id = evm_opportunity::OpportunityId::new(opportunity);
    json!({
        "opportunity_id": id.to_string(),
        "chain_id": opportunity.chain_id.0,
        "observed_block": opportunity.block_number.0,
        "state_version": version,
        "pools": opportunity
            .hops
            .iter()
            .map(|hop| json!({
                "pool": format!("{:#x}", hop.pool.address),
                "reserve_in": hop.reserve_in.to_string(),
                "reserve_out": hop.reserve_out.to_string(),
                "fee": format!("{:?}", hop.fee),
            }))
            .collect::<Vec<_>>(),
        "input_token": format!("{:#x}", opportunity.input_token.address),
        "input_amount": opportunity.input_amount.to_string(),
        "output_amount": opportunity.output_amount.to_string(),
        "gross_profit": opportunity.gross_profit.to_string(),
        "search": {
            "strategy": format!("{:?}", opportunity.search.strategy),
            "rounds": opportunity.search.rounds,
            "evaluations": opportunity.search.evaluations,
            "interval_closed": opportunity.search.interval_closed,
            "upper_bound": opportunity.search.upper_bound.to_string(),
        },
        "direction": id.direction,
    })
}

/// What a worker came back with, beside the question it was asked.
fn simulation_line(outcome: &SimOutcome) -> Value {
    let (status, gas_used, net_profit, block, state_source, fingerprint) = match &outcome.run {
        SimRun::Executed(run) => (
            format!("{:?}", run.status),
            run.gas_used(),
            format!("{:?}", run.net_profit),
            run.block.to_string(),
            run.state_source.clone(),
            run.fingerprint().to_string(),
        ),
        SimRun::Refused(_) => (
            "not executed".to_string(),
            0,
            "no run".to_string(),
            outcome.observed_block.0.to_string(),
            outcome.state_source.clone(),
            String::new(),
        ),
    };
    json!({
        "opportunity_id": outcome.id.to_string(),
        "observed_block": outcome.observed_block.0,
        "state_version": outcome.state_version,
        "worker": outcome.worker,
        "queue_wait_ms": outcome.queue_wait_ms,
        "status": status,
        "gas_used": gas_used,
        "net_profit": net_profit,
        "analytical_output": outcome.analytical_output.to_string(),
        "gross_profit": outcome.gross_profit.to_string(),
        "pin": block,
        "state_source": state_source,
        "fingerprint": fingerprint,
        "deltas": outcome
            .timing
            .deltas()
            .iter()
            .map(|(name, ms)| json!({"hop": name, "ms": ms}))
            .collect::<Vec<_>>(),
    })
}

/// Where the canonical blocks come from, and what reads their state.
///
/// Returns the adapter every block and state read goes through, the chain id the
/// endpoint itself reported, the source that will announce blocks, the number to
/// move forward from (§7), and what M12-B §3's readiness gate heard.
async fn build_canonical(
    config: &PipelineConfig,
) -> Result<(
    Arc<dyn ChainAdapter>,
    evm_core::ChainId,
    Box<dyn MarketDataSource>,
    BlockNumber,
    ReadinessFacts,
)> {
    match &config.canonical_source {
        CanonicalSource::WebSocket => {
            let ws_url = config.ws_url.as_deref().ok_or_else(|| {
                PipelineError::Config(
                    "the canonical source is a WebSocket but no --ws-url was given".to_string(),
                )
            })?;
            // The node every block, header and state read of this run goes through.
            // Its chain id is its own answer to `eth_chainId`, not a configured one
            // (§45).
            let rpc_url = config.rpc_url.as_deref().ok_or_else(|| {
                PipelineError::Config(
                    "the canonical source needs a node to read blocks and state from, \
                     but no --rpc-url was given"
                        .to_string(),
                )
            })?;
            let http = HttpChainAdapter::connect(rpc_url)
                .await
                .map_err(PipelineError::Chain)?;
            let chain_id = http.chain_id();
            // §7's head comes from this adapter rather than from the socket: the
            // subscription is opened inside `run`, so asking it for a number now
            // would be a second path to a fact the HTTP read already owns.
            // The head is also the only block number the freshness check has, so a
            // run that pinned its start has not read one and §3's fail-closed rule
            // applies to it.
            let (start_after, observed_head) = match config.start_block {
                Some(number) => (BlockNumber(number), None),
                None => {
                    let head = http.latest_block().await.map_err(PipelineError::Chain)?;
                    (head, Some(head.0))
                }
            };
            let readiness =
                gate_readiness(&config.readiness, &http, rpc_url, observed_head).await?;
            let source = WebSocketSource::connect(ws_url, config.source)
                .await
                .map_err(PipelineError::Live)?;
            Ok((
                Arc::new(http),
                chain_id,
                Box::new(source),
                start_after,
                readiness,
            ))
        }
        CanonicalSource::HttpPoll => {
            let rpc_url = config.rpc_url.as_deref().ok_or_else(|| {
                PipelineError::Config(
                    "the canonical source polls a node, but no --rpc-url was given".to_string(),
                )
            })?;
            let http = HttpChainAdapter::connect(rpc_url)
                .await
                .map_err(PipelineError::Chain)?;
            let chain_id = http.chain_id();
            // One client, two handles: the same connection pool answers the head
            // reads here and the block and state reads inside the engine (§62).
            let source =
                PollingSource::new(http.clone(), chain_id, SourceKind::HttpPoll, config.source);
            let mut source = Box::new(source);
            let (start_after, observed_head) = match config.start_block {
                Some(number) => (BlockNumber(number), None),
                None => {
                    let head = source.head().await.map_err(PipelineError::Live)?;
                    (head, Some(head.0))
                }
            };
            let readiness =
                gate_readiness(&config.readiness, &http, rpc_url, observed_head).await?;
            Ok((Arc::new(http), chain_id, source, start_after, readiness))
        }
        CanonicalSource::Replay { directory } => {
            // No endpoint to ask, so the directory's own blocks are the authority
            // on which chain they are — and `load` refuses a block that disagrees.
            let chain_id =
                RecordedChainAdapter::chain_id_of(directory).map_err(PipelineError::Chain)?;
            let adapter =
                RecordedChainAdapter::load(directory, chain_id).map_err(PipelineError::Chain)?;
            let first = adapter.first_block().ok_or_else(|| {
                PipelineError::Config(format!("{} holds no blocks to replay", directory.display()))
            })?;
            let reader =
                RecordedChainAdapter::load(directory, chain_id).map_err(PipelineError::Chain)?;
            let source = PollingSource::new(reader, chain_id, SourceKind::Replay, config.source);
            let start_after = match config.start_block {
                // §7's head-then-forward rule describes a live endpoint. A recorded
                // corpus starts at its own first block, which is what makes §32's
                // parity comparison able to say "the same blocks".
                Some(number) => BlockNumber(number),
                None => BlockNumber(first.0.saturating_sub(1)),
            };
            // The gate is not run, and that is reported as an absence rather than as a
            // verdict: a recording cannot be syncing, so there was nothing to check and
            // no `false` to pretend it answered (§3's last rule about unverified state).
            Ok((
                Arc::new(adapter),
                chain_id,
                Box::new(source),
                start_after,
                ReadinessFacts::not_asked(),
            ))
        }
    }
}

/// What the readiness gate asked and what it heard, for the session record.
///
/// §3 asks for the actual call count, and this is the only honest place to get it: the
/// gate's own tally, not an inference from how long the run took. `verdict: None` is the
/// replay's answer — no node was asked, so there is no verdict, and writing `false` or
/// `0 ms` there would be the "nothing measured reported as a measurement" error §9 of
/// M8.2's rules already forbids elsewhere.
#[derive(Clone, Debug)]
pub struct ReadinessFacts {
    verdict: Option<Readiness>,
    asks: usize,
}

impl ReadinessFacts {
    /// A run that has no node to ask.
    const fn not_asked() -> Self {
        Self {
            verdict: None,
            asks: 0,
        }
    }

    /// Why the gate held the run, or `None` when it did not hold it.
    fn withheld(&self) -> Option<String> {
        self.verdict.as_ref().and_then(Readiness::withheld_because)
    }

    /// The `status.jsonl` line: the node's answer, the policy it was judged under, and
    /// what it cost. Kept to the three facts §3 names, because the line is written from
    /// values this run already holds and asks the node for nothing (§3's no-extra-RPC
    /// rule for evidence).
    fn describe(&self, policy: &HeadFreshnessPolicy) -> Value {
        json!({
            "readiness": {
                "verdict": self.verdict,
                "eth_syncing_asks": self.asks,
                "head_freshness_policy": policy,
                "detail": match &self.verdict {
                    None => "this run contacts no node, so readiness was never asked",
                    Some(Readiness::Ready) => "`false` means the node reports no sync in \
                                               progress; it is not a claim that the node is \
                                               at the network head",
                    Some(_) => "the run was refused before a block was read",
                },
            }
        })
    }
}

/// M12-B §3's readiness gate: ask the node whether it is ready, and refuse the run if
/// it is not.
///
/// M12-D §3 put it on the paths that can spend as well as on the one that only watches, so
/// this is now called from three places and one definition: the two live branches of the
/// bootstrap (`build_canonical`'s WebSocket and HTTP-poll arms, ahead of the registry read,
/// the execution lane's connect, the evidence writer and the first block), the route entry
/// ([`crate::arbitrage::run_once`], ahead of its first block read and therefore ahead of the
/// sequence lane that constructs a `Signer`), and the single-transaction entry
/// (`run_validation` in `evm-cli`, ahead of `ExecutionStage::connect` for the same reason).
/// What makes §3's 「禁止进入会产生真实执行动作的阶段」 true by construction is that
/// ordering, in each of the three: a held run has no session, no event loop, and no
/// execution lane.
///
/// The two spending entries pass [`HeadFreshnessPolicy::NotJudged`] and no observed head,
/// because neither has a reference height to be judged against — they read their own head
/// after the gate, and a gate that consumed it would no longer be ahead of anything. That
/// is the policy that still refuses a node reporting sync in progress and a node that gives
/// no usable answer, and it is not a claim of freshness; §4's `HeadBehindReference` needs a
/// reference and this call does not have one.
///
/// The recheck budget lives in [`ReadinessGate`]. This repository has one defined
/// recovery point per process: a source that fails mid-run ends the session
/// (`canonical_source_failed_during_run`) rather than restarting inside it, so the
/// recheck is the next `run`, and nothing asks `eth_syncing` per market event. Each of the
/// three call sites is a process-level start, so each asks once; no loop in this repository
/// calls any of them.
pub async fn gate_readiness(
    policy: &HeadFreshnessPolicy,
    http: &HttpChainAdapter,
    endpoint: &str,
    observed_head: Option<u64>,
) -> Result<ReadinessFacts> {
    let mut gate = ReadinessGate::new(*policy);
    let verdict = gate.check(http, observed_head).await;
    let facts = ReadinessFacts {
        verdict: Some(verdict),
        asks: gate.checks(),
    };
    match facts.withheld() {
        // §3's failure must not degrade into "a session with no opportunities": this is
        // an error out of `run`, and the words are the node's own answer rather than a
        // guess about why it answered that way.
        Some(detail) => Err(PipelineError::NodeNotReady {
            endpoint: endpoint.to_string(),
            detail,
        }),
        None => Ok(facts),
    }
}

/// M12-B §4's labels in the one sentence an evidence reader needs with them: that a
/// purpose here is a *declaration*, and what the run did not do.
///
/// Written as a sentence rather than left to the enum because the distinction §4 exists
/// to protect is between three things an auditor can easily collapse: that a label says
/// the operator told us so, that a label says nobody told us anything, and that a label
/// was measured off the node. Only the first is ever true of this field, and `unknown`
/// is the second, never a quiet third.
fn endpoint_purpose_detail(canonical: EndpointPurpose, flashblocks: EndpointPurpose) -> String {
    format!(
        "canonical: {}; flashblocks: {} — every label here is what the operator declared \
         with `--rpc-endpoint-purpose` or `--flashblocks-endpoint-purpose`. `not declared` \
         is an absence, not a kind of endpoint: nothing in this run inferred a purpose from \
         a host, a port or a URL string, and nothing verified one against the node it \
         spoke to.",
        purpose_words(canonical),
        purpose_words(flashblocks),
    )
}

/// A label, or the words that say there is none.
fn purpose_words(purpose: EndpointPurpose) -> &'static str {
    match purpose {
        EndpointPurpose::Unknown => "not declared",
        declared => declared.label(),
    }
}

/// The job a source task does: run to the stop flag, then report.
async fn run_source(
    mut source: Box<dyn MarketDataSource>,
    start_after: BlockNumber,
    sink: mpsc::Sender<MarketEvent>,
    stop: Arc<AtomicBool>,
) -> SourceCompletion {
    let label = source.kind().as_str();
    let report = source.run(start_after, sink, stop).await;
    let capability = source.capability();
    // A run that ended in an error still keeps its counters, and they are what
    // §49's flush needs: the table a source reports is the evidence for §8's
    // unrecovered gap, which by definition is a run that ended in an error.
    let run = report.as_ref().ok().cloned().or_else(|| source.report());
    SourceCompletion {
        source: label,
        capability,
        report: run.as_ref().and_then(|run| serde_json::to_value(run).ok()),
        error: report.err().map(|error| error.to_string()),
    }
}

/// Wait for the Ctrl-C or SIGTERM that starts §49's flush.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => "sigint",
                    _ = terminate.recv() => "sigterm",
                }
            }
            // A process that cannot install the handler still stops on Ctrl-C:
            // what matters for §49 is the flush, not which signal earned it.
            Err(_) => {
                tokio::signal::ctrl_c().await.ok();
                "sigint"
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        "sigint"
    }
}

/// Load every configured registry directory and merge them into one set of
/// attestations. `merge` refuses a pool two files attest differently, so a
/// directory cannot quietly overrule another's evidence.
fn load_registries(dirs: &[PathBuf]) -> Result<Registry> {
    let mut merged = Registry::default();
    for dir in dirs {
        let loaded = Registry::load_dir(dir)
            .map_err(|error| PipelineError::Registry(dir.clone(), error.to_string()))?;
        merged
            .merge(loaded)
            .map_err(|error| PipelineError::Registry(dir.clone(), error.to_string()))?;
    }
    Ok(merged)
}

/// §46's registry half, asked for on its own: the chain ids the attested pools name,
/// deduped and sorted. A run that uses the lane without moving market data — §35's
/// validation transaction — still has to answer the same question, and re-deriving it
/// somewhere else is how two answers to one rule start disagreeing.
///
/// More than one id is not an error here: it is the list, and the caller decides that a
/// run on one chain cannot mean two of them.
pub fn attested_chain_ids(dirs: &[PathBuf]) -> Result<Vec<u64>> {
    let registry = load_registries(dirs)?;
    let mut ids: Vec<u64> = registry.pools.keys().map(|pool| pool.chain_id.0).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Run one session to completion and return its record.
pub async fn run(config: &PipelineConfig) -> Result<SessionReport> {
    let clock = Clock::new();
    let started_unix_ms = now_unix_ms();
    if config.state_dump.is_some()
        && !matches!(config.canonical_source, CanonicalSource::Replay { .. })
    {
        return Err(PipelineError::Config(
            "a recorded state describes exactly one block, so it can only serve a run whose \
             findings are all priced at that block — which is a replay. A live run reads its \
             state from the node it reads its blocks from (§62), and this combination would \
             decline every finding it made"
                .to_string(),
        ));
    }
    let (chain, chain_id, canonical, start_after, readiness) = build_canonical(config).await?;

    // §46: the registry attests pools for a chain, the endpoint is on a chain, and
    // nothing downstream can notice the two disagreeing.
    let registry = load_registries(&config.registry_dirs)?;
    let registry_chains: Vec<u64> = {
        let mut ids: Vec<u64> = registry.pools.keys().map(|pool| pool.chain_id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    if registry_chains.len() != 1 || registry_chains[0] != chain_id.0 {
        // Named by whatever answered `eth_chainId`: a node URL for a live source,
        // the directory for a replay, because those are the two things that can
        // disagree with the registry (§46).
        return Err(PipelineError::ChainMismatch {
            registry: registry_chains.first().copied().unwrap_or_default(),
            node: chain_id.0,
            endpoint: match (&config.rpc_url, &config.canonical_source) {
                (Some(url), _) => url.clone(),
                (None, CanonicalSource::Replay { directory }) => directory.display().to_string(),
                (None, _) => config.canonical_source.label().to_string(),
            },
        });
    }

    // Where findings get their state. A live run has one answer — the node it is
    // reading blocks from — and only a run pointed at a recording has the other.
    let mut engine = match &config.state_dump {
        None => MarketEngine::new(chain, &registry, config, clock),
        Some(file) => {
            let dump = StateDump::from_file(file).map_err(|error| PipelineError::StateDump {
                path: file.clone(),
                detail: error.to_string(),
            })?;
            // §46's boundary, applied to a recording: a dump of another chain's
            // block is not this chain's state, however well it reads.
            if dump.chain_id != chain_id.0 {
                return Err(PipelineError::StateDump {
                    path: file.clone(),
                    detail: format!(
                        "it is a recording of chain {} while this run is on chain {}",
                        dump.chain_id, chain_id.0
                    ),
                });
            }
            MarketEngine::new(chain, &registry, config, clock)
                .with_recorded_state(dump, file.clone())
        }
    };
    let session_id = format!(
        "{}-{}-{}",
        config.canonical_source.label(),
        chain_id.0,
        started_unix_ms
    );
    // M6's lane, connected before the session starts and for the whole of it.
    //
    // A lane that was asked for and cannot connect ends the run. This is the one
    // place the pipeline could quietly degrade instead — fall back to M5's shape
    // and report a normal session — and it is the case §58 exists to prevent: the
    // numbers would be identical to a run that never asked to execute, so the
    // operator would learn about the missing lane from an evidence file after
    // paying for a session that could not produce one.
    let execution = match &config.execution {
        None => None,
        Some(setup) => {
            let url = config.rpc_url.as_deref().ok_or_else(|| {
                PipelineError::Config(format!(
                    "the execution lane ({}) reads a fee, a nonce and a canonical block from \
                     an endpoint, and this run has none — a replay supplies recorded blocks, \
                     which is not a node to send through",
                    setup.mode.name()
                ))
            })?;
            // M12-F §7: the ledger is read back before the lane exists. The stage builds a
            // `Signer` in its constructor and allocates a nonce at its first send, so a record
            // a dead process left in flight has to be on that lane before either happens —
            // otherwise this run allocates the nonce the previous one already spent. An unreadable
            // or contradictory file is an error out of `run`, not a warning: §7 forbids clearing
            // the ledger and trading on, and §11 forbids degrading to a memory-only lane, which
            // is the silent fork a missing file would otherwise start.
            let journal = ExecutionJournal::open(&config.ledger_dir, chain_id.0, journal_stamp())
                .map_err(|error| PipelineError::Execution(error.to_string()))?;
            Some(
                ExecutionStage::connect(url, chain_id.0, setup.clone(), clock, journal)
                    .await
                    .map_err(|error| PipelineError::Execution(error.to_string()))?,
            )
        }
    };
    // One directory per session: a rerun into the same `--out-dir` lands beside
    // the previous run instead of appending to its lines and overwriting its
    // summary files (§48 asks for a session record, and two sessions in one file
    // is neither).
    let mut evidence = EvidenceWriter::open(
        &config.evidence_dir.join(&session_id),
        &session_id,
        config.execution.is_some(),
    )?;
    // §3's 「记录实际调用次数」: the gate's own tally goes into the session record and
    // the metrics of the run that made it, from values already in hand. Nothing here
    // asks the node again — an evidence write that cost an RPC would be exactly the
    // per-event call §3 forbids.
    evidence.line(EvidenceFile::Status, &readiness.describe(&config.readiness))?;
    engine
        .metrics_mut()
        .add("readiness.eth_syncing_asks", readiness.asks as u64);
    // M8.1's traces get their own directory tree for the same §48 reason and the same
    // one-directory-per-session rule, and beside rather than inside: a baseline file
    // never joins the files a run's decisions were read from, so adding telemetry
    // cannot be blamed for what a market record says (§2.1, §44).
    let latency = match &config.latency_dir {
        None => None,
        Some(dir) => {
            let mode = config
                .execution
                .as_ref()
                .map_or("no-lane", |setup| setup.mode.name());
            Some(LatencyEvidence::open(
                &dir.join(&session_id),
                git_revision(),
                mode,
            )?)
        }
    };
    let mut session = Session::new(config, engine, evidence, execution, latency);

    let (sink, mut events) = mpsc::channel::<MarketEvent>(config.queues.event_capacity.max(1));
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles: Vec<(&'static str, JoinHandle<SourceCompletion>)> = Vec::new();
    handles.push((
        canonical.kind().as_str(),
        tokio::spawn(run_source(
            canonical,
            start_after,
            sink.clone(),
            Arc::clone(&stop),
        )),
    ));

    // §27: the candidate watcher rides on the same event stream and gets the
    // canonical announcements forwarded to it, so its mapping table is measured
    // in production rather than only in a test.
    let mut forward: Option<mpsc::Sender<BlockAnnouncement>> = None;
    if let Some(endpoint) = &config.flashblocks_url {
        let reader = HttpChainAdapter::connect(endpoint)
            .await
            .map_err(PipelineError::Chain)?;
        if reader.chain_id() != chain_id {
            // Same roles as the registry gate above: `registry` is the chain this run
            // has already attested, `node` is what *this* endpoint answered to
            // `eth_chainId`. M12-A §16 D1 recorded the two being printed the other way
            // round; the comparison itself was always correct and still aborts here.
            return Err(PipelineError::ChainMismatch {
                registry: chain_id.0,
                node: reader.chain_id().0,
                endpoint: endpoint.clone(),
            });
        }
        let mut source =
            FlashblockSource::new(reader, chain_id, endpoint.clone(), config.flashblocks);
        let (hint_sink, mut hints) =
            mpsc::channel::<BlockAnnouncement>(config.queues.event_capacity.max(1));
        forward = Some(hint_sink);
        let sink = sink.clone();
        let stop = Arc::clone(&stop);
        handles.push((
            SourceKind::Flashblock.as_str(),
            tokio::spawn(async move {
                let report = source.run_reconciling(sink, stop, &mut hints).await;
                let capability = source.capability();
                SourceCompletion {
                    source: SourceKind::Flashblock.as_str(),
                    capability,
                    report: report
                        .as_ref()
                        .ok()
                        .and_then(|run| serde_json::to_value(run).ok()),
                    error: report.err().map(|error| error.to_string()),
                }
            }),
        ));
    }
    // The runner's own handle goes: when every source has stopped, `events.recv()`
    // answers `None` and the loop ends by itself (§50's close, not a timeout).
    drop(sink);

    let mut pool = SimulationPool::spawn(
        config.queues.simulation_capacity,
        config.queues.outcome_capacity,
        config.queues.simulation_workers,
        clock,
    );
    let sim = pool.sender.clone();

    let deadline = tokio::time::sleep(config.duration);
    tokio::pin!(deadline);
    // Every `break` below stamps its own reason, so the variable is declared
    // empty rather than seeded with a value that could never be read.
    let ended_by: &'static str;
    loop {
        if let Some(max) = config.max_blocks {
            if session.blocks >= max {
                ended_by = "max blocks reached";
                break;
            }
        }
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(event) => session.on_event(event, &sim, &forward).await?,
                    None => {
                        ended_by = "every source ended";
                        break;
                    }
                }
            }
            outcome = pool.outcomes.recv() => {
                match outcome {
                    Some(outcome) => session.on_outcome(outcome).await?,
                    None => { ended_by = "simulation outcome stream closed"; break }
                }
            }
            _ = &mut deadline => { ended_by = "duration elapsed"; break }
            why = shutdown_signal() => { ended_by = why; break }
        }
        if session.fatal.is_some() {
            ended_by = "unrecovered gap (§8)";
            break;
        }
    }

    // §49, for whichever reason ended the loop: a job already handed to a worker
    // is not allowed to vanish with the run. The wait is bounded — a wedged worker
    // must not outlive the session that stopped asking — and a wait that expires
    // leaves a counter behind rather than a silently unanswered finding (§51).
    let drain_budget = Duration::from_millis(2_000 + 1_000 * session.jobs_in_flight.max(1) as u64);
    let draining = async {
        while session.jobs_in_flight > 0 {
            match pool.outcomes.recv().await {
                Some(outcome) => session.on_outcome(outcome).await?,
                None => break,
            }
        }
        Ok::<(), PipelineError>(())
    };
    if tokio::time::timeout(drain_budget, draining).await.is_err() {
        session
            .engine
            .metrics_mut()
            .bump("simulation_still_in_flight_at_end");
    }

    // §49's order: ask everything to stop, close the queues that let a stage leave
    // its `recv`, then join so nothing's counters are lost.
    stop.store(true, Ordering::Relaxed);
    drop(sim);
    drop(forward);
    let shutdown_budget = Duration::from_millis(config.source.poll_interval_ms * 3 + 5_000);
    // The queue is bounded, and a source that read ahead of the flag is parked in
    // `send` with blocks it produced before it looks again: with nobody receiving,
    // it never gets back to the check that would end it, and §49's join step waits
    // on a task that cannot finish. So the queue is emptied while the tasks are
    // joined, and what was left in it is counted — the run ended before these
    // events, which is stated, not silently dropped (§51).
    let mut left_in_queue = 0u64;
    let mut queue_open = true;
    let mut sources = Vec::new();
    for (label, handle) in handles {
        let joined = tokio::time::timeout(shutdown_budget, handle);
        tokio::pin!(joined);
        let completion = loop {
            // Not `biased`: the budget's future has to be polled on every pass, or a
            // source that ignored the flag and kept producing would spin past the
            // deadline this loop exists to enforce.
            tokio::select! {
                got = events.recv(), if queue_open => match got {
                    Some(_) => left_in_queue += 1,
                    None => queue_open = false,
                },
                outcome = &mut joined => break outcome,
            }
        };
        let completion = match completion {
            Ok(Ok(completion)) => completion,
            Ok(Err(_panic)) => SourceCompletion {
                source: label,
                capability: json!({"probed": false, "detail": "the source task panicked"}),
                report: None,
                error: Some("task panic".to_string()),
            },
            Err(_elapsed) => SourceCompletion {
                source: label,
                capability: json!({"probed": false, "detail": "the task never stopped"}),
                report: None,
                error: Some(format!(
                    "source did not end within {} ms of the stop request",
                    shutdown_budget.as_millis()
                )),
            },
        };
        sources.push(completion);
    }
    while events.try_recv().is_ok() {
        left_in_queue += 1;
    }
    if left_in_queue > 0 {
        session
            .engine
            .metrics_mut()
            .add("market_events_left_at_shutdown", left_in_queue);
    }
    let workers = pool.shutdown().await;

    let report = session.finish(
        &session_id,
        start_after,
        ended_by,
        started_unix_ms,
        &sources,
        &workers,
    )?;

    if let Some(error) = report.fatal {
        return Err(error);
    }
    Ok(report.session)
}

/// What [`run`] writes at the end, split out so the evidence is flushed whether
/// the run finished or is about to return an error.
impl Session<'_> {
    fn finish(
        &mut self,
        session_id: &str,
        start_after: BlockNumber,
        ended_by: &str,
        started_unix_ms: u64,
        sources: &[SourceCompletion],
        workers: &[WorkerReport],
    ) -> std::result::Result<Finish, PipelineError> {
        for completion in sources {
            self.evidence.line(
                EvidenceFile::Status,
                &json!({
                    "source_run_ended": {
                        "source": completion.source,
                        "report": completion.report,
                        "error": completion.error,
                    },
                }),
            )?;
        }
        for report in workers {
            self.evidence
                .line(EvidenceFile::Status, &json!({"simulation_worker": report}))?;
        }
        let metrics = self.engine.metrics().to_json();
        self.evidence.write_whole("metrics.json", &metrics)?;

        let capability: Value = serde_json::Value::Object(
            sources
                .iter()
                .map(|completion| (completion.source.to_string(), completion.capability.clone()))
                .collect(),
        );
        // M8.1's tables and README, written last so `summary.json` covers every lifecycle
        // this session recorded — including the ones that ended at a refusal. An error here
        // ends the run: a baseline that stops mid-table is not evidence of a baseline, and
        // §46's files are never quietly half-written.
        let traces = match self.latency.as_mut() {
            None => None,
            Some(evidence) => Some((
                evidence.finish()?,
                u64::try_from(evidence.traces_recorded()).unwrap_or(u64::MAX),
            )),
        };
        let counters = self.engine.metrics();
        let record = json!({
            "session_id": session_id,
            "chain_id": self.engine.chain_id().0,
            "source": self.config.canonical_source.label(),
            "start_block": start_after.0,
            "end_block": self.last_block.map(|number| number.0),
            "started_at_unix_ms": started_unix_ms,
            "ended_at_unix_ms": now_unix_ms(),
            "ended_by": ended_by,
            "blocks": self.blocks,
            "state_changes": counters.get("syncs_applied") + counters.get("registrations"),
            "opportunities": counters.get("opportunities_found"),
            "simulations": self.simulations,
            "risk_decisions": {
                "accept": self.accepts,
                "reject": self.rejects,
                "unknown": self.unknowns,
            },
            "events": self.events,
            "findings_settled": self.engine.settled_count(),
            "ledger": self.engine.ledger_stats(),
            "registry_dirs": self
                .config
                .registry_dirs
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>(),
            "pools": {
                "registry": self.engine.registry_pools(),
                "known": self.engine.pool_count(),
                "synced": self.engine.synced_pool_count(),
            },
            "endpoints": {
                // M12-D §5: the URL itself is gone from this object and the digests below
                // are what remains. A configured endpoint is the one string a run can carry
                // into committed evidence that is partly a credential — an API key or a JWT
                // lives in the path or query of the URL an operator pastes — and the digest
                // answers everything the record needs to ask: is this the same socket the
                // RPC trace names, and are two roles served by one provider. This is the
                // rule `crates/chain/src/rpc_trace.rs` already applies to its own lines.
                // Sessions written before this change keep what they wrote; the rule is
                // forward-only.
                // M12-B §4: what the operator declared each endpoint to be, and never
                // derived from an address. The two purposes are per *role*, not per URL: a
                // run can reach its canonical state over HTTP and its heads over WebSocket,
                // and the declaration says who serves that role while the digests say
                // whether the two are one provider. `unknown` is a recorded absence, not a
                // guess — a public node is never here as a local one, and a silent
                // configuration is never here as public either.
                "rpc_purpose": self.config.canonical_purpose,
                "flashblocks_purpose": self.config.flashblocks_purpose,
                // Identity and digest in separate keys (§4.2): the purpose is what the
                // operator declared, the digest is what the RPC trace lines call the
                // endpoint, and one rule computes the digest here and there so the two
                // cannot disagree about whether two lines name one provider.
                "rpc_endpoint_id": self.config.rpc_url.as_deref().map(evm_chain::endpoint_id),
                "ws_endpoint_id": self.config.ws_url.as_deref().map(evm_chain::endpoint_id),
                "flashblocks_endpoint_id": self
                    .config
                    .flashblocks_url
                    .as_deref()
                    .map(evm_chain::endpoint_id),
                "purpose_detail": endpoint_purpose_detail(
                    self.config.canonical_purpose,
                    self.config.flashblocks_purpose,
                ),
            },
            // §63: a run that read its state from a recording says so, in the
            // same file that says which blocks it replayed.
            "state_source": self.engine.state().describe(),
            "wrapped_native": self.config.wrapped_native.map(|address| format!("{address:#x}")),
            "risk_thresholds": {
                "minimum_net_profit_wei": self.config.risk.minimum_net_profit_wei.to_string(),
                "maximum_gas": self.config.risk.maximum_gas,
                "detail": "an unset maximum_gas is answered per block by that block's own gas limit",
            },
            "queues": self.config.queues.describe(),
            "capability": capability,
            "sources": sources,
            "simulation_workers": workers,
            "execution": self.execution_summary(),
            // Both fields are absent-or-a-number rather than zero-or-nothing: a reader
            // has to be able to tell "this run measured no latency" from "it measured
            // and no finding ever reached the risk layer".
            "latency_traces": traces.as_ref().map(|(dir, count)| json!({
                "directory": dir.display().to_string(),
                "traces": count,
            })),
            "milestone": "M6",
        });
        let session = session_record(&record);
        self.evidence.write_whole("live-session.json", &session)?;
        Ok(Finish {
            fatal: self.fatal.take(),
            session: SessionReport {
                session_id: session_id.to_string(),
                ended_by: ended_by.to_string(),
                blocks: self.blocks,
                events: self.events,
                simulations: self.simulations,
                accepts: self.accepts,
                rejects: self.rejects,
                unknowns: self.unknowns,
                executions: self.executions,
                sent: self.sent,
                evidence_dir: self.evidence.dir().to_path_buf(),
                metrics,
                session,
                sources: sources.to_vec(),
                workers: workers.to_vec(),
            },
        })
    }
}

/// [`Session::finish`]'s pair: the record that was written, and the reason the
/// run must still be reported as failed.
struct Finish {
    fatal: Option<PipelineError>,
    session: SessionReport,
}
