//! §56: M10's plan walked through M6's lifecycle, unchanged.
//!
//! §24 bans an `ArbitrageSigner`, an `ArbitrageSubmitter` and an `ArbitrageReceipt`, and this
//! file is the witness that the ban was honoured rather than merely stated: every case below
//! enters through [`ExecutionStage::on_arbitrage_plan`] and comes out through the crate's own
//! builder, signer, gate, ledger, lane and receipt tracker. What M10 adds is one entry point,
//! one gate leg, and the two identities the record carries — no second ladder.
//!
//! Three claims are checked in every case, because they are the three §71 refuses to let
//! collapse into one word:
//!
//! * **the bytes** — the raw transaction the endpoint was handed decodes back to this plan's
//!   calldata, to this executor, with zero `value` (§23, §34, §37);
//! * **the rung** — the record stopped where the mode and the gate allowed, and `route_id`
//!   travelled with it (§38, §39);
//! * **the class** — §40's six are decided from the report's own typed facts, so submitted,
//!   included and successful stay three different sentences.
//!
//! The endpoint is scripted, which §40 permits for the *interface* only: its answers have the
//! shape measured on GIWA in `data/evidence/m6/probe-read-surface-2.txt` and
//! `probe-submission-surface.txt` (chain 91 342, a 371 wei base fee, a 1 000 000 wei tip, an L1
//! fee field on the receipt). Nothing here is evidence that a broadcast worked — §30's real
//! controlled execution is recorded separately — and every fixture is labelled
//! `CONTROLLED_FIXTURE` (§29), so no run in this file may be read as a market opportunity.
//!
//! Two of the cases are about something *not* happening, which is only a test when the check can
//! fail: the endpoint counts every trait call it receives, so "refused before anything was read"
//! is a measured zero rather than a hope.

use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use alloy_primitives::{address, keccak256, Address, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    decode_raw, recover_sender, Abilities, AmountDerivation, ArbitrageExecutionPlan, BuildPolicy,
    ChainReader, ClassFacts, DecodedTransaction, EndpointKind, ExecutablePlan, ExecutionBinding,
    ExecutionClass, ExecutionError, ExecutionJournal, ExecutionKey, ExecutionMode, ExecutionRecord,
    ExecutionSetup, ExecutionStage, ExecutionStatus, FeePolicy, FeeReading, FeeSource, Freshness,
    LaneRelease, MarketKind, NonceReading, NonceSource, PlanLeg, PlanValidity, ProfitDenomination,
    ProfitPolicy, Receipt, ReceiptPolicy, ReceiptStatus, SenderFunding, SignedTransaction, Signer,
    SimulationContext, SimulationOutcome, StageReport, SubmissionOutcome, TransactionSubmitter,
    TransactionType,
};
use evm_metrics::{Clock, Metrics};
use evm_protocol::{decode_calldata, ExecutorCall};

/// §40's synthetic key: the scalar one, never the operator's wallet (§35).
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
/// The canonical block the route was priced at — §8's provenance, and the head the passing runs
/// are judged against, so the plan's age is zero and the freshness leg is not what is under test.
const SIM_BLOCK: u64 = 37_984_319;
/// The block the scripted chain mines the transaction into.
const MINED_BLOCK: u64 = SIM_BLOCK + 1;

const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
const BALANCE_WEI: u128 = 20_000_000_000_000_000;
/// REVM's measurement for the two-leg route (§25). Deliberately different from every gas figure
/// the record ends up quoting, so "the simulation's number" and "the chain's number" stay
/// separable in the assertions below.
const SIMULATED_GAS: u64 = 210_000;
/// The limit that same REVM run was executed under, and therefore the number §13's policy
/// resolves against. It is larger than the burn by design: EIP-150 hands a nested call 63/64 of
/// the gas its caller has left, so a limit built from the burn can starve the deepest frame of a
/// two-leg route even when the identical call completes with headroom (§57's first real attempt
/// reverted exactly that way — 229,302 burned against a 249,302 limit).
const SIMULATED_GAS_LIMIT: u64 = 230_000;
const CHAIN_GAS_USED: u64 = 198_400;
/// §13's margin, resolved by the crate's own `GasPolicy` and quoted here so the assertion names
/// which two numbers produced the limit.
const GAS_MARGIN: u64 = 20_000;

