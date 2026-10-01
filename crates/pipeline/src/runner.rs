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

use evm_chain::{ChainAdapter, HttpChainAdapter, RecordedChainAdapter};
use evm_core::BlockNumber;
use evm_live::{
    now_unix_ms, BlockAnnouncement, FlashblockSource, MarketDataSource, MarketEvent, PollingSource,
    SourceKind, SourceStatus, WebSocketSource,
};
use evm_metrics::Clock;
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
    /// §8: the reason a run must be reported as failed rather than as finished.
    fatal: Option<PipelineError>,
}

impl<'a> Session<'a> {
    fn new(config: &'a PipelineConfig, engine: MarketEngine, evidence: EvidenceWriter) -> Self {
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
        Ok(())
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
/// endpoint itself reported, the source that will announce blocks, and the number
/// to move forward from (§7).
async fn build_canonical(
    config: &PipelineConfig,
) -> Result<(
    Arc<dyn ChainAdapter>,
    evm_core::ChainId,
    Box<dyn MarketDataSource>,
    BlockNumber,
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
            let start_after = match config.start_block {
                Some(number) => BlockNumber(number),
                None => http.latest_block().await.map_err(PipelineError::Chain)?,
            };
            let source = WebSocketSource::connect(ws_url, config.source)
                .await
                .map_err(PipelineError::Live)?;
            Ok((Arc::new(http), chain_id, Box::new(source), start_after))
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
            let start_after = match config.start_block {
                Some(number) => BlockNumber(number),
                None => source.head().await.map_err(PipelineError::Live)?,
            };
            Ok((Arc::new(http), chain_id, source, start_after))
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
            Ok((Arc::new(adapter), chain_id, Box::new(source), start_after))
        }
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
    let (chain, chain_id, canonical, start_after) = build_canonical(config).await?;

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
    let engine = match &config.state_dump {
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
    // One directory per session: a rerun into the same `--out-dir` lands beside
    // the previous run instead of appending to its lines and overwriting its
    // summary files (§48 asks for a session record, and two sessions in one file
    // is neither).
    let evidence = EvidenceWriter::open(&config.evidence_dir.join(&session_id), &session_id)?;
    let mut session = Session::new(config, engine, evidence);

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
            return Err(PipelineError::ChainMismatch {
                registry: reader.chain_id().0,
                node: chain_id.0,
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
                "rpc_url": self.config.rpc_url,
                "ws_url": self.config.ws_url,
                "flashblocks_url": self.config.flashblocks_url,
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
            "no_execution": evm_risk::NO_BROADCAST,
            "milestone_note": "M5 stops at a risk decision: nothing here broadcasts, signs, \
                              deploys, or bids gas (§26).",
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
