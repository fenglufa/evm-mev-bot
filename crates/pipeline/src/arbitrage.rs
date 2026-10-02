//! M7 §57's A→Q walked in one call: a live route, decided, sent, and accounted for.
//!
//! [`crate::runner`] answers a different question — whether a market event can travel the
//! whole loop while every stage keeps the semantics it earned. This file answers M7's
//! question: whether **one** route, read off the chain at one pinned block, is worth
//! broadcasting, and what actually happened when it was. So it runs no engine, no graph, no
//! queue: a route this size is decided by reading two pools, pricing both legs, executing the
//! six steps in REVM against the node's own state, and handing the result to
//! [`SequenceStage`]. Every stage it reuses is the same type the live path uses; nothing here
//! is a second implementation of a decision someone else already made.
//!
//! ```text
//! head → pin → read both venues → orient → price both legs   (§5: input from current real state)
//!     → REVM the canonical six steps                          (§B: real simulation, no mock)
//!     → risk thresholds → Accept or stop                      (§C)
//!     → SequencePlan::from_run                                (§16's shape; §1 forbids making one)
//!     → LivePreflightReads::gather → PreflightReport          (§D: §26's checks, §27's no re-search)
//!     → SequenceStage::run                                    (§E–§H: build, sign, submit, receipt)
//!     → delta / flow / route / profit audits                  (§I–§N, §O)
//!     → §52 counters, §53 single-sample latencies, one record (§P: everything recomputable)
//! ```
//!
//! ## The four rules that decide the shape of this file
//!
//! - **§1: the route is found, not made.** The only inputs to the price are `getReserves()`,
//!   `token0()` and `token1()` read at one pinned block, plus a fee an earlier run *measured*
//!   by bisection and this run cites by evidence path. No reserve, balance, allowance,
//!   threshold or gas figure is touched, and the ban is checked rather than asserted:
//!   [`SimulationRequest::preflight`] must return an empty setup vector, because a route that
//!   needed a manufactured balance to be possible at all is this file refusing, not running.
//! - **§34: what was not paid for cannot be sent.** The sender is declared with
//!   [`evm_simulation::Endowment::PinnedState`], so the run's own account of its funding is
//!   [`SenderFunding::RealState`] and the lane's build policy would reject any other.
//! - **§27: the gate does not re-search.** One candidate, handed in by the caller. A failed
//!   preflight ends the attempt — this module holds no list of alternatives to try next, and a
//!   second route is a second invocation with its own evidence directory.
//! - **§51: the label is an argument.** `REAL_MARKET` or `CONTROLLED_FIXTURE` travels on the
//!   plan, into the report, and into the counters, and a fixture's profit is never counted as
//!   a real arbitrage whatever the receipts say.
//!
//! A refusal is a result, not an exception. Every stage that stops short leaves the rows it
//! already produced on disk plus a `route-run.json` that names the stage and the reason, and
//! `run_once` returns it as an [`ArbitrageRun`] with `refusal` set — because §Q's acceptance
//! item is about a report that tells the truth, and a `PARTIAL`/`BLOCKED` outcome (§59) is
//! only reachable if the run that earned it is still a run.
//!
//! Nothing in this module names a key, a submission method or an endpoint. Those live in
//! `evm-execution`, and the flags live in `evm-cli`; the keyword scan in
//! `crates/cli/tests/no_execution.rs` is what keeps that true.

use std::path::PathBuf;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use serde_json::{json, Value};

use evm_chain::{BlockContext, ChainAdapter, HttpChainAdapter};
use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};
use evm_execution::giwa::{read_pool, LivePreflightReads};
use evm_execution::{
    ExecutionMode, ExecutionRecord, ExecutionSetup, Freshness, GateAttempt, Ledger, MarketKind,
    ProfitVerificationStatus, SenderFunding, SequencePlan, SequenceReport, SequenceStage,
    Tolerance,
};
use evm_metrics::{Clock, LatencyTrace, Metrics, Stage, TraceSource};
use evm_opportunity::swap_exact_in;
use evm_risk::{RiskDecision, RiskPolicy, NO_BROADCAST};
use evm_simulation::{
    engine::run as simulate, BlockPin, EvmRules, Funding, GasPricing, PricedRoute, RouteLeg,
    RpcStateProvider, Settle, SimulationRequest, SimulationResult, SimulationSender, StateProvider,
    StateSpec, TransactionSpec,
};

use crate::config::RiskConfig;
use crate::error::{PipelineError, Result};
use crate::evidence::{EvidenceFile, EvidenceWriter};
use crate::latency::{git_revision, record_ladder, LatencyEvidence, TraceRecorder};
use crate::runner::attested_chain_ids;

/// The finished run's own record, written whole through a temporary name (§49).
const RECORD_FILE: &str = "route-run.json";

/// §5's candidate, in the facts that make it one: the pair, the two venues, the asset the
/// round trip is denominated in, the size, and the fee with the measurement that established
/// it.
///
/// The fee travels as a [`Fee`] plus a provenance string rather than being looked up here,
/// because §5's "fee proven" is a claim about evidence that already exists — a file written by
/// a run that bisected these pools' own bytecode — and this module's job is to name that file,
/// not to re-derive it (§27: neither the gate re-searches nor the run that feeds it).
#[derive(Clone, Debug)]
pub struct RouteCandidate {
    /// `0` means "whatever the endpoint answers for", which §45 makes the default and this
    /// module still checks: the chain a candidate was named on is stated in the evidence.
    pub chain_id: u64,
    /// §16's input asset: the chain's wrapped native token.
    pub input_token: Address,
    /// The token the two venues disagree about.
    pub mid_token: Address,
    /// The two venues, in no meaningful order — which one is bought through is decided from
    /// the reserves read at the pinned block, never from the order they were typed in.
    pub venues: [Address; 2],
    pub input_amount: U256,
    pub fee: Fee,
    /// Where a reader can check the fee, in words that name a file.
    pub fee_evidence: String,
}

/// Everything one route run needs that is not the chain.
#[derive(Clone, Debug)]
pub struct ArbitrageConfig {
    pub rpc_url: String,
    /// §46's boundary: the endpoint must answer for the chain the repository attests.
    pub registry_dirs: Vec<PathBuf>,
    /// The wallet that signs, pays for gas, and holds every asset the route touches.
    pub sender: Address,
    /// Who that account is, in the caller's words. It goes into the simulation's sender label,
    /// so an evidence file says whose balance it spent instead of leaving a 20-byte address to
    /// be recognised by a reader.
    pub sender_label: &'static str,
    pub candidate: RouteCandidate,
    pub market: MarketKind,
    pub setup: ExecutionSetup,
    /// §18's tolerance between what the simulation measured and what the receipts report.
    pub tolerance: Tolerance,
    pub risk: RiskConfig,
    pub evidence_dir: PathBuf,
    /// M8.1 §40: the base directory this run also writes its latency traces to, or `None`
    /// for a run that measures nothing. The run makes its own subdirectory of this one,
    /// named after the session, so a baseline already on disk is never rewritten (§46).
    ///
    /// Nothing downstream consults it. A run with a value here and a run without one
    /// price the same route, run the same EVM, take the same decisions and write the same
    /// `route-run.json` (§41's backward-compatibility requirement, which the M7 evidence
    /// files are the proof of).
    pub latency_dir: Option<PathBuf>,
}

/// One venue, as the node described it at the block the run pinned.
///
/// The pair's own ordering is kept (`token0`/`reserve0`, `token1`/`reserve1`) rather than a
/// pre-oriented `(in, out)` pair, because this run asks each pool the question in both
/// directions: the buy leg moves input→mid and the sell leg mid→input, and getting the second
/// one's sides wrong is precisely the mistake that turns a stale market into a phantom profit.
#[derive(Clone, Debug)]
pub struct Venue {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    /// `blockTimestampLast` — §8's staleness question is answerable from this and from nothing
    /// else in a pool whose last trade is old.
    pub last_synced: U256,
    pub read_at_block: u64,
    pub source: String,
}