const EXECUTOR: Address = address!("00000000000000000000000000000000000000ee");
const RECIPIENT: Address = address!("00000000000000000000000000000000000000bb");
const TOKEN_A: Address = address!("00000000000000000000000000000000000000a1");
const TOKEN_B: Address = address!("00000000000000000000000000000000000000b1");
const PAIR_0: Address = address!("00000000000000000000000000000000000000f1");
const PAIR_1: Address = address!("00000000000000000000000000000000000000f2");

fn sim_hash() -> B256 {
    B256::left_padding_from(&[7u8; 20])
}

fn mined_block_hash() -> B256 {
    B256::left_padding_from(&[9u8; 20])
}

/// The run this plan was priced on. A re-priced run gets its own id, which is what lets two sizes
/// of one route become two records rather than a duplicate (§30).
fn run_one() -> B256 {
    B256::left_padding_from(&[5u8; 20])
}

fn run_two() -> B256 {
    B256::left_padding_from(&[6u8; 20])
}

fn test_signer(mode: ExecutionMode) -> Signer {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(mode, key)
}

/// The account the synthetic key signs for, so the passing runs exercise §17's sender check
/// instead of only the case that names it.
fn synthetic_address() -> Address {
    test_signer(ExecutionMode::SignOnly)
        .address()
        .expect("a signer built from a key knows its address")
}

fn binding() -> ExecutionBinding {
    ExecutionBinding {
        chain_id: CHAIN,
        executor: EXECUTOR,
    }
}

fn leg(index: usize, amount_in: u64, amount_out: u64, min: u64) -> PlanLeg {
    let (token_in, token_out, pool, derivation) = match index {
        0 => (TOKEN_A, TOKEN_B, PAIR_0, AmountDerivation::PlanInput),
        _ => (
            TOKEN_B,
            TOKEN_A,
            PAIR_1,
            AmountDerivation::PreviousLegOutput,
        ),
    };
    PlanLeg {
        pool,
        token_in,
        token_out,
        amount_in: U256::from(amount_in),
        amount_out: U256::from(amount_out),
        min_amount_out: U256::from(min),
        derivation,
    }
}

/// The route: A → B → A, 1 000 in, 1 050 mid, 1 100 back, floored at 1 060 of the input token.
///
/// §29's label is the point of the fixture rather than a caveat on it: the numbers are chosen so
/// that a guard fires or does not, and none of them was read off a market.
fn route(
    sender: Address,
    input_amount: u64,
    outcome: SimulationOutcome,
    min_final_output: u64,
    simulation_id: B256,
) -> ArbitrageExecutionPlan {
    ArbitrageExecutionPlan::new(
        CHAIN,
        EXECUTOR,
        sender,
        RECIPIENT,
        TOKEN_A,
        U256::from(input_amount),
        vec![
            leg(0, input_amount, 1_050, 1_040),
            leg(1, 1_050, 1_100, 1_090),
        ],
        U256::from(min_final_output),
        PlanValidity {
            simulated_at_block: BlockNumber(SIM_BLOCK),
            max_block_age: 3,
            provenance: "§8's window, as this fixture declares it".to_string(),
        },
        SimulationContext {
            correlation_id: "m10-lifecycle-route".to_string(),
            block_number: BlockNumber(SIM_BLOCK),
            block_hash: sim_hash(),
            state_fingerprint: format!("{SIM_BLOCK}:0"),
            simulation_id,
            outcome,
            funding: SenderFunding::RealState {
                source: "§34: the fixture funded the sender it spends from".to_string(),
            },
            market: MarketKind::ControlledFixture {
                proves: "the atomic execution primitive works".to_string(),
            },
        },
        ProfitPolicy {
            denomination: ProfitDenomination::TokenSettled {
                token: TOKEN_A,
                reason: "§34: the route round-trips in the input token".to_string(),
            },
            required_final_balance: U256::from(min_final_output),
            provenance: "§12's floor, stated once and mirrored by the policy".to_string(),
        },
    )
}

