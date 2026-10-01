//! §50's historical gate: the one real block this workspace has — WETH/TTAX across two
//! pools at block 37 191 169 — asked of the execution layer.
//!
//! M4 settled what this block is worth: the detector's gross profit does not survive
//! execution. The analytical ask reverts with the pool's own reason, and the smallest
//! ask completes and comes back short *before* gas, so no threshold turns either rung
//! into an Accept (`risk_decision.rs` proves that for every minimum). §50 therefore asks
//! this file for the first of its two halves, and it is the half real history can
//! answer:
//!
//! ```text
//! RiskRejected  →  Execution must stop.
//! ```
//!
//! Stopped here means stopped *before* the endpoint: an intent that the risk layer never
//! accepted is not built, not priced, not nonce-claimed and not sent, and the four
//! endpoint surfaces are wired to one object whose only behaviour is to panic when asked.
//! Every assertion in this file is about the absence of that panic.
//!
//! The second half §50 asks for — Builder → Signer behind a RiskApproved finding — is
//! not reachable from real history without editing it, and §51 forbids the edit (no
//! reserve, fee, tax, balance, block or receipt is adjusted to manufacture an Accept).
//! It runs instead through §35's validation transaction, which needs no profit claim, and
//! through the scripted lane in `crates/execution/tests/`.
//!
//! Nothing here is a number someone typed: the route comes from the detector over the
//! captured block, the state from the dump M4 froze off the archive node, the decision
//! from the risk layer, and the override's own words from the simulation request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;

use evm_core::BlockNumber;
use evm_execution::{
    Abilities, AttemptProvenance, ChainReader, EndpointKind, ExecutionError, ExecutionIds,
    ExecutionMode, ExecutionSetup, ExecutionStage, FeePolicy, FeeReading, FeeSource, Freshness,
    NonceReading, NonceSource, Receipt, Result as ExecutionResult, SenderFunding,
    SignedTransaction, Signer, StageReport, SubmissionOutcome, TransactionIntent,
    TransactionSubmitter, TransactionType,
};
use evm_metrics::{Clock, Metrics};
use evm_risk::{RiskDecision, RiskPolicy, RiskThresholds};
use evm_simulation::{engine::run, GasPricing, SimulationRequest, SimulationResult};

mod support;
use support::Fixture;

/// An endpoint whose only behaviour is to say it was asked.
///
/// It does not return an error and it does not answer a value: reaching this object at
/// all is the failure, because the run being tested should have stopped before the first
/// read. A refusal the stage could handle gracefully would let a wrong implementation
/// pass the report assertions; a panic cannot be mistaken for the ladder finishing.
#[derive(Default, Debug)]
struct NeverAsk {
    asked: AtomicBool,
}

impl NeverAsk {
    fn touched(&self) -> bool {
        self.asked.load(Ordering::Relaxed)
    }

    fn asked_of(&self, what: &str) -> ! {
        self.asked.store(true, Ordering::Relaxed);
        panic!(
            "a risk-rejected finding reached the {what} read; §45/§K say no intent, so no \
             price, nonce, balance or send may be asked for it"
        )
    }
}

#[async_trait]
impl FeeSource for NeverAsk {
    async fn fee_reading(
        &self,
        _block_number: u64,
        _block_hash: B256,
        _tx_type: TransactionType,
        _policy: &FeePolicy,
    ) -> ExecutionResult<FeeReading> {
        self.asked_of("fee")
    }

    async fn suggested_tip(&self) -> ExecutionResult<Option<U256>> {
        self.asked_of("tip")
    }

    async fn balance(&self, _address: Address, _block_number: u64) -> ExecutionResult<U256> {
        self.asked_of("balance")
    }
}

#[async_trait]
impl NonceSource for NeverAsk {
    async fn nonce(&self, _address: Address) -> ExecutionResult<NonceReading> {
        self.asked_of("nonce")
    }
}

#[async_trait]
impl ChainReader for NeverAsk {
    async fn block_hash_at(&self, _number: BlockNumber) -> ExecutionResult<Option<B256>> {
        self.asked_of("canonical block")
    }

    async fn endpoint_chain_id(&self) -> ExecutionResult<u64> {
        self.asked_of("chain id")
    }
}

#[async_trait]
impl TransactionSubmitter for NeverAsk {
    fn endpoint(&self) -> EndpointKind {
        EndpointKind::Recorded
    }

    fn may_submit(&self) -> bool {
        false
    }

    async fn submit(&self, _transaction: &SignedTransaction) -> ExecutionResult<SubmissionOutcome> {
        self.asked_of("submission")
    }

    async fn receipt(&self, _transaction_hash: B256) -> ExecutionResult<Option<Receipt>> {
        self.asked_of("receipt")
    }
}