impl Venue {
    /// The reserves in the direction the route moves: this pool's balance of `from`, then of
    /// `to`, or the reason it holds neither.
    fn reserves(&self, from: Address, to: Address) -> std::result::Result<(U256, U256), String> {
        if self.token0 == from && self.token1 == to {
            Ok((self.reserve0, self.reserve1))
        } else if self.token1 == from && self.token0 == to {
            Ok((self.reserve1, self.reserve0))
        } else {
            Err(format!(
                "venue {} holds {} and {}, which is not the pair {} then {}; its reserves \
                 cannot be ordered, and ordering them by assumption is how a wrong side becomes \
                 a phantom profit",
                self.pool, self.token0, self.token1, from, to
            ))
        }
    }

    fn quote(
        &self,
        from: Address,
        to: Address,
        fee: Fee,
        amount: U256,
    ) -> std::result::Result<U256, String> {
        let (reserve_in, reserve_out) = self.reserves(from, to)?;
        swap_exact_in(reserve_in, reserve_out, fee, amount)
            .map_err(|error| format!("{} will not price {from}→{to}: {error}", self.pool))
    }

    fn row(&self) -> Value {
        json!({
            "pool": format!("{:#x}", self.pool),
            "token0": format!("{:#x}", self.token0),
            "token1": format!("{:#x}", self.token1),
            "reserve0": self.reserve0.to_string(),
            "reserve1": self.reserve1.to_string(),
            "getReserves_blockTimestampLast": self.last_synced.to_string(),
            "read_at_block": self.read_at_block,
            "source": self.source,
        })
    }
}

/// The route as this run priced it: the two legs, both demands, and the structure the
/// simulation and §26's gate both read.
#[derive(Clone, Debug)]
pub struct PricedLegs {
    pub buy: Venue,
    pub sell: Venue,
    /// What the buying pool is asked to pay, in the mid token.
    pub mid_demand: U256,
    /// What the selling pool is asked to pay back, in the input token — the zero-slippage
    /// demand, which is also the amount the run asks the EVM to produce.
    pub out_demand: U256,
    pub route: PricedRoute,
}

/// Where an attempt stopped, in the §57 vocabulary.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub stage: &'static str,
    pub detail: String,
}

/// One route run: what was read, what was decided, what was sent, what it made — and the
/// reason it went no further when it went no further.
pub struct ArbitrageRun {
    pub session_id: String,
    pub chain_id: u64,
    pub head: BlockNumber,
    pub header: BlockContext,
    pub mode: ExecutionMode,
    pub market: MarketKind,
    pub sender: Address,
    pub candidate: RouteCandidate,
    pub legs: Option<PricedLegs>,
    pub simulation: Option<SimulationResult>,
    pub decision: Option<RiskDecision>,
    pub plan: Option<SequencePlan>,
    pub preflight: Option<Value>,
    pub report: Option<SequenceReport>,
    pub refusal: Option<Refusal>,
    pub metrics: Value,
    pub latency_ms: Value,
    pub evidence_dir: PathBuf,
}