fn succeeded() -> SimulationOutcome {
    SimulationOutcome::Succeeded {
        gas_used: SIMULATED_GAS,
        proved_gas_limit: SIMULATED_GAS_LIMIT,
        final_amount: U256::from(1_100u64),
    }
}

/// The passing fixture, as an immutable plan.
fn executable(sender: Address) -> ExecutablePlan {
    ExecutablePlan::new(
        route(sender, 1_000, succeeded(), 1_060, run_one()),
        &binding(),
    )
    .expect("the fixture route validates")
}

/// The endpoint: the four traits the stage reads, answering in GIWA's shape, with every payload it
/// was handed recorded **and every trait call counted**.
struct Scripted {
    chain_id: u64,
    base_fee: U256,
    tip: U256,
    balance: U256,
    nonce: u64,
    /// What `eth_getBlockByNumber` holds; an unlisted height is a block the chain does not have.
    blocks: HashMap<u64, B256>,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
    sent: Mutex<Vec<Vec<u8>>>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new() -> Self {
        Self {
            chain_id: CHAIN,
            base_fee: U256::from(BASE_FEE),
            tip: U256::from(TIP),
            balance: U256::from(BALANCE_WEI),
            nonce: 0,
            blocks: HashMap::from([(SIM_BLOCK, sim_hash()), (MINED_BLOCK, mined_block_hash())]),
            answers: Mutex::new(VecDeque::new()),
            receipts: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn answer(self, outcome: SubmissionOutcome) -> Self {
        self.answers
            .lock()
            .expect("an unlocked queue")
            .push_back(outcome);
        self
    }

    fn receipt(self, receipt: Option<Receipt>) -> Self {
        self.receipts
            .lock()
            .expect("an unlocked queue")
            .push_back(receipt);
        self
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }

    /// Every read and send the run asked for. A refusal that should not have touched the network
    /// has to be able to say zero.
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn raw(&self, index: usize) -> Vec<u8> {
        self.sent
            .lock()
            .expect("an unlocked counter")
            .get(index)
            .cloned()
            .expect("the run handed over this payload")
    }

    /// The payload the endpoint holds, decoded back into fields. The recovery closure is the
    /// crate's own, so the sender this returns is the sender the crate claims signed the bytes.
    fn decoded(&self, index: usize) -> DecodedTransaction {
        decode_raw(&self.raw(index), recover_sender).expect("the bytes handed to the node decode")
    }
}

#[async_trait]
impl FeeSource for Scripted {
    async fn fee_reading(
        &self,
        block_number: u64,
        block_hash: B256,
        tx_type: TransactionType,
        policy: &FeePolicy,
    ) -> evm_execution::Result<FeeReading> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        policy.apply(
            self.chain_id,
            block_number,
            block_hash,
            Some(self.base_fee),
            Some(self.tip),
            tx_type,
        )
    }

    async fn suggested_tip(&self) -> evm_execution::Result<Option<U256>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.tip))
    }

    async fn balance(&self, _address: Address, _block_number: u64) -> evm_execution::Result<U256> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.balance)
    }
}

#[async_trait]
impl NonceSource for Scripted {
    async fn nonce(&self, address: Address) -> evm_execution::Result<NonceReading> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(NonceReading {
            address,
            confirmed: self.nonce,
            pending: self.nonce,
            at_block: SIM_BLOCK,
            source: "scripted eth_getTransactionCount (confirmed and pending) in §56".to_string(),
        })
    }
}

#[async_trait]
impl ChainReader for Scripted {
    async fn block_hash_at(&self, number: BlockNumber) -> evm_execution::Result<Option<B256>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.blocks.get(&number.0).copied())
    }

    async fn endpoint_chain_id(&self) -> evm_execution::Result<u64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.chain_id)
    }
}

#[async_trait]
impl TransactionSubmitter for Scripted {
    fn endpoint(&self) -> EndpointKind {
        EndpointKind::PublicHttpRpc
    }

