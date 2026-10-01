//! The simulation stage: a bounded queue, worker threads, and no `unsafe`.
//!
//! ## Why the workers are where they are (§22)
//!
//! M4's [`evm_simulation::Simulator`] is `#[async_trait(?Send)]`, so the future
//! `run()` returns is **not** `Send`. `tokio::spawn` on a multi-thread runtime
//! requires `Send`, and the ways out of that are the one this file takes and the
//! ones it refuses:
//!
//! ```text
//! Option A  make the provider/DB abstraction Send + Sync
//!           → touches M4's execution layer, which §1 names as the baseline. The
//!             REVM database handle is why the future is !Send in the first
//!             place; this milestone does not get to redesign it.
//! Option B  one dedicated single-thread runtime for all simulations
//!           → every run queues behind one thread, so §23's "block 101 while a
//!             block-100 simulation is running" costs the whole simulation stage.
//! Option C  spawn_blocking, one current-thread runtime per worker  ← CHOSEN
//!           → the !Send future never leaves the thread that created it, so no
//!             `unsafe impl Send` anywhere; workers are OS threads, so they are
//!             beside ingestion rather than inside it; and the queue between them
//!             is bounded, which gives §50 one answer per edge instead of none.
//! ```
//!
//! The cost of C is measured rather than asserted: [`WorkerReport`] carries
//! per-worker counts, and the run's `simulation_duration` / `simulation_queue_wait`
//! series land in the metrics file — §22's "benchmark, document, test" is
//! answered by the evidence a real run writes.
//!
//! ## What may enter the queue (§18, §19, §20)
//!
//! A job reads state at **the block the finding was priced at**, pinned by that
//! block's own hash. `latest` appears nowhere here: the provider is built with a
//! [`evm_simulation::BlockPin`], and the engine re-checks the pin against
//! whatever the node answers before it executes anything.
//!
//! ## What may not
//!
//! M4's §57 rule confines setup overrides to the test sender's own account, so
//! token balances are never manufactured. A route whose input token is not the
//! chain's wrapped native asset therefore **cannot be funded** by this pipeline:
//! it is recorded as a decline with its reason and counted, not skipped, and not
//! simulated against invented state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use serde::Serialize;
use tokio::sync::{mpsc, Mutex};

use evm_chain::{BlockContext, ChainAdapter};
use evm_core::BlockNumber;
use evm_metrics::{Clock, PipelineTiming};
use evm_opportunity::{
    swap_exact_in, Hop, Opportunity, OpportunityId, PricedHop, TrackedOpportunity,
};
use evm_risk::RiskThresholds;
use evm_simulation::{
    BlockPin, DumpStateProvider, Funding, GasPricing, PricedRoute, RouteLeg, RpcStateProvider,
    Settle, SimulationRequest, SimulationResult, StateDump, StateProvider, StateSpec,
    TransactionSpec,
};
use evm_state::UpdatePosition;

use crate::config::RiskConfig;

/// A finding that reached the head of the simulation queue.
pub struct SimulationJob {
    pub id: OpportunityId,
    /// The store version this finding is bound to — the block its state reads
    /// are pinned to (§18).
    pub state_version: UpdatePosition,
    pub observed_block: BlockNumber,
    pub request: SimulationRequest,
    pub provider: Arc<dyn StateProvider>,
    /// Which risk thresholds this run must be decided against, fixed here from
    /// the pinned header. §25's gas ceiling is `max(header.gas_limit)`, and a
    /// header is only ever read at one height — so deciding it after the fact
    /// would let a later block answer a question about an earlier one.
    pub risk: RiskThresholds,
    /// Where the state reads go, in the words the provider gives (§41's evidence
    /// has to name the source of a run that did not come from the recorded dump).
    pub state_source: String,
    /// Stamped by the worker as the run proceeds.
    pub timing: PipelineTiming,
    /// Monotonic ms, on the run's clock, when the job was handed to the queue.
    pub enqueued_at: u64,
}

impl SimulationJob {
    /// What the run was asked to produce, for the evidence line.
    pub fn asked_output(&self) -> U256 {
        self.request.transaction.asked_output
    }
}