/// Run one [`RouteCandidate`] against the live chain and, if the configured mode allows it,
/// put it on chain.
///
/// The order is fixed by §57 and is not negotiable at the call site: the chain is read, the
/// route is priced, the EVM runs, risk decides, §26's gate reads the chain a second time, and
/// only then is anything signed. Each step writes its own row before the next one is started,
/// so the directory holds the run as far as it got whichever step stopped it.
pub async fn run_once(config: &ArbitrageConfig) -> Result<ArbitrageRun> {
    let clock = Clock::new();
    let expected = single_attested_chain(config)?;

    let adapter = HttpChainAdapter::connect(&config.rpc_url)
        .await
        .map_err(PipelineError::Chain)?;
    let chain_id = adapter.chain_id().0;
    if chain_id != expected {
        return Err(PipelineError::ChainMismatch {
            registry: expected,
            node: chain_id,
            endpoint: config.rpc_url.clone(),
        });
    }
    if config.candidate.chain_id != 0 && config.candidate.chain_id != chain_id {
        return Err(PipelineError::Config(format!(
            "this candidate was named for chain {} and the endpoint answers for chain {chain_id}",
            config.candidate.chain_id
        )));
    }

    // §8's protection is structural rather than a rule to remember: one head read, turned
    // into a pin, and every state read below names that height. Nothing asks the node what
    // "latest" is a second time.
    //
    // M8.1's two nanosecond readings bracket exactly these two calls, and they are the only
    // timestamps this module takes for measurement's sake: §16's Observation stage is "the
    // run first confirmed that this block exists", which here is the head read and the
    // header read it turns into a pin, and nothing else.
    let detected_at = clock.now_ms();
    let detected_ns = clock.now_ns();
    let head = adapter.latest_block().await.map_err(PipelineError::Chain)?;
    let header = adapter
        .get_block_context(head)
        .await
        .map_err(PipelineError::Chain)?;
    let observed_ns = clock.now_ns();
    let session_id = format!("route-{chain_id}-{}-{}", head.0, evm_metrics::unix_ms());
    let dir = config.evidence_dir.join(&session_id);
    let mut evidence = EvidenceWriter::route_run(&dir, &session_id)?;
    let mut run = ArbitrageRun::open(config, session_id, dir, chain_id, head, header.clone());
    let mut metrics = Metrics::default();
    let mut latencies = serde_json::Map::new();
    let mut trace = RunTrace::open(config, clock, chain_id, head.0, run.session_id.clone())?;
    trace.recorder.begin_at(Stage::Observation, detected_ns);
    trace.recorder.end_at(Stage::Observation, observed_ns);
    // §42.3's mapping, stated in the evidence rather than only in prose: a single-route run
    // has no incremental state engine and no graph to rebuild. It reads the two venues from
    // the node inside detection, so those two stages of the model are recorded as not
    // reached here rather than borrowed from a pipeline that did not run.
    trace.recorder.skip(
        Stage::StateUpdate,
        "a route run applies no state updates: it reads both venues from the node at the pin, \
         inside detection",
    );
    trace.recorder.skip(
        Stage::GraphUpdate,
        "a route run builds no graph: the pair's orientation is decided from the reserves read \
         at the pin, inside detection",
    );
    trace
        .recorder
        .begin_at(Stage::OpportunityDetection, observed_ns);

    // ---- §A: the route, from the chain's own state at the pin ----------------------------
    let legs = match price_legs(&adapter, ChainId(chain_id), head, config).await {
        Ok(legs) => legs,
        Err(detail) => {
            // §34's shape, produced by a real run: the stage that stopped the lifecycle is
            // recorded as having run and failed, and everything after it as never reached.
            trace.failed(Stage::OpportunityDetection, &detail);
            run.refuse("opportunity", detail);
            return Ok(finish(
                run,
                &mut evidence,
                metrics,
                Value::Object(latencies),
                &mut trace,
            ));
        }
    };
    let detected_ms = clock.now_ms().saturating_sub(detected_at);
    latencies.insert(
        "opportunity_detection_latency_ms".to_string(),
        json!(detected_ms),
    );
    // §4's identity rule: the trace carries the finding's own stable id, the same string
    // [`SequencePlan`] and `executions.jsonl` carry, instead of a second one to disagree
    // with. It is computed here rather than at §C because it is a pure function of the
    // legs, so naming the opportunity earlier changes no value anyone reads.
    let opportunity_id = route_id(chain_id, head, &legs, config.candidate.input_amount);
    trace.name_opportunity(&opportunity_id);
    trace.recorder.end(Stage::OpportunityDetection);
    metrics.bump("opportunity_count");
    metrics.record_latency("opportunity_detection_latency", detected_ms);
    evidence.line(
        EvidenceFile::Opportunities,
        &opportunity_row(head, &legs, config, &run.session_id),
    )?;

    // ---- §B: the six steps, in REVM, against the node's state at that pin ---------------
    let chain: Arc<dyn ChainAdapter> = Arc::new(adapter.clone());
    let provider = Arc::new(RpcStateProvider::new(
        chain,
        BlockPin::new(head, header.hash),
    ));
    let state_source = provider.source();
    let request = match simulation_request(&header, &legs, config, &state_source) {
        Ok(request) => request,
        Err(detail) => {
            run.legs = Some(legs);
            trace.not_reached(Stage::Simulation, &detail);
            run.refuse("simulation", detail);
            return Ok(finish(
                run,
                &mut evidence,
                metrics,
                Value::Object(latencies),
                &mut trace,
            ));
        }
    };
    // §1 and §34 together, checked before the EVM is entered rather than after it has
    // answered: an empty setup vector is the proof that this run manufactured nothing, and a
    // non-empty one is a route that exists because the run arranged it.
    let setup = match request.preflight() {
        Ok(setup) => setup,
        Err(error) => {
            run.legs = Some(legs);
            let detail = format!("the request refused itself before running: {error}");
            trace.not_reached(Stage::Simulation, &detail);
            run.refuse("simulation", detail);
            return Ok(finish(
                run,
                &mut evidence,
                metrics,
                Value::Object(latencies),
                &mut trace,
            ));
        }
    };
    if !setup.is_empty() {
        run.legs = Some(legs);
        let detail = format!(
            "this route would have needed {} manufactured state entr{} to execute at all \
             ({}); §1 forbids a profit that exists because the run arranged it",
            setup.len(),
            if setup.len() == 1 { "y" } else { "ies" },
            setup
                .iter()
                .map(|entry| entry.reason.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        );
        // The EVM was never entered, so Simulation is a stage this lifecycle did not reach
        // rather than one that ran slowly — §9's distinction, in the branch that earns it.
        trace.not_reached(Stage::Simulation, &detail);
        run.refuse("simulation", detail);
        return Ok(finish(
            run,
            &mut evidence,
            metrics,
            Value::Object(latencies),
            &mut trace,
        ));
    }

    let simulated_at = clock.now_ms();
    trace.recorder.begin(Stage::Simulation);
    let provider: Arc<dyn StateProvider> = provider;
    let simulation = match simulate(provider, &request).await {
        Ok(simulation) => simulation,
        Err(error) => {
            run.legs = Some(legs);
            let detail = format!("the simulation produced no answer: {error}");
            // §16: an error terminates the trace as a stage that ran and did not answer.
            // A revert is not this branch — the EVM answered with a failure there, so that
            // run completes Simulation and lets Risk say no.
            trace.failed(Stage::Simulation, &detail);
            run.refuse("simulation", detail);
            return Ok(finish(
                run,
                &mut evidence,
                metrics,
                Value::Object(latencies),
                &mut trace,
            ));
        }
    };
    trace.recorder.end(Stage::Simulation);
    // §11: this span asked a node for the state it computed on — `state_source` is the
    // provider's own name for itself, the same string that goes into the run's
    // simulation evidence — so the row says which cost class its time belongs to rather
    // than letting the stage's name decide. Nothing is read here to make that decision;
    // the run already carried the fact.
    trace
        .recorder
        .classify_reads(Stage::Simulation, &state_source);
    let simulation_ms = clock.now_ms().saturating_sub(simulated_at);
    latencies.insert("simulation_latency_ms".to_string(), json!(simulation_ms));
    metrics.bump("simulation_count");
    metrics.record_latency("simulation_latency", simulation_ms);
    evidence.line(
        EvidenceFile::SimulationResults,
        &simulation_row(&simulation, &run.session_id),
    )?;

    // ---- §C: risk decides, and its decision is not this module's to overrule ------------
    let thresholds = config.risk.thresholds(&header);
    // §20: Risk is its own stage, never folded into the simulation's span. Its span is the
    // decision alone; the rows written after it land in the hop toward Preflight, which is
    // where that waiting actually is.
    trace.recorder.begin(Stage::Risk);
    let decision = thresholds.evaluate(&simulation);
    trace.recorder.end(Stage::Risk);
    metrics.bump(if decision.accepted() {
        "risk_accept_count"
    } else {
        "risk_reject_count"
    });
    let risk_line = decision.to_string();
    evidence.line(
        EvidenceFile::RiskDecisions,
        &json!({
            "session_id": run.session_id,
            "decision": risk_line,
            "thresholds": thresholds,
            "provenance": config.risk.provenance(&header),
            "no_broadcast": NO_BROADCAST,
        }),
    )?;
    if !decision.accepted() {
        run.legs = Some(legs);
        run.simulation = Some(simulation);
        run.decision = Some(decision);
        // §34's Test 3, produced by a real decline rather than asserted about one:
        // Simulation Completed, Risk Completed with its answer, and everything from
        // Preflight onward recorded as never reached.
        trace.declined(Stage::Risk, &risk_line);
        run.refuse(
            "risk",
            format!(
                "the risk layer declined this route ({risk_line}); its thresholds are its own \
                 and nothing downstream may overrule them"
            ),
        );
        return Ok(finish(
            run,
            &mut evidence,
            metrics,
            Value::Object(latencies),
            &mut trace,
        ));
    }

    // ---- §16's shape, taken out of the run rather than described to it ------------------
    let state_fingerprint = format!("pinned-block-{}-{}", head.0, header.hash);
    // §18's expected side, read as the engine states it: what the route *credited* the
    // sender, in the input token — a difference of two balances, not a closing balance, so a
    // wallet that already held the asset cannot read as a profit the pools paid.
    let priced_output_wei = simulation.compared.simulated.unwrap_or(legs.out_demand);
    let attempt = GateAttempt::Arbitrage {
        simulation_success: simulation.success(),
        risk_accepted: decision.accepted(),
        freshness: Freshness::Active,
    };
    let plan = match SequencePlan::from_run(
        &simulation,
        &decision,
        &opportunity_id,
        &state_fingerprint,
        SenderFunding::RealState {
            source: state_source.clone(),
        },
        config.market.clone(),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            run.legs = Some(legs);
            run.simulation = Some(simulation);
            run.decision = Some(decision);
            let detail = format!("the accepted run yielded no executable sequence: {error}");
            // Risk accepted; the shape it accepted turned out not to be executable, so
            // Preflight and everything after it were never reached.
            trace.declined(Stage::Risk, &detail);
            run.refuse("plan", detail);
            return Ok(finish(
                run,
                &mut evidence,
                metrics,
                Value::Object(latencies),
                &mut trace,
            ));
        }
    };

    // ---- §D: §26's checks, read fresh over the endpoint that will send ------------------
    // §21's gate, measured as the interval the lane was actually asked to cover: connecting
    // it and gathering its reads are one span here, because M7 meters no separate "open the
    // lane" instant and §15 forbids inventing one to split them.
    trace.recorder.begin(Stage::Preflight);
    // The two failures below end the run with an error rather than with a recorded verdict,
    // and §49 still asks for the session's evidence: `execution_failed` closes and writes the
    // trace on the way out, so a lane that could not be opened leaves a trace saying so instead
    // of a directory with nothing in it.
    let mut stage = match SequenceStage::connect(
        &config.rpc_url,
        chain_id,
        config.setup.clone(),
        clock,
        config.tolerance,
    )
    .await
    {
        Ok(stage) => stage,
        Err(error) => return Err(trace.execution_failed(&run.session_id, Stage::Preflight, error)),
    };
    let gathered = match (LivePreflightReads {
        market: &adapter,
        abilities: stage.abilities(),
        setup: stage.setup(),
        plan: &plan,
        route: &legs.route,
        priced_output_wei,
        fee_evidence: config.candidate.fee_evidence.clone(),
        attempt: attempt.clone(),
        clock,
    })
    .gather(&mut metrics)
    .await
    {
        Ok(gathered) => gathered,
        Err(error) => return Err(trace.execution_failed(&run.session_id, Stage::Preflight, error)),
    };
    trace.recorder.end(Stage::Preflight);
    // §21 also asks for the gate's *verdict* beside its duration. That verdict is already
    // in `preflight.json` and in the report's refusal; the trace carries the duration, and no
    // new check is run to fill a field (§21's rule, restated in M8.1).
    let preflight = gathered.to_json();
    evidence.write_whole("preflight.json", &preflight)?;
    // §53's `preflight_latency` under the name the task gives it, from the one span the
    // gatherer measured; the lane's own spelling travels in the same metrics file.
    latencies.insert(
        "preflight_latency_ms".to_string(),
        json!(gathered.latency_ms),
    );

    // A failed verdict is not an early return: [`SequenceStage::run`] refuses the attempt
    // itself, stamps the ladder at the rung it reached, and hands back a full report — which
    // is what §D asks to see (a gate that stopped the run is evidence that the gate ran).
    let report = stage
        .run(
            &plan,
            &attempt,
            Some(&gathered.report),
            &gathered.before,
            &mut metrics,
        )
        .await;
    route_latencies(stage.ledger(), &report, &mut metrics, &mut latencies);
    // §21–§24's execution half, read off the rung stamps the lane wrote for its own reasons
    // (§15: no second query, no second clock). Where the ladder stopped is part of what the
    // reader gets — a run that built and never signed has a skipped Sign, not a missing one —
    // so it is folded in here rather than at the first sign of a submission.
    record_ladder(
        &mut trace.recorder,
        ladder_record(stage.ledger(), &report),
        stage.mode(),
        &report.detail,
    );
    record_route_metrics(&report, &mut metrics);
    evidence.line(EvidenceFile::Executions, &report.to_json())?;
    // Two rows, two types: the signed envelope (§52) and the endpoint's answer (§53) are
    // different records, and every step that reached a signature has both.
    for step in &report.transactions {
        let signed = serde_json::to_value(&step.signed).map_err(serialization_failed(
            EvidenceFile::SignedTransactions.file_name(),
        ))?;
        evidence.line(EvidenceFile::SignedTransactions, &signed)?;
        let submission = serde_json::to_value(&step.submission)
            .map_err(serialization_failed(EvidenceFile::Submissions.file_name()))?;
        evidence.line(EvidenceFile::Submissions, &submission)?;
    }
    run.legs = Some(legs);
    run.simulation = Some(simulation);
    run.decision = Some(decision);
    run.plan = Some(plan);
    run.preflight = Some(preflight);
    run.report = Some(report);
    Ok(finish(
        run,
        &mut evidence,
        metrics,
        Value::Object(latencies),
        &mut trace,
    ))
}