    fn may_submit(&self) -> bool {
        true
    }

    async fn submit(
        &self,
        transaction: &SignedTransaction,
    ) -> evm_execution::Result<SubmissionOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("an unlocked counter")
            .push(transaction.raw().to_vec());
        Ok(self
            .answers
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .unwrap_or(SubmissionOutcome::Rejected {
                reason: "the script ran out; §25 forbids guessing here".to_string(),
                endpoint: EndpointKind::PublicHttpRpc,
            }))
    }

    async fn receipt(&self, transaction_hash: B256) -> evm_execution::Result<Option<Receipt>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .receipts
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .flatten();
        Ok(next.map(|mut receipt| {
            receipt.transaction_hash = transaction_hash;
            receipt
        }))
    }
}

/// The stage over a scripted endpoint, with the receipt budget cut to two fast reads so the
/// timeout case costs milliseconds instead of §26's real twelve seconds.
fn assemble(endpoint: Scripted, mode: ExecutionMode) -> (ExecutionStage, Arc<Scripted>) {
    let scripted = Arc::new(endpoint);
    let abilities = Abilities {
        submitter: scripted.clone(),
        fees: scripted.clone(),
        nonces: scripted.clone(),
        chain: scripted.clone(),
    };
    let setup = ExecutionSetup {
        mode,
        fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
        build: BuildPolicy {
            expected_chain_id: CHAIN,
            ..Default::default()
        },
        receipts: ReceiptPolicy {
            attempts: 2,
            between_attempts: Duration::from_millis(1),
        },
    };
    // §19: `BuildOnly` runs with no key present at all, which is what makes its stop a ceiling
    // rather than a near miss.
    let signer = match mode {
        ExecutionMode::BuildOnly => Signer::without_key(mode),
        other => test_signer(other),
    };
    let stage = ExecutionStage::new(
        abilities,
        signer,
        setup,
        CHAIN,
        Clock::new(),
        ExecutionJournal::volatile(),
    )
    .expect("a stage over a scripted endpoint");
    (stage, scripted)
}

fn accepted() -> SubmissionOutcome {
    SubmissionOutcome::Accepted {
        transaction_hash: None,
        endpoint: EndpointKind::PublicHttpRpc,
        detail: "scripted eth_sendRawTransaction acknowledgement".to_string(),
    }
}

fn rejected() -> SubmissionOutcome {
    SubmissionOutcome::Rejected {
        reason: "scripted nonce too low".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    }
}

/// A receipt in the shape the probe measured on this chain, L1 fields included, addressed to the
/// executor the plan names — §26's `bind` refuses a receipt whose `to` disagrees, and a test whose
/// receipt the crate would throw away proves nothing about the run.
fn receipt(success: bool, sender: Address) -> Receipt {
    Receipt {
        transaction_hash: B256::ZERO,
        block_number: MINED_BLOCK,
        block_hash: mined_block_hash(),
        transaction_index: 2,
        success,
        gas_used: CHAIN_GAS_USED,
        effective_gas_price: U256::from(BASE_FEE * 2 + TIP),
        cumulative_gas_used: Some(U256::from(CHAIN_GAS_USED)),
        from: sender,
        to: Some(EXECUTOR),
        contract_address: None,
        tx_type: Some(2),
        logs: Vec::new(),
        l1_fee: Some(U256::from(7_400_000_000u64)),
        l1_gas_price: Some(U256::from(1_000u64)),
        l1_gas_used: Some(U256::from(7_400_000u64)),
        l1_base_fee_scalar: Some(U256::from(1_000u64)),
        l1_blob_base_fee: Some(U256::ZERO),
        l1_blob_base_fee_scalar: Some(U256::ZERO),
        provenance: "scripted eth_getTransactionReceipt in §56".to_string(),
    }
}

/// The record as the ledger holds it now, which is what `close()` reported from.
fn held(stage: &ExecutionStage, report: &StageReport) -> ExecutionRecord {
    stage
        .ledger()
        .get(report.execution_id.as_ref().expect("the run made a record"))
        .expect("the ledger holds the record it claimed")
        .clone()
}

