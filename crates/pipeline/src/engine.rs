//! The market path: sealed block → state → graph → finding, in one place.
//!
//! §9's requirement is that Live and Replay share this code rather than two
//! implementations that happen to agree. It is satisfied structurally: the state
//! work below is [`evm_replay::ReplayEngine`] — the same type M1's replay tests
//! drive — behind the same [`evm_chain::ChainAdapter`] a recorded directory
//! implements. There is no live branch here to diverge from a replay branch.
//!
//! §15's trigger is the other half of this file. The scan does not run per log,
//! and not unconditionally per block: it runs when the block restated a pool,
//! because a finding that touches no changed pool is a finding the previous
//! block already had.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use evm_chain::ChainAdapter;
use evm_core::ChainId;
use evm_graph::GraphBuild;
use evm_live::BlockAnnouncement;
use evm_metrics::{Clock, Metrics, PipelineTiming};
use evm_opportunity::{
    detect_opportunities, Detection, LedgerPolicy, OpportunityId, OpportunityLedger, Registered,
    TrackedOpportunity,
};
use evm_protocol::Registry;
use evm_replay::{ReplayEngine, ReplayReport, Written};
use evm_simulation::StateDump;
use evm_state::{InMemoryStateStore, UpdatePosition};

use crate::config::PipelineConfig;
use crate::error::{PipelineError, Result};
use crate::sim::StateSource;

/// What one sealed block produced, in the order the stages ran.
///
/// The runner turns this into evidence lines and simulation jobs; the engine
/// produces it and holds no channel, so the whole path is testable without a
/// source and without a node.
#[derive(Clone, Debug)]
pub struct BlockOutcome {
    pub announcement: BlockAnnouncement,
    pub timing: PipelineTiming,
    /// The replay report for this block alone, not the run's running total.
    pub report: ReplayReport,
    /// Pools this block restated, ascending by address — deterministic, because
    /// the invalidations below are derived from this list (§16).
    pub changed_pools: Vec<ChainPool>,
    /// Findings this block invalidated by restating one of their pools.
    pub invalidated: Vec<OpportunityId>,
    pub graph: Option<GraphBuild>,
    pub detection: Option<Detection>,
    /// The store position the graph was taken at: the position every finding
    /// from this block is bound to (§16's `state_version`).
    pub state_version: Option<UpdatePosition>,
    /// Findings the simulation stage may be given: still active, and not already
    /// settled by this run.
    pub to_simulate: Vec<TrackedOpportunity>,
    /// Whether §15's trigger let the scan run at all, so a block with no scan
    /// reads as "not triggered" rather than as "nothing was found".
    pub scanned: bool,
}

/// A pool, named the way the ledger names it.
pub type ChainPool = evm_core::PoolId;

/// The engine for one run: state, graph, ledger, metrics.
pub struct MarketEngine {
    chain: Arc<dyn ChainAdapter>,
    /// Where a finding's state is read from, named at the one place the choice is
    /// made (§63).
    state: StateSource,
    replay: ReplayEngine<Arc<dyn ChainAdapter>>,
    graph: evm_graph::MarketGraphBuilder,
    ledger: OpportunityLedger,
    metrics: Metrics,
    clock: Clock,
    /// Ids this run is finished offering: handed to a worker, or declined for a
    /// reason that will not change while the run lasts. A finding that stays
    /// active across several no-change blocks is the same finding, and §21's bound
    /// on work means nothing if it is dispatched again every second — equally, a
    /// decline repeated once per block would be noise wearing evidence's clothes.
    settled: BTreeSet<OpportunityId>,
    registry_pools: usize,
}

impl MarketEngine {
    /// One adapter, shared: the same object answers the block reads on this path
    /// and the state reads inside a simulation, which is how §62's one-provider
    /// rule is held by construction rather than claimed in a report.
    pub fn new(
        chain: Arc<dyn ChainAdapter>,
        registry: &Registry,
        config: &PipelineConfig,
        clock: Clock,
    ) -> Self {
        let adapters = vec![Box::new(evm_protocol::V2Adapter::new(registry.clone()))
            as Box<dyn evm_protocol::ProtocolAdapter>];
        Self {
            replay: ReplayEngine::new(
                chain.clone(),
                adapters,
                InMemoryStateStore::new(chain.chain_id()),
            ),
            state: StateSource::Node(Arc::clone(&chain)),
            chain,
            graph: evm_graph::MarketGraphBuilder::new(),
            ledger: OpportunityLedger::new(LedgerPolicy {
                max_active: config.ledger.max_active,
                stale_history: config.ledger.stale_history,
            }),
            metrics: Metrics::default(),
            clock,
            settled: BTreeSet::new(),
            registry_pools: registry.pools.len(),
        }
    }