/// M8.1's record of one route run's lifecycle, and the file it belongs in.
///
/// This is the whole of the instrumentation's footprint: a [`TraceRecorder`] that makes one
/// map write per stage boundary the run already passes, and — only when `--latency-output`
/// named a directory — an open `traces.jsonl` to write at the end. When no directory was
/// named, the recorder holds no trace, every call below becomes a no-op *including the clock
/// reading*, and the run takes the code path it took before this type existed (§40's backward
/// compatibility, §38's "the cost is a timestamp and a store").
///
/// Two rules bind here that bind nowhere else in the file:
///
/// - **A stop is written down, not left blank.** Whichever way the run ends, the stages after
///   it get a `skipped` record carrying the reason, so a reader of `traces.jsonl` can tell
///   "not attempted" from "attempted and not measured" (§9). `declined` is the exception: the
///   stage that answered is already closed, and only what follows is skipped.
/// - **The trace must never be the reason a run's verdict is lost.** Every write is either
///   folded into the run's own [`ArbitrageRun::refuse`] by [`finish`] or returned from
///   [`RunTrace::execution_failed`] beside the error that was already leaving.
struct RunTrace {
    recorder: TraceRecorder,
    /// The same clock the run meters with, kept so the recorder can be handed over at the
    /// end: `TraceRecorder::finish` consumes its recorder, and a replacement needs a clock.
    clock: Clock,
    evidence: Option<LatencyEvidence>,
}

impl RunTrace {
    /// Open the run's trace. `Live` because this lifecycle reads a node and, if it gets that
    /// far, signs on chain — even a run whose market is labelled `CONTROLLED_FIXTURE` spends
    /// real network time, and §45's separation is about which latencies get averaged together,
    /// not about who was called. A fixture or replay trace is built by the code that replays
    /// recorded evidence, not here.
    fn open(
        config: &ArbitrageConfig,
        clock: Clock,
        chain_id: u64,
        head: u64,
        session_id: String,
    ) -> Result<Self> {
        let Some(base) = config.latency_dir.clone() else {
            return Ok(Self {
                recorder: TraceRecorder::off(clock),
                clock,
                evidence: None,
            });
        };
        // One subdirectory per session: §44 forbids rewriting a baseline that is already on
        // disk, and a name that carries the session id is also how a trace line is traced
        // back to the `route-run.json` that holds its verdict.
        let evidence = LatencyEvidence::open(
            &base.join(&session_id),
            git_revision(),
            config.setup.mode.name(),
        )?;
        let trace = LatencyTrace::new(TraceSource::Live, chain_id, Some(head), None);
        Ok(Self {
            recorder: TraceRecorder::on(clock, trace),
            clock,
            evidence: Some(evidence),
        })
    }

    /// §4's identity rule: the trace carries the finding's own stable id rather than a
    /// second one to disagree with, which also re-derives `trace_id` from it.
    fn name_opportunity(&mut self, opportunity_id: &str) {
        if let Some(trace) = self.recorder.trace_mut() {
            trace.set_opportunity(opportunity_id);
        }
    }

    /// A stage that ran and did not answer, and everything after it.
    fn failed(&mut self, stage: Stage, detail: &str) {
        let note = format!("never attempted: {detail}");
        self.recorder.fail(stage, detail);
        self.recorder.skip_after(stage, &note);
    }

    /// A stage the lifecycle never entered, and everything after it.
    fn not_reached(&mut self, stage: Stage, detail: &str) {
        let note = format!("never attempted: {detail}");
        self.recorder.skip(stage, &note);
        self.recorder.skip_after(stage, &note);
    }

    /// The lifecycle stopped *because a stage answered* — Risk declining, a plan that would
    /// not build. That stage's record is already the answer, so only what follows it is a
    /// skip; writing it again would be refused and would put a wiring bug in the trace file.
    fn declined(&mut self, stage: Stage, detail: &str) {
        let note = format!("never attempted: {detail}");
        self.recorder.skip_after(stage, &note);
    }

    /// The lane failed in a way that ends the run with an error rather than with a refusal.
    /// §49 still asks for this session's evidence, so the trace is closed and written on the
    /// way out and the write's own failure is folded into the error rather than replacing it.
    fn execution_failed<E: std::fmt::Display>(
        &mut self,
        session_id: &str,
        stage: Stage,
        error: E,
    ) -> PipelineError {
        let detail = error.to_string();
        self.failed(stage, &detail);
        match self.finish(session_id) {
            Ok(()) => PipelineError::Execution(detail),
            Err(write) => PipelineError::Execution(format!(
                "{detail} | the latency trace could not be written either: {write}"
            )),
        }
    }

    /// Close the lifecycle and write its line, tables and README. Idempotent: a recorder
    /// already handed over holds nothing.
    fn finish(&mut self, session_id: &str) -> Result<()> {
        let Some(evidence) = self.evidence.as_mut() else {
            return Ok(());
        };
        let recorder = std::mem::replace(&mut self.recorder, TraceRecorder::off(self.clock));
        let recorded = recorder.finish().with_session(session_id);
        evidence.record(recorded)?;
        evidence.finish()?;
        Ok(())
    }
}

/// The one ladder row this attempt climbed, if it became a record at all.
///
/// §55's binding already happened inside the lane — a record is in the ledger because its
/// hash was read off the submission — so this is a lookup by the id the report names, not a
/// second claim about which transaction was which.
fn ladder_record<'a>(ledger: &'a Ledger, report: &SequenceReport) -> Option<&'a ExecutionRecord> {
    report.execution_id.as_deref().and_then(|id| ledger.get(id))
}