/// §40's class, decided from the report's typed facts and the plan it was made for.
///
/// Nothing is inferred that the stage did not state: `sent` is §2.2's boundary as the submission
/// row answered it, `stopped_with` is the check that refused, and `receipt_answer` is the
/// tracker's. A run §40 has no class for therefore comes back `None` — which this file asserts as
/// often as the classes it can name.
fn class_of(plan: &ExecutablePlan, report: &StageReport) -> Option<ExecutionClass> {
    ExecutionClass::decide(&ClassFacts {
        executable: Some(plan),
        stopped_with: report.stopped_with.as_ref(),
        sent: report.sent,
        receipt: report.receipt_answer,
    })
}

async fn run_plan(
    stage: &mut ExecutionStage,
    plan: &ExecutablePlan,
    freshness: Freshness,
    metrics: &mut Metrics,
) -> StageReport {
    stage
        .on_arbitrage_plan(plan, &binding(), freshness, metrics)
        .await
}

/// §56/§23/§37/§38/§40: the whole ladder for a validated plan, and the bytes the node was handed
/// decode back to the plan.
#[tokio::test]
async fn a_validated_plan_walks_the_ladder_and_the_bytes_decode_back_to_it() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let endpoint = Scripted::new()
        .answer(accepted())
        .receipt(Some(receipt(true, sender)));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Included));
    assert!(report.sent);
    assert_eq!(scripted.sent_count(), 1, "one send, not two");
    assert_eq!(report.lane, LaneRelease::Released);
    assert!(stage.lane_is_idle());
    assert_eq!(report.stopped_with, None, "nothing refused");
    assert_eq!(
        report.receipt_answer,
        Some(ReceiptStatus::Included),
        "and the tracker's answer is a typed fact, not a sentence"
    );

    // The bytes, read out of what the endpoint actually holds rather than out of what the stage
    // says it built.
    let decoded = scripted.decoded(0);
    assert_eq!(decoded.unsigned.to, Some(EXECUTOR), "§36's target");
    assert_eq!(
        decoded.unsigned.input,
        plan.calldata().clone(),
        "§23: the signed calldata *is* the plan's calldata"
    );
    assert_eq!(
        decoded.unsigned.value,
        U256::ZERO,
        "§34: an ERC-20 route hands the contract no wei"
    );
    assert_eq!(decoded.unsigned.chain_id, CHAIN);
    assert_eq!(
        decoded.sender,
        Some(sender),
        "§17: the key signed for the plan's sender"
    );
    assert_eq!(
        decoded.unsigned.gas_limit,
        SIMULATED_GAS_LIMIT + GAS_MARGIN,
        "§13: the limit the simulation proved, plus the declared margin — not the gas it burned, \
         because a limit built from the burn starves the deepest call under EIP-150"
    );
    let call = decode_calldata(&decoded.unsigned.input).expect("the payload is the executor ABI");
    assert_eq!(
        call,
        plan.plan().to_call(),
        "and it decodes back to exactly this route"
    );
    match call {
        ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => {
            assert_eq!(legs.len(), 2, "one transaction, two legs — §40's point");
            assert_eq!(legs[0].pool, PAIR_0);
            assert_eq!(legs[1].pool, PAIR_1);
            assert_eq!(
                legs[1].token_out, TOKEN_A,
                "the round trip ends where it began"
            );
            assert_eq!(input_token, TOKEN_A);
            assert_eq!(amount_in, U256::from(1_000u64));
            assert_eq!(min_final_amount, U256::from(1_060u64));
            assert_eq!(recipient, RECIPIENT, "§19's named recipient, not a sink");
        }
        other => panic!("the plan encoded an execute call, not {other:?}"),
    }

    // The record.
    let owner = held(&stage, &report);
    assert_eq!(
        owner.route_id.as_deref(),
        Some(plan.route_id()),
        "§38's execution-layer identity travelled with the run"
    );
    assert_eq!(owner.opportunity_id, "m10-lifecycle-route");
    assert_eq!(owner.simulation_id, run_one());
    assert_eq!(owner.status, ExecutionStatus::Included);
    assert_eq!(owner.gas_used, Some(CHAIN_GAS_USED), "the chain's figure");
    assert_eq!(owner.value_wei, U256::ZERO);
    assert_eq!(owner.failure, None);
    assert_eq!(owner.blocked_reason, None);
    assert_eq!(
        owner.nonce, 0,
        "the lane's allocation, not a plan-invented nonce"
    );
    // §39: the third id is a correlation binding to the plan, recomputed here rather than quoted
    // from the intent, so the test is a witness for the rule and not a copy of the code.
    let mut joined = Vec::with_capacity(64);
    joined.extend_from_slice(run_one().as_slice());
    joined.extend_from_slice(plan.plan_hash().as_slice());
    assert_eq!(owner.risk_decision_id, keccak256(&joined));
    assert_eq!(report.risk_decision_id, owner.risk_decision_id);

    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::IncludedSucceeded),
        "§71: included is the chain's answer, and no further than that"
    );
    assert!(report.detail.contains("included in block"));
    assert!(!report.detail.contains("profit"), "§2.4");
    assert_eq!(metrics.get("execution_submit_success"), 1);
    assert_eq!(metrics.get("execution_receipt_success"), 1);
    assert_eq!(
        report.sources.len(),
        3,
        "fee, nonce and balance, each named"
    );
}