    /// Point this run's simulation state reads at one block's recorded state
    /// instead of at the node (§63's replay acceptance).
    ///
    /// Only the simulation half moves: the market path below still reads the
    /// blocks it is told to read, so a replay run and a live run apply the same
    /// logs through the same engine.
    pub fn with_recorded_state(mut self, dump: StateDump, file: PathBuf) -> Self {
        self.state = StateSource::Recorded {
            dump: Arc::new(dump),
            file,
        };
        self
    }

    /// Where findings get their state.
    pub fn state(&self) -> &StateSource {
        &self.state
    }

    pub fn chain(&self) -> &Arc<dyn ChainAdapter> {
        &self.chain
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain.chain_id()
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn metrics_mut(&mut self) -> &mut Metrics {
        &mut self.metrics
    }

    pub fn ledger_stats(&self) -> evm_opportunity::LedgerStats {
        self.ledger.stats()
    }

    pub fn registry_pools(&self) -> usize {
        self.registry_pools
    }

    pub fn pool_count(&self) -> usize {
        self.replay.store().pool_count()
    }

    pub fn synced_pool_count(&self) -> usize {
        self.replay.store().synced_pool_count()
    }

    pub fn snapshot(&self) -> evm_state::StateSnapshot {
        self.replay.snapshot()
    }

    /// A finding is still giveable to the risk layer only while it is active at
    /// the version it was priced against (§24: a simulation that finishes after
    /// its state moved is not an executable opportunity).
    pub fn live_finding(&self, id: &OpportunityId) -> Option<&TrackedOpportunity> {
        self.ledger
            .active()
            .iter()
            .find(|entry| entry.id == *id && entry.is_active())
    }

    /// The whole path for one sealed block. Errors only where the run cannot
    /// continue honestly; a market that offers nothing is a result.
    pub async fn on_canonical(
        &mut self,
        announcement: &BlockAnnouncement,
        max_jobs_per_block: usize,
    ) -> Result<BlockOutcome> {
        let number = announcement.number;
        let started = self.clock.now_ms();
        let mut timing = PipelineTiming::new(
            announcement.chain_timestamp_secs,
            announcement.observed_at_unix_ms,
            started,
        );

        let mut report = ReplayReport::default();
        self.replay
            .replay_block(number, &mut report)
            .await
            .map_err(PipelineError::Replay)?;
        report.chain_id = Some(self.chain_id());
        report.from_block = Some(number);
        report.to_block = Some(number);
        let now = self.clock.now_ms();
        timing.set_decoded(now);
        timing.set_state_updated(now);

        self.metrics.add("blocks", 1);
        self.metrics.add("logs", report.logs);
        self.metrics.add("sync_events", report.sync_events);
        self.metrics.add("swap_events", report.swap_events);
        self.metrics
            .add("pool_created_events", report.pool_created_events);
        self.metrics.add("unclaimed_logs", report.unclaimed_logs);
        self.metrics
            .add("unattested_syncs", report.unattested_syncs);
        self.metrics.add("syncs_applied", report.syncs_applied);
        self.metrics.add("registrations", report.registrations);
        self.metrics.add("rejected_syncs", report.rejected_syncs);
        self.metrics
            .add("rejections", report.rejections.len() as u64);
        self.metrics.record_chain_lag(
            "chain_to_received",
            announcement.chain_timestamp_secs,
            announcement.observed_at_unix_ms,
        );

        // §16: the pools this block restated are the pools whose findings die now.
        let mut changed: Vec<ChainPool> = report
            .state_changes
            .iter()
            .filter(|change| change.written == Written::ReserveStatement)
            .map(|change| change.pool)
            .collect();
        changed.sort_unstable();
        changed.dedup();

        let version = self.replay.snapshot().position;
        let mut invalidated = Vec::new();
        if let Some(position) = version {
            for pool in &changed {
                invalidated.extend(self.ledger.invalidate_pool(*pool, position));
            }
        }
        self.metrics
            .add("invalidated_by_pool", invalidated.len() as u64);
        for id in &invalidated {
            // §65's number with a name: the staleness rule stopped a finding that
            // had not yet been given to a worker. One that had is a different fact
            // — the answer arrives later and is suppressed there — and the two are
            // counted separately rather than merged into one "stale" total.
            if !self.settled.contains(id) {
                self.metrics.bump("stale_before_simulation");
            }
        }

        // §13/§14: the graph is rebuilt from the store, immutably, per block.
        let mut graph = None;
        let mut detection = None;
        let mut scanned = false;
        if version.is_some() {
            let build = self.graph.build_traced(&self.replay.snapshot())?;
            self.metrics
                .add("graph_edges", build.graph.edge_count() as u64);
            self.metrics
                .add("graph_skipped_pools", build.skipped.len() as u64);
            for skipped in &build.skipped {
                // `SkipReason` is a fieldless enum, so its Debug name is stable
                // and the counter keys are readable in the metrics file.
                self.metrics
                    .bump(&format!("graph_skip.{:?}", skipped.reason));
            }
            graph = Some(build);
            timing.set_graph_updated(self.clock.now_ms());
            self.metrics
                .record_from("block_to_graph", started, self.clock.now_ms());

            // §15's trigger: restated pool → affected pair → scan.
            scanned = !changed.is_empty();
            if scanned {
                let found =
                    detect_opportunities(&graph.as_ref().expect("assigned just above").graph)?;
                self.metrics
                    .add("candidates", found.candidates.len() as u64);
                self.metrics
                    .add("candidate_rejections", found.rejected.len() as u64);
                self.metrics
                    .add("opportunities_found", found.opportunities.len() as u64);
                detection = Some(found);
                timing.set_opportunity_detected(self.clock.now_ms());
                self.metrics
                    .record_from("block_to_opportunity", started, self.clock.now_ms());
            }
        }

        let mut to_simulate = Vec::new();
        if let (Some(found), Some(position)) = (detection.as_ref(), version) {
            let observed_at = evm_live::now_unix_ms();
            for opportunity in &found.opportunities {
                match self
                    .ledger
                    .register(opportunity, position, announcement.hash, observed_at)
                {
                    Registered::Accepted => self.metrics.bump("opportunity_registered"),
                    Registered::Duplicate { first_version } => {
                        self.metrics.bump("opportunity_duplicate");
                        if first_version != position {
                            // Same identity, different state version: worth its
                            // own number, because it says the market re-priced a
                            // route the ledger already named (§51, not silently
                            // merged).
                            self.metrics.bump("opportunity_duplicate_at_new_version");
                        }
                    }
                    Registered::RejectedForCapacity => {
                        // §51: the finding was not held, and that is a fact.
                        self.metrics.bump("opportunity_evicted_for_capacity");
                    }
                }
            }
        }

        // The offer to the simulation stage runs on every block, not only on the
        // ones that found something: a finding a full queue declined stays in the
        // ledger, and §23's retry only means something if a later block asks
        // again. `simmable` cannot yield a finding whose pools moved — that entry
        // left the active set above — which is why nothing here has to remember to
        // check for staleness (§65).
        if version.is_some() {
            for entry in self.ledger.simmable() {
                if self.settled.contains(&entry.id) {
                    continue;
                }
                if to_simulate.len() >= max_jobs_per_block {
                    self.metrics.bump("simulation_dispatch_declined_by_bound");
                    continue;
                }
                to_simulate.push(entry);
            }
        }

        Ok(BlockOutcome {
            announcement: *announcement,
            timing,
            report,
            changed_pools: changed,
            invalidated,
            graph,
            detection,
            state_version: version,
            to_simulate,
            scanned,
        })
    }

    /// Record that this run is finished offering one finding: it went to a worker,
    /// or it was declined for a reason that will not change with the next block. A
    /// job the queue refused is *not* marked, so a later block may ask again — the
    /// retry is counted, never silent.
    pub fn note_settled(&mut self, id: &OpportunityId) {
        self.settled.insert(*id);
    }

    pub fn settled_count(&self) -> usize {
        self.settled.len()
    }

    /// Stamp and count the simulation half of the journey.
    pub fn note_simulated(&mut self, timing: &PipelineTiming) {
        if let (Some(start), Some(finish)) =
            (timing.simulation_started_at, timing.simulation_finished_at)
        {
            self.metrics
                .record_latency("simulation_duration", finish - start);
        }
        if let Some(finish) = timing.simulation_finished_at {
            self.metrics
                .record_from("block_to_simulation_end", timing.received_at, finish);
        }
    }

    pub fn note_risk_decided(&mut self, timing: &PipelineTiming) {
        if let Some(decided) = timing.risk_decided_at {
            self.metrics
                .record_from("block_to_risk", timing.received_at, decided);
        }
    }
}