impl ArbitrageRun {
    fn open(
        config: &ArbitrageConfig,
        session_id: String,
        evidence_dir: PathBuf,
        chain_id: u64,
        head: BlockNumber,
        header: BlockContext,
    ) -> Self {
        Self {
            session_id,
            chain_id,
            head,
            mode: config.setup.mode,
            header,
            market: config.market.clone(),
            sender: config.sender,
            candidate: config.candidate.clone(),
            legs: None,
            simulation: None,
            decision: None,
            plan: None,
            preflight: None,
            report: None,
            refusal: None,
            metrics: Value::Null,
            latency_ms: Value::Null,
            evidence_dir,
        }
    }

    fn refuse(&mut self, stage: &'static str, detail: String) {
        self.refusal = Some(Refusal { stage, detail });
    }

    /// §57's one line: what the route was, how far it got, and what it made.
    pub fn line(&self) -> String {
        let simulated = match &self.simulation {
            Some(run) if run.status.completed() => "completed",
            Some(_) => "did-not-complete",
            None => "not-run",
        };
        let outcome = match (&self.report, &self.refusal) {
            (Some(report), _) => report.line(),
            (None, Some(refusal)) => format!("stopped at {}: {}", refusal.stage, refusal.detail),
            (None, None) => "stopped before the execution lane".to_string(),
        };
        format!(
            "session={} chain={} head={} mode={} market={} simulated={} risk={} — {}",
            self.session_id,
            self.chain_id,
            self.head.0,
            self.mode.name(),
            self.market.name(),
            simulated,
            match &self.decision {
                Some(RiskDecision::Accept { .. }) => "accept",
                Some(_) => "reject",
                None => "not-reached",
            },
            outcome,
        )
    }

    /// `0` only for a route the market offered, the chain settled, and the evidence verified
    /// positive (§56's single counting rule); anything else exits non-zero.
    pub fn exit_code(&self) -> i32 {
        if self.counts_as_successful_real_arbitrage() {
            0
        } else {
            1
        }
    }

    pub fn counts_as_successful_real_arbitrage(&self) -> bool {
        self.report
            .as_ref()
            .is_some_and(SequenceReport::counts_as_successful_real_arbitrage)
    }

    /// §57's P: one file a reader can recompute the verdict from, with §52's four amount lines
    /// stated beside the denomination that makes them comparable. Public because `--json`
    /// prints exactly this value: the record a run writes and the record a caller reads off
    /// the terminal must be one rendering, not two that can disagree.
    pub fn record(&self) -> Value {
        let profit = self
            .report
            .as_ref()
            .and_then(|report| report.profit.as_ref());
        let costs = self.report.as_ref().and_then(|report| report.cost.as_ref());
        json!({
            "session_id": self.session_id,
            "milestone": "M7",
            "chain_id": self.chain_id,
            "pinned_block": self.head.0,
            "pinned_block_hash": format!("{:?}", self.header.hash),
            "mode": self.mode.name(),
            "market": self.market.to_json(),
            "sender": format!("{:#x}", self.sender),
            "candidate": {
                "input_token": format!("{:#x}", self.candidate.input_token),
                "mid_token": format!("{:#x}", self.candidate.mid_token),
                "venues": self.candidate.venues.iter().map(|venue| format!("{venue:#x}")).collect::<Vec<_>>(),
                "input_amount": self.candidate.input_amount.to_string(),
                "fee": format!("{}/{}", self.candidate.fee.numerator, self.candidate.fee.denominator),
                "fee_evidence": self.candidate.fee_evidence,
            },
            "route": self.legs.as_ref().map(|legs| json!({
                "buy": legs.buy.row(),
                "sell": legs.sell.row(),
                "mid_demand": legs.mid_demand.to_string(),
                "out_demand": legs.out_demand.to_string(),
                "priced_by": legs.route.priced_by,
            })),
            "plan": self.plan.as_ref().map(|plan| json!({
                "opportunity_id": plan.opportunity_id,
                "state_fingerprint": plan.state_fingerprint,
                "funding": plan.funding.describe(),
                "provenance": plan.provenance,
                "steps": plan.describe(),
                "tokens": plan.tokens.iter().map(|token| format!("{token:#x}")).collect::<Vec<_>>(),
            })),
            "simulation": self.simulation.as_ref().map(|run| simulation_row(run, "")),
            "risk": self.decision.as_ref().map(|decision| json!({ "decision": decision.to_string() })),
            "preflight": self.preflight,
            "execution": self.report.as_ref().map(|report| report.to_json()),
            "refusal": self.refusal.as_ref().map(|refusal| json!({
                "stage": refusal.stage,
                "detail": refusal.detail,
            })),
            // §52's amount lines, in the profit evidence's own denomination and as the signed
            // decimal that evidence reports — not rounded, not re-derived, not zero-filled.
            "m7_amounts": {
                "denomination": profit.map(|p| p.realized.denomination.describe()).unwrap_or_else(|| "not reached".to_string()),
                "gross_profit": profit.map(|p| p.realized.gross_profit.to_string()).unwrap_or_else(|| "not measured".to_string()),
                "l2_cost": costs.map(|c| c.l2_fee_total.to_string()).unwrap_or_else(|| "not measured".to_string()),
                "l1_cost": costs.map(|c| c.l1_fee_total.to_string()).unwrap_or_else(|| "not measured".to_string()),
                "net_profit": profit.and_then(|p| p.realized.net_profit.map(|net| net.to_string())).unwrap_or_else(|| "not provable in one denomination".to_string()),
                "profit_status": profit.map(|p| p.status.name().to_string()).unwrap_or_else(|| "no profit evidence".to_string()),
                "l1_fee_source": costs.map(|c| c.lines.iter().map(|line| line.l1_fee_source.describe()).collect::<Vec<_>>().join(" | ")).unwrap_or_else(|| "not reached".to_string()),
            },
            "latency_ms": self.latency_ms,
            "counters": self.metrics,
            "successful_real_arbitrage": self.counts_as_successful_real_arbitrage(),
            "evidence_dir": self.evidence_dir.display().to_string(),
        })
    }
}

/// §46's boundary, phrased the way [`crate::runner`] phrases it: a run trades on one chain,
/// so more than one attested id among the directories the caller named is a configuration
/// mistake rather than a choice.
fn single_attested_chain(config: &ArbitrageConfig) -> Result<u64> {
    let ids = attested_chain_ids(&config.registry_dirs)?;
    match ids.as_slice() {
        [one] => Ok(*one),
        other => Err(PipelineError::Config(format!(
            "the attested registries name {} chains {other:?}; a route lives on one, so pass \
             the --registry-dir that names the chain this endpoint is",
            other.len()
        ))),
    }
}