/// Why a finding was *not* simulated. A decline is a result with a reason, not
/// an absence (§51).
#[derive(Clone, Debug)]
pub struct Decline {
    pub id: OpportunityId,
    pub observed_block: BlockNumber,
    pub reason: String,
    /// A stable label, so the count is groupable in the metrics file.
    pub rule: &'static str,
}

/// Either a job for the queue, or the reason there isn't one.
pub enum JobPlan {
    /// Boxed: a job carries a whole `SimulationRequest`, and the decline that
    /// sits beside it names a block and a rule. One allocation keeps the plan
    /// the pipeline holds on the way to the queue the size of a pointer.
    Ready(Box<SimulationJob>),
    Declined(Decline),
}

/// What a worker produced for one job.
#[derive(Clone, Debug)]
pub enum SimRun {
    /// The EVM answered. A revert is an answer (M4's §34), so this covers
    /// completed and reverted runs alike; the status is inside.
    Executed(Box<SimulationResult>),
    /// No answer was produced: the request refused, or the provider could not
    /// serve the pinned state.
    Refused(String),
}

/// One worker's report on one job, on its way back to the pipeline.
#[derive(Clone, Debug)]
pub struct SimOutcome {
    pub id: OpportunityId,
    pub state_version: UpdatePosition,
    pub observed_block: BlockNumber,
    pub worker: usize,
    pub run: SimRun,
    pub timing: PipelineTiming,
    /// The thresholds the decision must be taken against — the ones chosen when
    /// the job was planned, from the pinned header.
    pub risk: RiskThresholds,
    pub state_source: String,
    /// The finding's own claim, kept beside the answer it got.
    pub analytical_output: U256,
    pub gross_profit: U256,
    /// How long the job sat in the queue before a worker took it, on the run's
    /// clock. §23's cost, stated separately from the execution it delayed.
    pub queue_wait_ms: u64,
}

impl SimOutcome {
    pub fn is_refused(&self) -> bool {
        matches!(self.run, SimRun::Refused(_))
    }
}

/// Per-worker counters: §22's chosen option, measurable rather than described.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WorkerReport {
    pub worker: usize,
    pub jobs_run: u64,
    pub jobs_refused: u64,
    /// Jobs taken off the queue that were never run because `stop` was set first.
    /// Counted: a job that vanishes silently is the thing §51 forbids.
    pub jobs_abandoned: u64,
    /// How long this worker existed, so a throughput figure can be divided out
    /// of `jobs_run` instead of being asserted.
    pub span_ms: u64,
    /// The worker's own view of the `recv` timeouts it sat through — the proof
    /// it was alive and idle rather than dead.
    pub idle_polls: u64,
}

/// Where a finding's state is read from.
///
/// There are two answers, and which one a run uses is stated in its evidence
/// rather than left to be inferred:
///
/// ```text
/// Node       the endpoint this run reads its blocks from, at the height the
///            finding was priced at — what a live run does, and the same
///            connection pool the state path uses (§62)
/// Recorded   one block's state as an earlier run recorded it off that node
/// ```
///
/// The second is §63's route for acceptance over recorded live events. It is a
/// recording, not a fixture of invented values: [`StateDump`] is written by
/// [`RpcStateProvider`] as it reads, so the file is the same bytes the node
/// served. It answers for **one height**, and a finding priced at any other
/// block is refused rather than handed state that is not its own (§18, §19).
#[derive(Clone)]
pub enum StateSource {
    Node(Arc<dyn ChainAdapter>),
    Recorded {
        /// Shared rather than held inline: a finding's job reads this dump once,
        /// while the enum itself is cloned by every stage that names the source.
        dump: Arc<StateDump>,
        /// The file the dump was read from, because the evidence has to name
        /// where state came from.
        file: PathBuf,
    },
}