/// Everything one gated attempt leaves behind, kept together so each test can assert on
/// the run, the decision, the lane's answer and the endpoint's own record of having been
/// asked.
struct Gate {
    run: SimulationResult,
    decision: RiskDecision,
    report: StageReport,
    endpoint: Arc<NeverAsk>,
    metrics: Metrics,
}

impl Gate {
    /// The lane's whole answer to a risk answer it did not accept: nothing at all, in
    /// four independent ways. `reached: None` and `execution_id: None` say no record was
    /// opened, `!endpoint.touched()` says no read was asked, and the absence of the
    /// signed and submission rows says no bytes were produced either.
    fn stopped(&self) -> bool {
        self.report.reached.is_none()
            && self.report.execution_id.is_none()
            && !self.report.sent
            && self.report.signed.is_none()
            && self.report.submission.is_none()
            && !self.endpoint.touched()
    }
}

/// The generous thresholds: any strictly positive net figure, no gas ceiling. Chosen so
/// the answer can only be about the run, never about a knob someone tightened.
fn thresholds() -> RiskThresholds {
    RiskThresholds {
        minimum_net_profit_wei: U256::ZERO,
        maximum_gas: u64::MAX,
    }
}

/// One real run, its real decision, and the lane's answer to both — over an endpoint that
/// panics if the lane asks it anything.
async fn gate(fixture: &Fixture, request: &SimulationRequest) -> Gate {
    let run = run(fixture.shared(), request)
        .await
        .expect("the real route produces a result or a refusal, never a crash");
    let decision = thresholds().evaluate(&run);

    let endpoint = Arc::new(NeverAsk::default());
    let abilities = Abilities {
        submitter: endpoint.clone(),
        fees: endpoint.clone(),
        nonces: endpoint.clone(),
        chain: endpoint.clone(),
    };
    // §34's words, quoted from the request that applied the override rather than
    // paraphrased here: this is what makes the run possible, and it is not a fact about
    // the chain.
    let funding = SenderFunding::Overridden {
        detail: request
            .sender_setup_override()
            .expect("the canonical spec funds its sender")
            .reason,
    };
    let opportunity_id = opportunity_id(fixture, &run);
    let state_fingerprint = format!("block {}", run.block.number.0);
    let mut stage = ExecutionStage::new(
        abilities,
        Signer::without_key(ExecutionMode::BuildOnly),
        ExecutionSetup::default(),
        run.chain_id.0,
        Clock::new(),
    )
    .expect("a lane over a scripted endpoint assembles");
    let mut metrics = Metrics::default();
    let report = stage
        .on_risk_decision(
            &run,
            &decision,
            AttemptProvenance {
                opportunity_id: &opportunity_id,
                state_fingerprint: &state_fingerprint,
                funding,
                freshness: Freshness::Active,
            },
            &mut metrics,
        )
        .await;
    Gate {
        run,
        decision,
        report,
        endpoint,
        metrics,
    }
}

/// The finding's own identity, in the words the pipeline uses for it: the chain, the
/// block, and the pools the detector walked.
fn opportunity_id(fixture: &Fixture, run: &SimulationResult) -> String {
    format!(
        "chain {} block {} historical route {}",
        run.chain_id.0,
        run.block.number.0,
        fixture
            .route
            .legs
            .iter()
            .map(|leg| format!("{:#x}", leg.address()))
            .collect::<Vec<_>>()
            .join("|"),
    )
}