/// Read both venues at the pinned head and price both legs.
///
/// Orientation is a reading, not an argument: the venue that pays more of the mid token for
/// this input under the candidate's own fee is bought through, and the other is sold into.
/// That order is then the order §23's route audit checks the receipts against, so a run that
/// guessed the sides would be caught by its own audit rather than believed by its own profit.
async fn price_legs(
    adapter: &HttpChainAdapter,
    chain: ChainId,
    at: BlockNumber,
    config: &ArbitrageConfig,
) -> std::result::Result<PricedLegs, String> {
    let candidate = &config.candidate;
    let mut sides = Vec::with_capacity(2);
    for venue in candidate.venues {
        let pool = read_pool(adapter, venue, at)
            .await
            .map_err(|error| format!("{venue}: {error}"))?;
        sides.push(Venue {
            pool: venue,
            token0: pool.token0,
            token1: pool.token1,
            reserve0: pool.reserve0,
            reserve1: pool.reserve1,
            last_synced: pool.last_synced,
            read_at_block: pool.read_at_block,
            source: pool.source,
        });
    }
    let quote = |venue: &Venue| {
        venue
            .quote(
                candidate.input_token,
                candidate.mid_token,
                candidate.fee,
                candidate.input_amount,
            )
            .unwrap_or(U256::ZERO)
    };
    // A tie goes to the order the candidate named, which is a convention stated here rather
    // than an accident of `>=`: two equal quotes price no arbitrage and the run will say so.
    let (buy, sell) = if quote(&sides[0]) >= quote(&sides[1]) {
        (sides[0].clone(), sides[1].clone())
    } else {
        (sides[1].clone(), sides[0].clone())
    };

    let mid_demand = buy.quote(
        candidate.input_token,
        candidate.mid_token,
        candidate.fee,
        candidate.input_amount,
    )?;
    if mid_demand.is_zero() {
        return Err(format!(
            "venue {} pays zero of {} for {}, so there is no first leg to send",
            buy.pool, candidate.mid_token, candidate.input_amount
        ));
    }
    let out_demand = sell.quote(
        candidate.mid_token,
        candidate.input_token,
        candidate.fee,
        mid_demand,
    )?;

    let leg =
        |venue: &Venue, from: Address, to: Address| -> std::result::Result<RouteLeg, String> {
            let (reserve_in, reserve_out) = venue.reserves(from, to)?;
            Ok(RouteLeg {
                pool: PoolId::new(chain, venue.pool),
                token_in: TokenId::new(chain, from),
                token_out: TokenId::new(chain, to),
                reserve_in,
                reserve_out,
                fee: candidate.fee,
            })
        };
    let gross = out_demand
        .checked_sub(candidate.input_amount)
        .unwrap_or_else(|| candidate.input_amount.saturating_sub(out_demand));
    let route = PricedRoute::new(
        chain,
        at,
        leg(&buy, candidate.input_token, candidate.mid_token)?,
        leg(&sell, candidate.mid_token, candidate.input_token)?,
        candidate.input_amount,
        mid_demand,
        out_demand,
        gross,
        format!(
            "getReserves(), token0() and token1() read at block {} for {} and {}; the {}/{} fee \
             on both legs is the figure this candidate carries, proven by bisection against \
             these pools' own deployed bytecode — {}",
            at.0,
            candidate.venues[0],
            candidate.venues[1],
            candidate.fee.numerator,
            candidate.fee.denominator,
            candidate.fee_evidence,
        ),
    )
    .map_err(|error| format!("the route is not priceable: {error}"))?;
    Ok(PricedLegs {
        buy,
        sell,
        mid_demand,
        out_demand,
        route,
    })
}

/// The canonical §16 sequence — `deposit()`, buy, sell, `withdraw()` and the two transfers —
/// run against the node's state at the pinned block, spent by the account that will really
/// sign it.
///
/// This crate's two sender declarations are the whole of §34, and the choice is made here
/// rather than downstream: the scaffolded test sender gets a manufactured balance and can
/// never reach a node; [`SimulationSender::from_pinned_state`] gets none, so a route that only
/// works with one fails here, loudly, instead of succeeding in simulation and reverting on
/// chain.
fn simulation_request(
    header: &BlockContext,
    legs: &PricedLegs,
    config: &ArbitrageConfig,
    state_source: &str,
) -> std::result::Result<SimulationRequest, String> {
    let canonical = TransactionSpec::canonical(
        Funding::WrapNative,
        Settle::UnwrapInputToken,
        legs.out_demand,
    );
    let transaction = TransactionSpec {
        sender: SimulationSender::from_pinned_state(config.sender, config.sender_label),
        rules: EvmRules::Prague,
        ..canonical
    };
    SimulationRequest::new(
        header.clone(),
        legs.route.clone(),
        transaction,
        StateSpec::new(state_source.to_string()),
        GasPricing::Eip1559 {
            priority_fee_per_gas: 0,
            provenance: format!(
                "block {}'s own base fee as its header reports it, with no tip: a hypothetical \
                 transaction on an already-sealed block is not competing to be included in it. \
                 Risk ceiling: {}.",
                header.number.0,
                config.risk.provenance(header),
            ),
        },
    )
    .map_err(|error| format!("the request is refused: {error}"))
}

/// A stable, recomputable id for a route this run read off the chain.
///
/// It is not an [`evm_opportunity::OpportunityId`], because no detector produced this finding:
/// a caller named a pair of venues and this file read their reserves. Saying so in the id's
/// shape is the point — a reader can recompute it from the block and the two addresses, and
/// can see it did not come from the graph.
fn route_id(chain_id: u64, head: BlockNumber, legs: &PricedLegs, input: U256) -> String {
    format!(
        "m7-{chain_id}-{}-{}-{}-{input}",
        head.0, legs.buy.pool, legs.sell.pool,
    )
}

/// §53's names, filled from the one ladder this attempt climbed.
///
/// The lane already meters its per-rung spans under the names M6 gave them, and the record it
/// stamps carries the raw rung times. §53 asks for these words with one sample each, so they
/// are computed from the same stamps rather than re-measured: two spellings of one fact, both
/// in `metrics.json`, with the crosswalk stated in the record.
fn route_latencies(
    ledger: &Ledger,
    report: &SequenceReport,
    metrics: &mut Metrics,
    latencies: &mut serde_json::Map<String, Value>,
) {
    let unread = |why: &str, latencies: &mut serde_json::Map<String, Value>| {
        latencies.insert(
            "lane_latencies".to_string(),
            json!(format!("unread: {why}")),
        );
    };
    let Some(id) = report.execution_id.as_deref() else {
        return unread("the attempt never became a record", latencies);
    };
    let Some(record) = ledger.get(id) else {
        return unread("the ledger holds no record for this id", latencies);
    };
    let span = |from: Option<u64>, to: Option<u64>| Some(to?.saturating_sub(from?));
    let sign = span(record.built_at_ms, record.signed_at_ms);
    let submission = span(record.signed_at_ms, record.submitted_at_ms);
    let inclusion = span(record.submitted_at_ms, record.included_at_ms);
    for (name, sample) in [
        ("sign_latency", sign),
        ("submission_latency", submission),
        ("inclusion_latency", inclusion),
    ] {
        if let Some(ms) = sample {
            metrics.record_latency(name, ms);
        }
    }
    for (key, sample) in [
        ("sign_latency_ms", json!(sign)),
        ("submission_latency_ms", json!(submission)),
        ("inclusion_latency_ms", json!(inclusion)),
        ("settled_at_ms", json!(record.settled_at_ms)),
        ("profit_verified_at_ms", json!(record.profit_verified_at_ms)),
        (
            "rung_stamps",
            json!({
                "created": record.created_at_ms,
                "built": record.built_at_ms,
                "signed": record.signed_at_ms,
                "submitted": record.submitted_at_ms,
                "included": record.included_at_ms,
                "settled": record.settled_at_ms,
                "profit_verified": record.profit_verified_at_ms,
            }),
        ),
    ] {
        latencies.insert(key.to_string(), sample);
    }
}

/// §52's counters, counted off the receipts and the profit verdict rather than off the plan:
/// a step that was built and never included is `build_count` and not `included_count`.
fn record_route_metrics(report: &SequenceReport, metrics: &mut Metrics) {
    for step in &report.transactions {
        metrics.bump("build_count");
        metrics.bump("sign_count");
        metrics.bump("submit_count");
        match step.receipt_status.name() {
            "included" => metrics.bump("included_count"),
            "reverted" => metrics.bump("revert_count"),
            other => metrics.bump(&format!("receipt_absent.{other}")),
        }
    }
    let Some(profit) = report.profit.as_ref() else {
        return;
    };
    match profit.status {
        ProfitVerificationStatus::VerifiedPositive => metrics.bump("profitable_count"),
        ProfitVerificationStatus::VerifiedNegative => metrics.bump("unprofitable_count"),
        _ => {}
    }
}

/// Write what a finished run owes the reader — `metrics.json` (§52/§53) and the run's own
/// `route-run.json` (§57's P) — and hand the run back.
///
/// The writes are best-effort in one direction only: an evidence failure is folded into the
/// run's own record of why it stopped, because the verdict a run reached must not be lost by
/// the file that was meant to carry it. Everything else about the run still returns.
fn finish(
    mut run: ArbitrageRun,
    evidence: &mut EvidenceWriter,
    metrics: Metrics,
    latency_ms: Value,
    trace: &mut RunTrace,
) -> ArbitrageRun {
    run.metrics = metrics.to_json();
    run.latency_ms = latency_ms;
    if let Err(error) = evidence.write_whole("metrics.json", &run.metrics) {
        run.absorb(error);
    }
    // The trace is written before `route-run.json` on purpose: if writing it fails, the
    // failure is folded into the run's refusal and the last file a reader opens still says
    // so. The reverse order would leave the trace's own error unrecorded anywhere.
    if let Err(error) = trace.finish(&run.session_id) {
        run.absorb(error);
    }
    match evidence.write_whole(RECORD_FILE, &run.record()) {
        Ok(()) => {}
        Err(error) => run.absorb(error),
    }
    run
}