impl StateSource {
    /// The header for `number`, or the reason there isn't one.
    ///
    /// A refusal is not an error of the run: the caller writes it as a decline
    /// and keeps going, which is what §51 asks of a finding that could not be
    /// given its own state.
    pub async fn header_at(
        &self,
        number: BlockNumber,
    ) -> std::result::Result<BlockContext, String> {
        match self {
            Self::Node(chain) => chain.get_block_context(number).await.map_err(|error| {
                // Not "the node": this arm also serves a replay run, whose state
                // source is a recorded directory that no one could call. The
                // provider's own words carry which one it was (§53).
                format!(
                    "the state source of this run did not answer for block {}: {error}",
                    number.0
                )
            }),
            Self::Recorded { dump, file } => {
                if dump.block_number != number.0 {
                    return Err(format!(
                        "this run reads state from {}, a recording of block {}, and the finding \
                         is priced at block {}; a recording is not a substitute for the state a \
                         finding was priced against (§19)",
                        file.display(),
                        dump.block_number,
                        number.0,
                    ));
                }
                let recorded = dump.header.ok_or_else(|| {
                    format!(
                        "{} records no header for block {}, so its base fee, gas limit and block \
                         identity are unknown; the engine refuses state it did not read (§20)",
                        file.display(),
                        dump.block_number,
                    )
                })?;
                recorded.into_context(dump).map_err(|error| {
                    format!(
                        "{} could not be turned into a block context: {error}",
                        file.display()
                    )
                })
            }
        }
    }

    /// The provider a job reads through, pinned to the header the job was built
    /// against.
    fn provider(&self, pin: BlockPin) -> Arc<dyn StateProvider> {
        match self {
            Self::Node(chain) => Arc::new(RpcStateProvider::new(Arc::clone(chain), pin)),
            Self::Recorded { dump, file } => Arc::new(DumpStateProvider::new(
                (**dump).clone(),
                format!("{}@{}", file.display(), dump.block_number),
            )),
        }
    }

    /// What the run's evidence says about where state came from, before any
    /// finding exists to ask about.
    pub fn describe(&self) -> String {
        match self {
            Self::Node(_) => "chain: the source this run reads its blocks from".to_string(),
            Self::Recorded { dump, file } => {
                format!("recorded:{}@{}", file.display(), dump.block_number)
            }
        }
    }
}

/// The block a finding must be simulated against, as the chain described it.
///
/// A `Declined` here is a result with a reason, not a failure of the run: a
/// finding this pipeline is not allowed to fund is still a finding the evidence
/// has to name.
pub fn plan_job(
    entry: &TrackedOpportunity,
    header: &BlockContext,
    source: &StateSource,
    wrapped_native: Option<Address>,
    risk: &RiskConfig,
    clock: Clock,
    timing: PipelineTiming,
) -> JobPlan {
    let id = entry.id;
    let observed_block = entry.opportunity.block_number;
    let decline = |rule: &'static str, reason: String| {
        JobPlan::Declined(Decline {
            id,
            observed_block,
            reason,
            rule,
        })
    };

    match wrapped_native {
        Some(needed) if entry.opportunity.input_token.address != needed => {
            return decline(
                "funding_unavailable",
                format!(
                    "the route spends {} while this run's wrapped native asset is {needed}; \
                     M4's §57 confines setup overrides to the test sender, so no ERC-20 balance \
                     may be manufactured, and a sender the pinned state does not fund cannot \
                     trade this route",
                    entry.opportunity.input_token.address,
                ),
            );
        }
        Some(_) => {}
        None => {
            return decline(
                "no_wrapped_native_configured",
                "this run was given no wrapped-native address, so no route can be funded \
                 without inventing a token balance (§57)"
                    .to_string(),
            );
        }
    }
    if header.number != entry.state_version.block_number {
        return decline(
            "state_version_mismatch",
            format!(
                "the finding is bound to block {} but the header handed over describes block \
                 {}; simulating either one would not be simulating the state it was priced \
                 against (§18)",
                entry.state_version.block_number.0, header.number.0,
            ),
        );
    }

    let route = match priced_route(&entry.opportunity) {
        Ok(route) => route,
        Err(reason) => return decline("route_not_executable", reason),
    };

    let pin = BlockPin::new(header.number, header.hash);
    let provider = source.provider(pin);
    if provider.pin() != pin {
        return decline(
            "state_pin_mismatch",
            format!(
                "the state source answers to {:?} while the finding is priced at block {} as \
                 {:?}; one of the two is not the block this finding belongs to (§19, §20)",
                provider.pin(),
                header.number.0,
                pin,
            ),
        );
    }
    let source = provider.source();
    let transaction = TransactionSpec::canonical(
        Funding::WrapNative,
        Settle::UnwrapInputToken,
        entry.opportunity.output_amount,
    );
    let pricing = GasPricing::Eip1559 {
        priority_fee_per_gas: 0,
        provenance: format!(
            "block {}'s own base fee as its header reports it, with no tip: a hypothetical \
             transaction on an already-sealed block is not competing to be included in it. \
             Risk ceiling: {}.",
            header.number.0,
            risk.provenance(header),
        ),
    };
    let request = match SimulationRequest::new(
        header.clone(),
        route,
        transaction,
        StateSpec::new(source.clone()),
        pricing,
    ) {
        Ok(request) => request,
        Err(error) => {
            return decline(
                "request_refused",
                format!("the request was refused: {error}"),
            )
        }
    };
    JobPlan::Ready(Box::new(SimulationJob {
        id,
        state_version: entry.state_version,
        observed_block,
        request,
        provider,
        risk: risk.thresholds(header),
        state_source: source,
        timing,
        enqueued_at: clock.now_ms(),
    }))
}