/// §45's refusal, in the lane's own words: the intent is the first thing the ladder needs,
/// and the risk answer is what refuses to make one.
fn refused_because_no_intent(gate: &Gate) {
    assert!(
        gate.stopped(),
        "a rejected finding reached further than the intent that was never formed: {:?}",
        gate.report.reached
    );
    assert!(
        gate.report.detail.contains("no intent was formed"),
        "{}",
        gate.report.detail
    );
    assert!(
        gate.report.detail.contains("RiskDecision::Accept"),
        "the refusal names the check that stopped it: {}",
        gate.report.detail
    );
    assert_eq!(
        gate.metrics.get("execution_refused_before_claim"),
        1,
        "the stop is counted, not just silent: {}",
        gate.metrics.counters.to_json()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_analytical_ask_that_reverts_stops_before_the_endpoint() {
    // §50's first requirement, on M3's own figure: the zero-slippage ask has a gross
    // profit in the finding and a revert in the EVM, and it is the revert that decides.
    let fixture = Fixture::load().await;
    let gate = gate(&fixture, &fixture.spec(fixture.route.analytical_output)).await;

    assert!(
        !gate.run.status.completed(),
        "the rung this test is about has to be the reverting one: {:?}",
        gate.run.status
    );
    assert!(
        matches!(gate.decision, RiskDecision::Reject { .. }),
        "{}",
        gate.decision
    );
    assert!(
        gate.decision.reason().contains("reverted"),
        "the refusal quotes the pool's own revert reason: {}",
        gate.decision.reason()
    );
    assert!(
        gate.report.detail.contains("Reject"),
        "the lane repeats the risk layer's answer, not its own guess: {}",
        gate.report.detail
    );
    refused_because_no_intent(&gate);
    println!(
        "§50: {}\n     → {}\n     → {}",
        gate.run.summary(),
        gate.decision,
        gate.report.detail
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_run_that_completes_at_a_loss_stops_in_the_same_place() {
    // The other half of "a gross profit is not a sendable trade": this rung *does*
    // execute, so the stop is the profit floor's doing rather than the EVM's, and the
    // lane's behaviour is identical. Two different rules, one boundary.
    let fixture = Fixture::load().await;
    let gate = gate(&fixture, &fixture.spec(U256::ONE)).await;

    assert!(
        gate.run.status.completed(),
        "the ask of one is the rung that pays: {:?}",
        gate.run.status
    );
    assert!(
        matches!(gate.decision, RiskDecision::Reject { .. }),
        "{}",
        gate.decision
    );
    assert!(
        gate.decision.reason().contains("before gas"),
        "the loss is stated in both parts, as M4's §31 requires: {}",
        gate.decision.reason()
    );
    refused_because_no_intent(&gate);
    println!(
        "§50: {}\n     → {}\n     → {}",
        gate.run.summary(),
        gate.decision,
        gate.report.detail
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn an_unknown_decision_stops_where_a_reject_does() {
    // §45's gate is on `Accept`, not on "not a Reject". This rung is the real route,
    // real state and real execution with §29's price undeclared, so the answer is
    // `Unknown` — and an Unknown is the one decision a reader might be tempted to send
    // anyway.
    let fixture = Fixture::load().await;
    let mut request = fixture.spec(U256::ONE);
    request.pricing = GasPricing::Unresolved {
        reason: "this test declares no price".to_string(),
    };
    let gate = gate(&fixture, &request).await;

    assert!(
        gate.run.status.completed(),
        "the run itself succeeded; only the price is missing: {}",
        gate.run.summary()
    );
    assert!(
        matches!(gate.decision, RiskDecision::Unknown { .. }),
        "{}",
        gate.decision
    );
    assert!(
        gate.report.detail.contains("Unknown"),
        "an Unknown is refused by name: {}",
        gate.report.detail
    );
    refused_because_no_intent(&gate);
    println!("§50: {} → {}", gate.decision, gate.report.detail);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn nothing_in_a_refused_attempt_is_a_fact_about_the_chain() {
    // The same refusal from the reader's side: the lane's report carries no chain
    // assertion at all — no mode beyond the one it was started in, no transaction hash,
    // no lane claim — so §51's "an unread fact is a fact about the run" holds for every
    // field a report has, not only for the ones this file happens to check.
    let fixture = Fixture::load().await;
    let gate = gate(&fixture, &fixture.spec(U256::ONE)).await;

    assert_eq!(gate.report.mode, ExecutionMode::BuildOnly);
    assert_eq!(gate.report.transaction_hash, None);
    assert!(gate.report.sources.is_empty());
    assert!(gate.report.record.is_none());
    assert_eq!(gate.report.simulation_id, B256::ZERO);
    assert_eq!(gate.report.risk_decision_id, B256::ZERO);
    // And the ids are zero *because* no intent was formed, not because the run had none:
    // the real finding does produce the hash the intent would have carried.
    let ids = ExecutionIds::of(&gate.run, &gate.decision, "for comparison");
    assert_ne!(ids.simulation_id, B256::ZERO);
    assert_eq!(
        gate.report.opportunity_id,
        opportunity_id(&fixture, &gate.run),
        "the refusal still says which finding it is about, verbatim — §37's log line needs it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_refusal_is_an_error_value_the_lane_does_not_swallow() {
    // A control on the refusal itself: the same guard the stage runs is reachable
    // directly, so the test above cannot be passing because the stage catches a
    // different problem and reports it in refusal-shaped words.
    let fixture = Fixture::load().await;
    let run = fixture.run_at(U256::ONE).await;
    let decision = thresholds().evaluate(&run);
    let error = TransactionIntent::from_run(
        &run,
        &decision,
        "direct call",
        &format!("block {}", run.block.number.0),
        SenderFunding::RealState {
            source: "not the question this test asks".to_string(),
        },
    )
    .expect_err("a Reject cannot make an intent, whatever the funding says");
    assert!(
        matches!(error, ExecutionError::InvalidIntent(_)),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("RiskDecision::Accept"),
        "{error}"
    );
}