/// §20/§24: `BuildOnly` still gets the front half — the plan leg, the route identity and the
/// build — and stops at its ceiling without touching a node.
#[tokio::test]
async fn a_build_only_run_stops_at_built_with_the_route_identity_attached() {
    let plan = executable(synthetic_address());
    let endpoint = Scripted::new();
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::BuildOnly);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert!(
        !report.sent,
        "built is not signed, signed is not sent (§2.2)"
    );
    assert_eq!(scripted.sent_count(), 0);
    assert_eq!(report.receipt_answer, None);
    assert_eq!(
        report.stopped_with, None,
        "a ceiling is not a failure (§24)"
    );
    let owner = held(&stage, &report);
    assert_eq!(owner.route_id.as_deref(), Some(plan.route_id()));
    assert_eq!(
        owner.blocked_reason.as_deref(),
        Some(report.detail.as_str())
    );
    assert!(report.detail.contains("stops at Built"));
    assert_eq!(
        class_of(&plan, &report),
        None,
        "§40 names no class for a mode ceiling"
    );
    assert!(stage.lane_is_idle());
    assert_eq!(metrics.get("execution_blocked"), 1);
    assert_eq!(metrics.get("execution_stopped"), 0);
}

/// §7: a plan bound to another chain is refused before the stage reads anything at all.
#[tokio::test]
async fn a_plan_bound_to_another_chain_is_refused_before_anything_is_read() {
    let plan = executable(synthetic_address());
    let endpoint = Scripted::new();
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    // The binding the caller hands over names a chain this stage is not configured for; the
    // endpoint's own answer stays 91 342, and §7 says that is a third opinion, not a tie-breaker.
    let other = ExecutionBinding {
        chain_id: 1,
        executor: EXECUTOR,
    };
    let report = stage
        .on_arbitrage_plan(&plan, &other, Freshness::Active, &mut metrics)
        .await;

    assert_eq!(report.execution_id, None, "no record was claimed");
    assert_eq!(report.reached, None);
    assert_eq!(scripted.sent_count(), 0);
    assert_eq!(
        scripted.call_count(),
        0,
        "and nothing was read either — the refusal is about the plan, not the moment"
    );
    assert!(report.detail.contains("§7"), "{}", report.detail);
    assert!(matches!(
        report.stopped_with,
        Some(ExecutionError::PlanRejected(_))
    ));
    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::PlanRejected),
        "no calldata was built for this chain, so nothing could be sent"
    );
    assert_eq!(metrics.get("execution_refused_before_claim"), 1);
    assert_eq!(
        metrics.get("execution_attempt"),
        0,
        "the ladder never started"
    );
}