/// M3's finding, in the form an EVM can be handed.
///
/// The same mapping M4's end-to-end test uses, for the same reason: the route's
/// reserves and fee are the ones the detector attested, so what executes is what
/// was claimed.
pub fn priced_route(opportunity: &Opportunity) -> std::result::Result<PricedRoute, String> {
    let [first, second] = opportunity.hops;
    let [first_hop, second_hop] = opportunity.path.hops();
    let leg = |hop: Hop, priced: PricedHop| RouteLeg {
        pool: priced.pool,
        token_in: hop.token_in,
        token_out: hop.token_out,
        reserve_in: priced.reserve_in,
        reserve_out: priced.reserve_out,
        fee: priced.fee,
    };
    let mid = swap_exact_in(
        first.reserve_in,
        first.reserve_out,
        first.fee,
        opportunity.input_amount,
    )
    .map_err(|error| format!("the first hop does not quote: {error}"))?;
    PricedRoute::new(
        opportunity.chain_id,
        opportunity.block_number,
        leg(first_hop, first),
        leg(second_hop, second),
        opportunity.input_amount,
        mid,
        opportunity.output_amount,
        opportunity.gross_profit,
        format!(
            "evm-opportunity::detect_opportunities on block {}, search {:?}",
            opportunity.block_number.0, opportunity.search.strategy,
        ),
    )
    .map_err(|error| format!("the finding is not executable-shaped: {error}"))
}

/// The queue, the workers, and the handle that stops them.
pub struct SimulationPool {
    pub sender: mpsc::Sender<SimulationJob>,
    pub outcomes: mpsc::Receiver<SimOutcome>,
    handles: Vec<tokio::task::JoinHandle<WorkerReport>>,
    stop: Arc<AtomicBool>,
}