impl ArbitrageRun {
    /// Fold an evidence-file failure into this run's own account, keeping any refusal that
    /// already explains why the run stopped.
    fn absorb(&mut self, error: PipelineError) {
        let detail = error.to_string();
        match &mut self.refusal {
            Some(refusal) => refusal.detail = format!("{} | {detail}", refusal.detail),
            None => {
                self.refusal = Some(Refusal {
                    stage: "evidence",
                    detail,
                })
            }
        }
    }
}

/// The one failure mode of writing a row: a type that will not serialize. Named by the file
/// it was headed for, because §52's envelope and §53's endpoint answer are different records
/// and a reader has to be told which one is broken.
fn serialization_failed(file: &'static str) -> impl Fn(serde_json::Error) -> PipelineError {
    move |error| PipelineError::Evidence {
        path: PathBuf::from(file),
        detail: format!("a {file} row does not serialize: {error}"),
    }
}

fn opportunity_row(
    head: BlockNumber,
    legs: &PricedLegs,
    config: &ArbitrageConfig,
    session_id: &str,
) -> Value {
    json!({
        "session_id": session_id,
        "observed_block": head.0,
        "found_by": "a named candidate read at one pinned live block; no detector ran, and §27 \
                     forbids this run searching for another",
        "input_amount": config.candidate.input_amount.to_string(),
        "input_token": format!("{:#x}", config.candidate.input_token),
        "mid_token": format!("{:#x}", config.candidate.mid_token),
        "fee": format!("{}/{}", config.candidate.fee.numerator, config.candidate.fee.denominator),
        "fee_evidence": config.candidate.fee_evidence,
        "market": config.market.name(),
        "buy": legs.buy.row(),
        "sell": legs.sell.row(),
        "mid_demand": legs.mid_demand.to_string(),
        "out_demand": legs.out_demand.to_string(),
        "priced_by": legs.route.priced_by,
    })
}

/// The run's own §41 row. `session_id` is empty for a record that already names its session
/// one level up.
fn simulation_row(run: &SimulationResult, session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "status": format!("{:?}", run.status),
        "completed": run.status.completed(),
        "steps": run.steps.len(),
        "gas_used": run.gas_used(),
        "chain_id": run.chain_id.0,
        "block": run.block.number.0,
        "block_hash": format!("{:?}", run.block.hash),
        "state_source": run.state_source,
        "sender": format!("{:#x}", run.sender),
        "net_profit": format!("{:?}", run.net_profit),
        "compared": run.compared,
        "outcome": run.outcome,
        "slippage": run.slippage,
        "fingerprint": format!("{:x}", run.fingerprint()),
        "plan": run.plan_summary,
    })
}