/// §25/§40: a route REVM saw revert is refused as a revert, not renamed a build failure.
#[tokio::test]
async fn a_route_that_revm_saw_revert_is_refused_as_a_contract_revert() {
    let sender = synthetic_address();
    let plan = ExecutablePlan::new(
        route(
            sender,
            1_000,
            SimulationOutcome::Reverted {
                revert: "DeliveryMismatch".to_string(),
            },
            1_060,
            run_one(),
        ),
        &binding(),
    )
    .expect("a reverted simulation is still a well-formed route");
    let endpoint = Scripted::new();
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.execution_id, None);
    assert_eq!(scripted.sent_count(), 0);
    assert_eq!(scripted.call_count(), 0);
    assert!(report.detail.contains("reverted"), "{}", report.detail);
    assert!(report.detail.contains("DeliveryMismatch"));
    // The refusal is a plan refusal in §39's taxonomy, and §40 still answers from the simulation
    // outcome: the guard that would have fired on chain is the fact, and the refusal is only the
    // stage acting on it early.
    assert!(matches!(
        report.stopped_with,
        Some(ExecutionError::PlanRejected(_))
    ));
    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::ContractReverted),
        "nothing was sent, and the reason is the contract's own guard"
    );
    assert_eq!(metrics.get("execution_refused_before_claim"), 1);
}

/// §8: staleness is decided by the existing freshness leg on the plan's own window. The run gets a
/// `StaleOpportunity` refusal, and §40's six have no word for it — which is the correct answer,
/// not a gap.
#[tokio::test]
async fn a_stale_plan_is_blocked_by_the_existing_freshness_leg() {
    let plan = executable(synthetic_address());
    let stale = plan.freshness_at(BlockNumber(SIM_BLOCK + 10));
    assert!(
        matches!(stale, Freshness::Stale { .. }),
        "the fixture declares a three-block window"
    );
    let endpoint = Scripted::new();
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, stale, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert_eq!(scripted.sent_count(), 0, "the gate is before the node");
    assert!(
        report.detail.contains("opportunity_fresh"),
        "{}",
        report.detail
    );
    assert!(report.detail.contains("the plan is stale"));
    assert!(matches!(
        report.stopped_with,
        Some(ExecutionError::StaleOpportunity(_))
    ));
    assert_eq!(
        class_of(&plan, &report),
        None,
        "§40 does not name a stale stop, and inventing a class for it is the collapse §40 forbids"
    );
    assert_eq!(metrics.get("execution_gate_blocked"), 1);
    assert!(stage.lane_is_idle(), "and the nonce it allocated came back");
}

/// §40: the chain included the bytes and the call failed. That is `IncludedReverted` — sent,
/// included, reverted, and not a submission failure.
#[tokio::test]
async fn an_included_receipt_with_status_zero_is_a_reverted_run() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let endpoint = Scripted::new()
        .answer(accepted())
        .receipt(Some(receipt(false, sender)));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Reverted));
    assert!(report.sent, "the send still happened");
    assert_eq!(scripted.sent_count(), 1);
    assert_eq!(report.receipt_answer, Some(ReceiptStatus::Reverted));
    assert_eq!(report.stopped_with, None);
    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::IncludedReverted),
        "the guard fired on chain — the route left no partial state and earned nothing"
    );
    assert!(report.detail.contains("nothing about this run is profit"));
    let owner = held(&stage, &report);
    assert_eq!(owner.route_id.as_deref(), Some(plan.route_id()));
    assert_eq!(metrics.get("execution_revert"), 1);
    assert_eq!(metrics.get("execution_receipt_success"), 0);
    assert!(
        stage.lane_is_idle(),
        "a revert is a terminal receipt answer"
    );
}

/// §25: the node said no. Bytes were handed over, nothing is in flight, and §40's
/// `SubmissionFailed` is decided from the typed refusal rather than from prose.
#[tokio::test]
async fn a_node_refusal_is_a_submission_failure_with_nothing_in_flight() {
    let plan = executable(synthetic_address());
    let endpoint = Scripted::new().answer(rejected());
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert_eq!(
        scripted.sent_count(),
        1,
        "the payload did reach the interface"
    );
    assert!(
        !report.sent,
        "§2.2: a rejected submission is not a send, and the report says so"
    );
    assert_eq!(report.receipt_answer, None, "no receipt was ever asked for");
    assert!(matches!(
        report.stopped_with,
        Some(ExecutionError::SubmissionRejected(_))
    ));
    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::SubmissionFailed)
    );
    assert!(
        stage.lane_is_idle(),
        "a definite no means the nonce comes back"
    );
}