impl SimulationPool {
    /// `capacity` is §50's bound on the queue; `workers` is how many threads
    /// outside this runtime drive REVM.
    pub fn spawn(capacity: usize, outcome_capacity: usize, workers: usize, clock: Clock) -> Self {
        let (sender, receiver) = mpsc::channel::<SimulationJob>(capacity.max(1));
        let (outcome_sender, outcomes) = mpsc::channel::<SimOutcome>(outcome_capacity.max(1));
        let stop = Arc::new(AtomicBool::new(false));
        let receiver = Arc::new(Mutex::new(receiver));
        let handles = (0..workers.max(1))
            .map(|index| {
                spawn_worker(
                    index,
                    Arc::clone(&receiver),
                    outcome_sender.clone(),
                    Arc::clone(&stop),
                    clock,
                )
            })
            .collect();
        // The pool's own copy goes: the workers hold the rest, and `shutdown`
        // drops the queue sender so an idle worker can leave `recv`.
        drop(outcome_sender);
        Self {
            sender,
            outcomes,
            handles,
            stop,
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Join every worker and collect its report. §49: an in-flight simulation is
    /// finished or abandoned with a count, never left running as the process
    /// exits.
    pub async fn shutdown(self) -> Vec<WorkerReport> {
        self.stop();
        let Self {
            sender,
            outcomes,
            mut handles,
            ..
        } = self;
        // Closing the queue is what lets an idle worker leave its `recv()`: the
        // workers still drain whatever is already queued, then see `None`.
        drop(sender);
        drop(outcomes);
        let mut reports = Vec::with_capacity(handles.len());
        for handle in handles.drain(..) {
            match handle.await {
                Ok(report) => reports.push(report),
                Err(_error) => reports.push(WorkerReport {
                    // The sentinel worker index says "this thread died"; the
                    // runner writes it to the evidence stream, because a worker
                    // that vanished is §51's kind of fact.
                    worker: usize::MAX,
                    jobs_abandoned: 1,
                    idle_polls: 0,
                    ..WorkerReport::default()
                }),
            }
        }
        reports.sort_by_key(|report| report.worker);
        reports
    }
}

/// One Option C worker: a blocking thread owning a current-thread runtime, so
/// the `!Send` future is only ever polled by the thread that made it.
fn spawn_worker(
    index: usize,
    receiver: Arc<Mutex<mpsc::Receiver<SimulationJob>>>,
    outcomes: mpsc::Sender<SimOutcome>,
    stop: Arc<AtomicBool>,
    clock: Clock,
) -> tokio::task::JoinHandle<WorkerReport> {
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime for the simulation worker");
        runtime.block_on(worker_loop(index, receiver, outcomes, stop, clock))
    })
}

async fn worker_loop(
    index: usize,
    receiver: Arc<Mutex<mpsc::Receiver<SimulationJob>>>,
    outcomes: mpsc::Sender<SimOutcome>,
    stop: Arc<AtomicBool>,
    clock: Clock,
) -> WorkerReport {
    let started = Instant::now();
    let mut report = WorkerReport {
        worker: index,
        ..WorkerReport::default()
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // The lock guards the receive handle only, never a run: holding it across
        // `run()` would put every worker back in a single file.
        let job = {
            let mut queue = receiver.lock().await;
            match tokio::time::timeout(Duration::from_millis(150), queue.recv()).await {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(_elapsed) => {
                    report.idle_polls += 1;
                    continue;
                }
            }
        };
        if stop.load(Ordering::Relaxed) {
            report.jobs_abandoned += 1;
            break;
        }
        let outcome = run_job(index, job, clock).await;
        report.jobs_run += 1;
        if outcome.is_refused() {
            report.jobs_refused += 1;
        }
        if outcomes.send(outcome).await.is_err() {
            // Nothing is reading results. §51: that is reported, and the worker
            // stops rather than producing uncounted work.
            break;
        }
    }
    report.span_ms = started.elapsed().as_millis() as u64;
    report
}

/// Run one job and stamp the two simulation stages on its way through.
async fn run_job(index: usize, job: SimulationJob, clock: Clock) -> SimOutcome {
    let SimulationJob {
        id,
        state_version,
        observed_block,
        request,
        provider,
        risk,
        state_source,
        mut timing,
        enqueued_at,
    } = job;
    let analytical_output = request.route.analytical_output;
    let gross_profit = request.route.analytical_gross_profit;
    let started = clock.now_ms();
    // The queue wait is the part of §23's latency that is *this design's* cost,
    // so it is measured as itself rather than folded into the run.
    let queue_wait = started.saturating_sub(enqueued_at);
    timing.set_simulation_started(started);
    let run = match evm_simulation::engine::run(provider, &request).await {
        Ok(result) => SimRun::Executed(Box::new(result)),
        Err(error) => SimRun::Refused(error.to_string()),
    };
    timing.set_simulation_finished(clock.now_ms());
    SimOutcome {
        id,
        state_version,
        observed_block,
        worker: index,
        run,
        timing,
        risk,
        state_source,
        analytical_output,
        gross_profit,
        queue_wait_ms: queue_wait,
    }
}

/// §51: a job that could not be queued is a fact, and this is the shape it is
/// written down in.
pub fn decline_line(decline: &Decline) -> serde_json::Value {
    serde_json::json!({
        "opportunity_id": decline.id.to_string(),
        "observed_block": decline.observed_block.0,
        "rule": decline.rule,
        "reason": decline.reason,
    })
}