/// §34's T2–T4 at the seam a route run actually uses: the same [`RunTrace`] methods
/// [`run_once`] calls on its way out, driven to each stop without a node.
///
/// What this proves is the *shape* each refusal leaves in `traces.jsonl` — which stage
/// holds a duration, which holds a reason, and that nothing a run never attempted is
/// ever filed as an attempt that took no time. What it deliberately does not prove is
/// that the live chain behaves this way: that is §31's observation run and its evidence
/// directory, and no test here stands in for it.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::latency::{README_FILE, SUMMARY_FILE, TRACES_FILE};
    use evm_metrics::{StageOutcome, StageRecord};
    use std::path::Path;

    const CHAIN: u64 = 91_342;
    const HEAD: u64 = 37_503_978;
    const ID: &str = "m7-91342-37503978-buy-sell-1";
    const SESSION: &str = "unit-session";

    fn config(latency_dir: Option<PathBuf>) -> ArbitrageConfig {
        ArbitrageConfig {
            rpc_url: String::new(),
            registry_dirs: Vec::new(),
            sender: Address::from_slice(&[0x11u8; 20]),
            sender_label: "the operator's funded test wallet",
            candidate: RouteCandidate {
                chain_id: CHAIN,
                input_token: Address::from_slice(&[0x11u8; 20]),
                mid_token: Address::from_slice(&[0x22u8; 20]),
                venues: [
                    Address::from_slice(&[0x33u8; 20]),
                    Address::from_slice(&[0x44u8; 20]),
                ],
                input_amount: U256::from(1u64),
                fee: Fee::new(3, 1000).expect("a fee of 3/1000 exists"),
                fee_evidence: "fixtures/live-m5/arbitrage-window".to_string(),
            },
            market: MarketKind::ControlledFixture {
                proves: "a test of the trace's shape".to_string(),
            },
            setup: ExecutionSetup::default(),
            tolerance: Tolerance::new(1, 100),
            risk: RiskConfig {
                minimum_net_profit_wei: 0,
                maximum_gas: None,
            },
            evidence_dir: PathBuf::from("unused-by-these-tests"),
            latency_dir,
        }
    }

    /// The three facts every run in this module starts from, in the order
    /// [`run_once`] establishes them: the block was confirmed, a route run applies
    /// no state updates and builds no graph, and the finding is priced.
    fn bracketed(mut trace: RunTrace) -> RunTrace {
        let at = trace.recorder.now_ns();
        trace.recorder.begin_at(Stage::Observation, at);
        trace.recorder.end_at(Stage::Observation, at + 1_000);
        trace
            .recorder
            .skip(Stage::StateUpdate, "a route run applies no state updates");
        trace
            .recorder
            .skip(Stage::GraphUpdate, "a route run builds no graph");
        trace
            .recorder
            .begin_at(Stage::OpportunityDetection, at + 1_000);
        trace.name_opportunity(ID);
        trace
    }

    /// A trace that records in memory. These tests ask which stage a refusal leaves
    /// timed; the file that shape lands in is `a_written_trace_...`'s question, and a
    /// shape test that wrote to disk would leave a directory behind for no reason.
    fn traced() -> RunTrace {
        let clock = Clock::new();
        let recorder = TraceRecorder::on(
            clock,
            LatencyTrace::new(TraceSource::Live, CHAIN, Some(HEAD), None),
        );
        bracketed(RunTrace {
            recorder,
            clock,
            evidence: None,
        })
    }

    /// The same trace opened the way a run with `--latency-trace` opens it: an
    /// evidence writer exists, so `finish` has somewhere to write.
    fn opened(dir: &Path) -> RunTrace {
        let trace = RunTrace::open(
            &config(Some(dir.to_path_buf())),
            Clock::new(),
            CHAIN,
            HEAD,
            SESSION.to_string(),
        )
        .expect("opening a trace creates its session directory");
        bracketed(trace)
    }

    fn record_of(trace: &RunTrace, stage: Stage) -> StageRecord {
        trace
            .recorder
            .trace()
            .and_then(|held| held.stage(stage))
            .unwrap_or_else(|| panic!("{} got no record at all", stage))
            .clone()
    }

    /// The stages after `last`, as a list rather than through `Stage::ALL`, so a test
    /// that means "the execution half" says exactly that.
    fn after(last: Stage) -> Vec<Stage> {
        Stage::ALL
            .into_iter()
            .filter(|stage| *stage > last)
            .collect()
    }

    /// §34's T2, at the stage a route run can fail it at: the pair was read and priced
    /// and the pricing said no, so detection ran and did not answer and nothing after it
    /// was entered.
    #[test]
    fn a_route_that_yielded_nothing_fails_detection_and_skips_the_rest() {
        let mut trace = traced();
        trace.failed(Stage::OpportunityDetection, "the two venues agree on price");
        let detection = record_of(&trace, Stage::OpportunityDetection);
        assert_eq!(detection.outcome, StageOutcome::Failed);
        assert!(
            detection.duration_ns.is_some(),
            "detection ran, so its span is a fact"
        );
        for stage in after(Stage::OpportunityDetection) {
            let record = record_of(&trace, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(record.duration_ns, None, "{stage} must not read as 0 ns");
        }
    }

    /// §34's T2 as the document states it: the EVM ran and answered no. Simulation
    /// keeps its duration and Risk onward is a skip — which is the difference §9 draws
    /// between "it got there in no time" and "it never got there".
    #[test]
    fn a_simulated_revert_skips_risk_onward_without_a_duration() {
        let mut trace = traced();
        trace.recorder.end(Stage::OpportunityDetection);
        trace.recorder.begin(Stage::Simulation);
        trace.recorder.end(Stage::Simulation);
        trace.declined(Stage::Simulation, "the six steps reverted");
        let simulation = record_of(&trace, Stage::Simulation);
        assert_eq!(simulation.outcome, StageOutcome::Completed);
        assert!(simulation.duration_ns.is_some(), "the EVM was run");
        for stage in after(Stage::Simulation) {
            let record = record_of(&trace, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(record.duration_ns, None, "{stage} must not read as 0 ns");
        }
    }

    /// A stage the EVM was never asked about is `Skipped`, not `Failed`: §9 keeps those
    /// two apart, and this is the branch that earns the difference.
    #[test]
    fn a_simulation_this_run_never_entered_is_skipped_not_failed() {
        let mut trace = traced();
        trace.recorder.end(Stage::OpportunityDetection);
        trace.not_reached(
            Stage::Simulation,
            "the request refused itself before running",
        );
        assert_eq!(
            record_of(&trace, Stage::OpportunityDetection).outcome,
            StageOutcome::Completed
        );
        let simulation = record_of(&trace, Stage::Simulation);
        assert_eq!(simulation.outcome, StageOutcome::Skipped);
        assert_eq!(simulation.started_ns, None);
        assert_eq!(
            record_of(&trace, Stage::ProfitVerification).outcome,
            StageOutcome::Skipped
        );
    }

    /// §34's T3: Simulation and Risk both answered and Risk's answer was no. The two
    /// stages that spoke stay timed; only what follows them is a skip.
    #[test]
    fn a_risk_decline_keeps_its_two_answers_and_skips_the_gate_onward() {
        let mut trace = traced();
        for stage in [Stage::OpportunityDetection, Stage::Simulation, Stage::Risk] {
            if stage != Stage::OpportunityDetection {
                trace.recorder.begin(stage);
            }
            trace.recorder.end(stage);
        }
        trace.declined(Stage::Risk, "the risk layer declined this route");
        for stage in [
            Stage::Observation,
            Stage::OpportunityDetection,
            Stage::Simulation,
            Stage::Risk,
        ] {
            let record = record_of(&trace, stage);
            assert_eq!(record.outcome, StageOutcome::Completed, "{stage}");
            assert!(record.duration_ns.is_some(), "{stage} was timed");
        }
        for stage in after(Stage::Risk) {
            let record = record_of(&trace, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(record.duration_ns, None, "{stage} must not read as 0 ns");
        }
    }

    /// §34's T4, from the gate itself: the attempt was declined there, so the execution
    /// half never began — and each empty stage says why in its own row.
    #[test]
    fn a_declined_gate_skips_the_execution_half_with_a_reason_each() {
        let mut trace = traced();
        for stage in [Stage::OpportunityDetection, Stage::Simulation, Stage::Risk] {
            if stage != Stage::OpportunityDetection {
                trace.recorder.begin(stage);
            }
            trace.recorder.end(stage);
        }
        trace.recorder.begin(Stage::Preflight);
        trace.declined(Stage::Preflight, "§26's checks did not pass");
        assert_eq!(
            record_of(&trace, Stage::Preflight).outcome,
            StageOutcome::Started,
            "the gate was opened and this is where the run ends"
        );
        for stage in after(Stage::Preflight) {
            let record = record_of(&trace, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert!(record.note.is_some(), "{stage} must say why it is empty");
        }
    }

    /// §40's compatibility claim at the level of the filesystem: a run that was never
    /// asked to measure holds no trace, records nothing, and its end writes no
    /// directory.
    #[test]
    fn a_run_that_measures_nothing_writes_nothing() {
        let base = std::env::temp_dir().join(format!(
            "evm-m8-latency-must-not-exist-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let mut off = RunTrace::open(
            &config(None),
            Clock::new(),
            CHAIN,
            HEAD,
            SESSION.to_string(),
        )
        .expect("a run that measures nothing opens nothing");
        assert!(!off.recorder.is_on());
        assert!(off.evidence.is_none());
        assert!(off.recorder.trace().is_none());
        off.recorder.begin(Stage::Observation);
        off.recorder.end(Stage::Observation);
        off.declined(Stage::Observation, "nothing");
        off.finish(SESSION).expect("nothing to write");
        assert!(
            !base.join(SESSION).exists(),
            "a disabled trace must not create a directory"
        );
    }

    /// §46's guarantees, checked in the one file they land in: the line carries the
    /// finding's identity rather than a second one, carries the session that wrote it,
    /// and always holds fourteen stage rows whatever the run reached.
    #[test]
    fn a_written_trace_is_one_line_of_fourteen_stages_and_a_name() {
        let base = std::env::temp_dir().join(format!("evm-m8-latency-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mut trace = opened(&base);
        let trace_id = trace
            .recorder
            .trace()
            .map(|held| held.trace_id().to_string())
            .expect("a trace");
        trace.recorder.end(Stage::OpportunityDetection);
        trace.declined(Stage::OpportunityDetection, "nothing to run");
        trace.finish(SESSION).expect("the trace writes");
        let dir = base.join(SESSION);
        let text = std::fs::read_to_string(dir.join(TRACES_FILE)).expect("traces.jsonl");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);
        let line: Value = serde_json::from_str(lines[0]).expect("one JSON line");
        assert!(trace_id.starts_with("lat-"), "{trace_id}");
        assert_eq!(line["trace_id"], json!(trace_id));
        assert_eq!(line["source"], json!("live"));
        assert_eq!(line["chain_id"], json!(CHAIN));
        assert_eq!(line["opportunity_block"], json!(HEAD));
        assert_eq!(line["opportunity_id"], json!(ID));
        assert_eq!(line["session_id"], json!(SESSION));
        assert_eq!(line["instrumentation_refusals"], json!([]));
        assert_eq!(
            line["stages"].as_array().map(Vec::len),
            Some(Stage::ALL.len()),
            "the shape must not depend on how far the run got"
        );
        // §25's other two files, and §44's metadata in the middle one.
        assert!(dir.join(SUMMARY_FILE).is_file());
        assert!(dir.join(README_FILE).is_file());
        let summary: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(SUMMARY_FILE)).expect("summary.json"),
        )
        .expect("valid JSON");
        assert_eq!(summary["sample_count"], json!(1));
        assert_eq!(summary["sources"][0]["chain_id"], json!(CHAIN));
        assert_eq!(
            summary["execution_mode"],
            json!(config(None).setup.mode.name())
        );
        assert_eq!(summary["git_revision"], json!(git_revision()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// §4's identity rule, as a key: the trace is named for the opportunity it runs, so
    /// two runs of the same route agree on `trace_id` and a run of a different one does
    /// not.
    #[test]
    fn the_trace_is_keyed_on_the_opportunity_it_runs() {
        let named = traced();
        let held = named.recorder.trace().expect("a trace");
        assert_eq!(held.opportunity_id(), Some(ID));
        assert_eq!(
            held.trace_id(),
            traced().recorder.trace().expect("a trace").trace_id()
        );
        // A trace that never got a name keys on the block alone, and that is a different
        // id: naming the finding must re-key, or the file would hold two lifecycles of
        // two routes under one key.
        let unnamed = LatencyTrace::new(TraceSource::Live, CHAIN, Some(HEAD), None);
        assert_ne!(unnamed.trace_id(), held.trace_id());
        let mut again = traced();
        again.name_opportunity("a second finding");
        assert_ne!(
            again.recorder.trace().expect("a trace").trace_id(),
            held.trace_id()
        );
    }
}