/// §26: the receipt budget ran out with no answer. The transaction may still land, so this run is
/// a timeout rather than a failure — and §40's `ReceiptTimeout` is now decidable from the report
/// alone, because the tracker's answer travels in it.
#[tokio::test]
async fn a_receipt_that_never_arrives_is_a_timeout_and_holds_the_lane() {
    let plan = executable(synthetic_address());
    // Accepted, and then nothing: the receipt queue is empty, so every read answers `None`.
    let endpoint = Scripted::new().answer(accepted());
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = run_plan(&mut stage, &plan, Freshness::Active, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Submitted));
    assert!(report.sent);
    assert_eq!(scripted.sent_count(), 1);
    assert_eq!(report.stopped_with, None, "not learning is not failing");
    assert_eq!(
        report.receipt_answer,
        Some(ReceiptStatus::Timeout),
        "the tracker's own word for a budget that ran out"
    );
    assert_eq!(
        class_of(&plan, &report),
        Some(ExecutionClass::ReceiptTimeout),
        "and §40 can name it without re-reading a log line"
    );
    assert!(report.detail.contains("§26"), "{}", report.detail);
    assert!(
        matches!(report.lane, LaneRelease::Held { .. }),
        "§11: something may still be live on chain, so the nonce stays reserved"
    );
    assert!(!stage.lane_is_idle());
}

/// §6: the same route at a different size is a different plan and a different execution, and the
/// route identity is the one thing that survives the change.
#[tokio::test]
async fn two_sizes_of_one_route_are_two_executions_sharing_one_route_identity() {
    let sender = synthetic_address();
    let small = executable(sender);
    let large_plan = ExecutablePlan::new(
        route(sender, 2_000, succeeded(), 2_060, run_two()),
        &binding(),
    )
    .expect("twice the size is still a route");

    assert_eq!(small.route_id(), large_plan.route_id(), "D5: one route");
    assert_ne!(small.plan_hash(), large_plan.plan_hash(), "two plans");
    assert_ne!(
        small.calldata(),
        large_plan.calldata(),
        "and the amount words are in the bytes"
    );
    assert_ne!(
        small.to_intent().ids.risk_decision_id,
        large_plan.to_intent().ids.risk_decision_id,
        "so the correlation id the record carries is different too"
    );

    let endpoint = Scripted::new()
        .answer(accepted())
        .answer(accepted())
        .receipt(Some(receipt(true, sender)))
        .receipt(Some(receipt(true, sender)));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let first = run_plan(&mut stage, &small, Freshness::Active, &mut metrics).await;
    let second = run_plan(&mut stage, &large_plan, Freshness::Active, &mut metrics).await;

    assert_eq!(first.reached, Some(ExecutionStatus::Included));
    assert_eq!(second.reached, Some(ExecutionStatus::Included));
    assert_eq!(
        metrics.get("execution_duplicate"),
        0,
        "a changed plan is a new run, never an edit of the old one (§6)"
    );
    assert_ne!(first.execution_id, second.execution_id);
    assert_ne!(first.transaction_hash, second.transaction_hash);
    assert_eq!(scripted.sent_count(), 2);
    assert_eq!(
        held(&stage, &first).route_id,
        held(&stage, &second).route_id,
        "and both records still say which route they walked"
    );
    for (index, report, plan) in [(0usize, &first, &small), (1, &second, &large_plan)] {
        let bytes = scripted.decoded(index).unsigned.input;
        assert_eq!(bytes, plan.calldata().clone(), "payload {index}");
        assert_eq!(
            report.receipt_answer,
            Some(ReceiptStatus::Included),
            "each run answered its own receipt"
        );
    }
}
