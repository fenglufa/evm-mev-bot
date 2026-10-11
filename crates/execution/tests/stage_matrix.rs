//! §55's ladder, driven end to end: one call to [`evm_execution::ExecutionStage`] against a
//! scripted endpoint, and the record, the evidence and the lane as they ended.
//!
//! [`lane_matrix`](mod@crate) tests each part against hand-made facts. This file tests the
//! one claim M6 actually makes — that the rungs are reached *in order*, that every stop
//! writes a reason, and that nothing reaches a node in a mode that may not send — and that
//! claim is only testable by running the whole sequence.
//!
//! §40's permission covers exactly one thing here: the endpoint's four answers are scripted,
//! because what is under test is what our code does with an answer. Everything else is the
//! crate's own — the real builder, signer (on the synthetic scalar-1 key), gate, ledger,
//! lane and receipt tracker. The receipt keeps the shape measured in
//! `data/evidence/m6/probe-submission-surface.txt`, L1 fee fields included. Nothing in this
//! file is evidence that a broadcast worked; §41's real-chain runs are recorded separately.
//!
//! Three of the cases below are about something *not* happening, which is only a test when
//! the check can fail: each of them also reads the endpoint's own count of payloads handed
//! over, so "no bytes left the process" is a number rather than a hope.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    Abilities, BuildPolicy, ChainReader, EndpointKind, ExecutionError, ExecutionJournal,
    ExecutionKey, ExecutionMode, ExecutionRecord, ExecutionSetup, ExecutionStage, ExecutionStatus,
    FeePolicy, FeeReading, FeeSource, LaneRelease, NonceReading, NonceSource, Receipt,
    ReceiptPolicy, SenderFunding, SignedTransaction, Signer, StageReport, SubmissionOutcome,
    TransactionIntent, TransactionSubmitter, TransactionType, UnsignedTransaction,
};
use evm_metrics::{Clock, Metrics};
use evm_simulation::BlockPin;

/// §40's synthetic key: the scalar one, never the operator's wallet.
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
const BLOCK: u64 = 37_486_792;
/// The block the scripted chain mines the transaction into.
const MINED_BLOCK: u64 = BLOCK + 1;

/// The fee numbers measured in `data/evidence/m6/probe-read-surface-2.txt`: a 371 wei base
/// fee and a 1 000 000 wei suggested tip.
const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
/// The test wallet's measured balance, 0.02 native (`probe-submission-surface.txt`).
const BALANCE_WEI: u128 = 20_000_000_000_000_000;

/// The label §42 requires, in the words the report uses for it.
const LABEL: &str = "M6 execution validation transaction";

fn pinned_hash() -> B256 {
    B256::left_padding_from(&[7u8; 20])
}

fn mined_block_hash() -> B256 {
    B256::left_padding_from(&[9u8; 20])
}

fn target() -> Address {
    Address::from_slice(&[0x6bu8; 20])
}

fn test_signer(mode: ExecutionMode) -> Signer {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(mode, key)
}

/// The key's own account, so the passing runs exercise §17's sender check rather than only
/// the one case that names it.
fn synthetic_address() -> Address {
    test_signer(ExecutionMode::SignOnly)
        .address()
        .expect("a signer built from a key knows its address")
}

fn unsigned() -> UnsignedTransaction {
    UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id: CHAIN,
        nonce: 0,
        to: Some(target()),
        value: U256::ZERO,
        gas_limit: 21_000,
        input: Bytes::from(vec![0x12u8, 0x34, 0x56, 0x78]),
        access_list: Vec::new(),
        // A validation transaction carries its own fee fields: nothing read it from a
        // simulation, and §12's rule is that a fee names its source. The stage prices it
        // from the scripted endpoint either way, and this is the number the gate sees.
        max_priority_fee_per_gas: Some(U256::from(TIP)),
        max_fee_per_gas: Some(U256::from(BASE_FEE * 2 + TIP)),
    }
}

/// One §35 validation intent — the only shape this milestone can honestly send.
fn validation_intent(sender: Address) -> TransactionIntent {
    TransactionIntent::validation(
        BlockPin::new(BlockNumber(BLOCK), pinned_hash()),
        sender,
        &unsigned(),
    )
    .expect("a validation intent over a call transaction")
}

/// The §40 endpoint: the four traits the stage reads, answered from these fields, with every
/// payload it was handed recorded.
///
/// Two behaviours here are this file's own code written to match a documented contract
/// rather than the crate's: it refuses to send when `may_submit()` says no (the same guard
/// `crates/execution/src/giwa/sequencer_direct.rs` runs), and it stamps the requested hash
/// onto the receipt it returns, which is what `eth_getTransactionReceipt` does.
struct Scripted {
    endpoint: EndpointKind,
    /// Mode and endpoint combined, the way the real adapter answers it (§20).
    allowed: bool,
    chain_id: u64,
    base_fee: U256,
    tip: U256,
    balance: U256,
    /// Both nonce views, answered as one moment.
    nonce: u64,
    /// What `eth_getBlockByNumber` holds; an unlisted height is a block the chain does not
    /// have.
    blocks: HashMap<u64, B256>,
    /// How many times a run asked this endpoint which block one height is. M12-B §5's rule
    /// is that the answer is never carried over, so the count belongs to the attempt, not to
    /// the stage.
    binding_reads: AtomicUsize,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    sent: Mutex<Vec<Vec<u8>>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
}

impl Scripted {
    /// An endpoint answering the way [`CHAIN`]'s does, for a run in `mode`.
    fn new(mode: ExecutionMode) -> Self {
        Self {
            endpoint: EndpointKind::PublicHttpRpc,
            allowed: mode.may_submit(),
            chain_id: CHAIN,
            base_fee: U256::from(BASE_FEE),
            tip: U256::from(TIP),
            balance: U256::from(BALANCE_WEI),
            nonce: 0,
            blocks: HashMap::from([(BLOCK, pinned_hash()), (MINED_BLOCK, mined_block_hash())]),
            binding_reads: AtomicUsize::new(0),
            answers: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            receipts: Mutex::new(VecDeque::new()),
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

    fn with_balance(self, balance: U256) -> Self {
        Self { balance, ..self }
    }

    /// The height a run pins a decision to, answered with a different hash: a reorg.
    fn reorg_the_pinned_block(self) -> Self {
        Self {
            blocks: HashMap::from([
                (BLOCK, B256::left_padding_from(&[0xeeu8; 20])),
                (MINED_BLOCK, mined_block_hash()),
            ]),
            ..self
        }
    }

    /// The node no longer holds `number` at all — the §5 case of a restart that has not
    /// yet caught back up to the height an older decision was pinned to.
    fn without_height(self, number: u64) -> Self {
        let mut blocks = self.blocks.clone();
        blocks.remove(&number);
        Self { blocks, ..self }
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }

    /// How many times this endpoint was asked which block a height is.
    fn binding_reads(&self) -> usize {
        self.binding_reads.load(Ordering::SeqCst)
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
        Ok(Some(self.tip))
    }

    async fn balance(&self, _address: Address, _block_number: u64) -> evm_execution::Result<U256> {
        Ok(self.balance)
    }
}

#[async_trait]
impl NonceSource for Scripted {
    async fn nonce(&self, address: Address) -> evm_execution::Result<NonceReading> {
        Ok(NonceReading {
            address,
            confirmed: self.nonce,
            pending: self.nonce,
            at_block: BLOCK,
            source: "scripted eth_getTransactionCount (confirmed and pending) in §55's matrix"
                .to_string(),
        })
    }
}

#[async_trait]
impl ChainReader for Scripted {
    async fn block_hash_at(&self, number: BlockNumber) -> evm_execution::Result<Option<B256>> {
        self.binding_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.blocks.get(&number.0).copied())
    }

    async fn endpoint_chain_id(&self) -> evm_execution::Result<u64> {
        Ok(self.chain_id)
    }
}

#[async_trait]
impl TransactionSubmitter for Scripted {
    fn endpoint(&self) -> EndpointKind {
        self.endpoint
    }

    fn may_submit(&self) -> bool {
        self.allowed
    }

    async fn submit(
        &self,
        transaction: &SignedTransaction,
    ) -> evm_execution::Result<SubmissionOutcome> {
        if !self.may_submit() {
            return Err(ExecutionError::ModeGate(format!(
                "submission was asked of an endpoint that may not broadcast ({})",
                self.endpoint.name()
            )));
        }
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
                endpoint: self.endpoint,
            }))
    }

    async fn receipt(&self, transaction_hash: B256) -> evm_execution::Result<Option<Receipt>> {
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

/// The stage over a scripted endpoint, with the receipt budget cut to two fast reads so a
/// timeout case costs milliseconds instead of §26's real twelve seconds.
///
/// Returns the endpoint back as well: [`ExecutionStage`] keeps the only other handles on it,
/// and the count that proves "nothing was sent" has to be read from outside.
fn assemble(endpoint: Scripted, mode: ExecutionMode) -> (ExecutionStage, Arc<Scripted>) {
    let scripted = Arc::new(endpoint);
    (stage_over(&scripted, mode), scripted)
}

/// A stage reading from an endpoint that outlives it: §5's rule is about what happens
/// between two attempts over one node, which one stage alone cannot show.
fn stage_over(scripted: &Arc<Scripted>, mode: ExecutionMode) -> ExecutionStage {
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
    // §19: `BuildOnly` runs with no key present at all, which is what makes its stop a
    // ceiling rather than a near miss.
    let signer = match mode {
        ExecutionMode::BuildOnly => Signer::without_key(mode),
        other => test_signer(other),
    };
    ExecutionStage::new(
        abilities,
        signer,
        setup,
        CHAIN,
        Clock::new(),
        ExecutionJournal::volatile(),
    )
    .expect("a stage over a scripted endpoint")
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

/// A receipt in the shape the probe measured on this chain, L1 fields included.
fn receipt(success: bool, sender: Address) -> Receipt {
    Receipt {
        transaction_hash: B256::ZERO,
        block_number: MINED_BLOCK,
        block_hash: mined_block_hash(),
        transaction_index: 2,
        success,
        gas_used: 21_000,
        effective_gas_price: U256::from(BASE_FEE * 2 + TIP),
        cumulative_gas_used: Some(U256::from(21_000u64)),
        from: sender,
        to: Some(target()),
        contract_address: None,
        tx_type: Some(2),
        logs: Vec::new(),
        l1_fee: Some(U256::from(7_400_000_000u64)),
        l1_gas_price: Some(U256::from(1_000u64)),
        l1_gas_used: Some(U256::from(7_400_000u64)),
        l1_base_fee_scalar: Some(U256::from(1_000u64)),
        l1_blob_base_fee: Some(U256::ZERO),
        l1_blob_base_fee_scalar: Some(U256::ZERO),
        provenance: "scripted eth_getTransactionReceipt in §55's matrix".to_string(),
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

async fn validation_run(
    stage: &mut ExecutionStage,
    metrics: &mut Metrics,
    sender: Address,
) -> StageReport {
    stage
        .on_validation(validation_intent(sender), LABEL, metrics)
        .await
}

/// §55/§Q: the whole ladder, rung by rung, ending in a receipt the chain named.
#[tokio::test]
async fn a_submitted_validation_run_reaches_included() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(true, synthetic_address())));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Included));
    let owner = held(&stage, &report);
    assert_eq!(owner.status, ExecutionStatus::Included);
    assert_eq!(owner.transaction_hash, report.transaction_hash);
    assert_eq!(
        owner.gas_used,
        Some(21_000),
        "the chain's figure, not the limit"
    );
    assert_eq!(owner.l1_fee, Some(U256::from(7_400_000_000u64)));
    assert!(owner.included_at_ms.is_some());
    assert_eq!(owner.failure, None, "a run that landed did not fail");
    assert_eq!(owner.blocked_reason, None);
    assert_eq!(scripted.sent_count(), 1, "one send, not two");
    assert_eq!(report.lane, LaneRelease::Released);
    assert!(stage.lane_is_idle());
    assert!(report.sent);
    let submission = report
        .submission
        .as_ref()
        .expect("the send is its own evidence row");
    assert!(submission.was_sent());
    assert_eq!(
        submission.outcome, "included",
        "and it says what the chain said"
    );
    assert_eq!(metrics.get("execution_submit_success"), 1);
    assert_eq!(metrics.get("execution_receipt_success"), 1);
    // §2.4: included is not profitable. The line says what the chain charged and turns
    // that into no gain — the comparison is M7's (§56).
    assert!(
        report.detail.contains("included in block"),
        "{}",
        report.detail
    );
    assert!(!report.detail.contains("profit"));
    assert_eq!(
        report.sources.len(),
        3,
        "fee, nonce and balance are all read"
    );
}

/// §19/§20/§U: `BuildOnly` stops at `Built` because it may not read a key — a ceiling the
/// record names, not a failure.
#[tokio::test]
async fn build_only_stops_at_built_with_no_bytes_anywhere() {
    let endpoint = Scripted::new(ExecutionMode::BuildOnly);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::BuildOnly);
    let mut metrics = Metrics::default();
    // The mode never reads a key, so there is no account to match the sender against, and
    // an arbitrary one is honest here.
    let sender = Address::from_slice(&[0xa1u8; 20]);
    let report = validation_run(&mut stage, &mut metrics, sender).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Built));
    assert_eq!(report.transaction_hash, None);
    assert!(!report.sent);
    assert_eq!(report.lane, LaneRelease::Released);
    assert!(stage.lane_is_idle());
    let owner = held(&stage, &report);
    assert_eq!(owner.status, ExecutionStatus::Built);
    assert_eq!(owner.failure, None, "the mode's ceiling is not a failure");
    let reason = owner
        .blocked_reason
        .expect("and the record says why it stopped");
    assert!(reason.contains("stops at Built"), "{reason}");
    assert_eq!(scripted.sent_count(), 0, "no payload reached the endpoint");
    assert!(report.signed.is_none());
    assert_eq!(metrics.get("execution_blocked"), 1);
    assert_eq!(metrics.get("execution_stopped"), 0);
}

/// §20: `SignOnly` over an endpoint that *could* broadcast has real bytes and still no send,
/// and the non-send is recorded as §53's blocked submission row rather than left out.
#[tokio::test]
async fn sign_only_holds_bytes_and_records_a_blocked_submission() {
    let endpoint = Scripted::new(ExecutionMode::SignOnly).answer(accepted());
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::SignOnly);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Signed));
    let evidence = report.signed.as_ref().expect("the bytes exist");
    assert_eq!(evidence.recovered_sender, synthetic_address());
    assert_eq!(evidence.expected_sender, synthetic_address());
    assert_eq!(evidence.chain_id, CHAIN);
    let submission = report
        .submission
        .as_ref()
        .expect("and the non-send is a row of its own");
    assert_eq!(submission.outcome, "blocked");
    assert!(!submission.was_sent());
    assert_eq!(
        scripted.sent_count(),
        0,
        "the queued answer was never asked for"
    );
    let owner = held(&stage, &report);
    assert_eq!(owner.failure, None);
    assert!(owner
        .blocked_reason
        .expect("a sign-only stop names its mode")
        .contains("sign-only"));
    assert!(stage.lane_is_idle(), "bytes that never left free the nonce");
}

/// §30/§R: the same state binding claimed twice gives one record, one transaction and a
/// second attempt that hands back the lane it took.
#[tokio::test]
async fn a_second_attempt_on_the_same_state_claims_nothing() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(true, synthetic_address())));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let first = validation_run(&mut stage, &mut metrics, synthetic_address()).await;
    let second = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(first.reached, Some(ExecutionStatus::Included));
    assert_eq!(
        second.execution_id, first.execution_id,
        "the duplicate reports the record that owns the state"
    );
    assert!(second.detail.contains("§30"), "{}", second.detail);
    assert_eq!(second.transaction_hash, first.transaction_hash);
    assert_eq!(stage.ledger().len(), 1);
    assert_eq!(metrics.get("execution_duplicate"), 1);
    assert_eq!(
        scripted.sent_count(),
        1,
        "the only payload is the first run's; the duplicate never reached a node"
    );
    assert!(stage.lane_is_idle());
}

/// §33/§M: a balance below this build's maximum spend stops the run before the signer is
/// asked for anything, with both numbers in the reason.
#[tokio::test]
async fn an_insufficient_balance_stops_before_the_signer() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .with_balance(U256::from(1_000u64));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert!(report.signed.is_none(), "nothing was signed");
    assert_eq!(scripted.sent_count(), 0);
    assert!(
        stage.lane_is_idle(),
        "a run that never sent gives the nonce back"
    );
    assert!(
        report.detail.contains("balance_sufficient"),
        "{}",
        report.detail
    );
    assert!(
        report.detail.contains("1000 wei available"),
        "{}",
        report.detail
    );
    let owner = held(&stage, &report);
    assert_eq!(owner.status, ExecutionStatus::Failed);
    let failure = owner.failure.expect("this one did fail");
    assert!(failure.contains("insufficient balance"), "{failure}");
    assert_eq!(metrics.get("execution_gate_blocked"), 1);
}

/// §32: a pin the chain no longer holds is a reorg, and a reorg stops the run at the gate
/// rather than at a node.
#[tokio::test]
async fn a_pin_the_chain_no_longer_holds_blocks_before_signing() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .reorg_the_pinned_block();
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert!(
        report.detail.contains("block_binding_valid"),
        "{}",
        report.detail
    );
    assert_eq!(scripted.sent_count(), 0);
    assert!(report.signed.is_none());
}

/// §5's first principle, in the shape a node restart leaves: the attempt pins a height, and
/// the endpoint now answers nothing at all for it. A previous attempt confirmed that height,
/// so a remembered answer would have passed the leg — the run asks instead, and stops.
#[tokio::test]
async fn a_height_the_node_no_longer_holds_is_asked_again_and_stops_the_run() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(true, synthetic_address())))
        .without_height(BLOCK);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert!(
        report.detail.contains("block_binding_valid"),
        "{}",
        report.detail
    );
    assert!(
        report.detail.contains("not verified"),
        "a block the node does not have is an unverified leg, not a reorg: {}",
        report.detail
    );
    assert_eq!(scripted.sent_count(), 0, "nothing left the process");
    assert!(report.signed.is_none());
    assert!(
        stage.lane_is_idle(),
        "a run that never sent gives the nonce back"
    );
    assert_eq!(
        scripted.binding_reads(),
        1,
        "the attempt asked once, at the gate, and did not reuse a decision"
    );
}

/// §5's rule read the other way: the answer is not carried forward either. Two attempts over
/// one endpoint each ask which block the height is, so a confirmation earned before the node
/// went down cannot be what authorises the transaction sent after it came back.
#[tokio::test]
async fn each_attempt_asks_the_node_rather_than_reusing_an_earlier_answer() {
    let endpoint = Arc::new(Scripted::new(ExecutionMode::BuildOnly));
    let sender = Address::from_slice(&[0xa1u8; 20]);
    let mut metrics = Metrics::default();

    let mut first = stage_over(&endpoint, ExecutionMode::BuildOnly);
    let first_report = validation_run(&mut first, &mut metrics, sender).await;
    assert_eq!(first_report.reached, Some(ExecutionStatus::Built));
    assert_eq!(endpoint.binding_reads(), 1);

    let mut second = stage_over(&endpoint, ExecutionMode::BuildOnly);
    let second_report = validation_run(&mut second, &mut metrics, sender).await;
    assert_eq!(second_report.reached, Some(ExecutionStatus::Built));
    assert_eq!(
        endpoint.binding_reads(),
        2,
        "the second attempt re-read the binding instead of inheriting the first one's"
    );
    assert_eq!(endpoint.sent_count(), 0, "and neither mode may send");

    // The counter measures attempts that reached the chain legs, not runs that started:
    // a duplicate is turned away before a build exists, so it never asks the node anything.
    let other = {
        let mut tx = unsigned();
        tx.gas_limit += 1;
        tx
    };
    let distinct = TransactionIntent::validation(
        BlockPin::new(BlockNumber(BLOCK), pinned_hash()),
        sender,
        &other,
    )
    .expect("a validation intent over a call transaction");
    let mut third = stage_over(&endpoint, ExecutionMode::BuildOnly);
    let _ = third
        .on_validation(distinct.clone(), LABEL, &mut metrics)
        .await;
    assert_eq!(
        endpoint.binding_reads(),
        3,
        "a distinct intent is a distinct attempt, and it asks too"
    );

    let duplicate = third.on_validation(distinct, LABEL, &mut metrics).await;
    assert!(duplicate.detail.contains("§30"), "{}", duplicate.detail);
    assert_eq!(
        endpoint.binding_reads(),
        3,
        "the duplicate stopped before the chain legs, so it added no read"
    );
}

/// §26/§O: running out of receipt reads is not a failure. The record stays where the chain
/// left it, the lane stays held, and nothing is resent.
#[tokio::test]
async fn a_receipt_that_never_arrives_is_a_timeout_and_not_a_failure() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(None)
        .receipt(None);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Submitted));
    assert!(report.detail.contains("§26"), "{}", report.detail);
    assert!(report.detail.contains("nothing is resent") || report.detail.contains("may still"));
    let owner = held(&stage, &report);
    assert_eq!(owner.failure, None);
    assert!(owner
        .blocked_reason
        .expect("and the reason is on the record")
        .contains("§26"));
    assert_eq!(
        scripted.sent_count(),
        1,
        "a timeout is never a reason to send again"
    );
    assert!(report.sent, "the bytes did go out, and the record says so");
    assert!(
        matches!(report.lane, LaneRelease::Held { .. }),
        "the nonce is still reserved: {:?}",
        report.lane
    );
    assert!(!stage.lane_is_idle());
}

/// §25: an answer that leaves the transaction possibly in flight holds the lane and keeps
/// the record off the rung the node never granted.
#[tokio::test]
async fn an_unknown_answer_holds_the_lane_and_the_record() {
    let endpoint = Scripted::new(ExecutionMode::Submit).answer(SubmissionOutcome::Unknown {
        reason: "scripted HTTP 502 with no JSON answer".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    });
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(
        report.reached,
        Some(ExecutionStatus::Signed),
        "no acknowledgement, so no `Submitted` rung"
    );
    assert!(report.detail.contains("§25"), "{}", report.detail);
    assert!(
        report.transaction_hash.is_some(),
        "the bytes exist and are named"
    );
    assert!(report.sent, "and this row still says they left");
    assert_eq!(scripted.sent_count(), 1);
    assert!(matches!(report.lane, LaneRelease::Held { .. }));
    let owner = held(&stage, &report);
    assert_eq!(owner.failure, None, "not knowing is not a failure");
}

/// §25/§39: a definite refusal is the one send answer that ends the attempt as a failure —
/// and it gives the nonce straight back.
#[tokio::test]
async fn a_definite_refusal_fails_the_attempt_and_frees_the_lane() {
    let endpoint = Scripted::new(ExecutionMode::Submit).answer(rejected());
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    let owner = held(&stage, &report);
    let failure = owner.failure.expect("the node said no");
    assert!(failure.contains("rejected"), "{failure}");
    assert_eq!(report.lane, LaneRelease::Released);
    assert!(stage.lane_is_idle());
    assert!(!report.sent, "a refusal is not a transaction on the chain");
    assert_eq!(scripted.sent_count(), 1);
    assert_eq!(metrics.get("execution_submission_rejected"), 1);
}

/// §P: the chain executed it and the call failed. `Reverted`, never a success, never a
/// profit.
#[tokio::test]
async fn a_reverted_receipt_is_recorded_as_reverted() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(false, synthetic_address())));
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Reverted));
    assert!(report.detail.contains("reverted"), "{}", report.detail);
    assert!(report.detail.contains("nothing about this run is profit"));
    let owner = held(&stage, &report);
    assert_eq!(owner.status, ExecutionStatus::Reverted);
    assert_eq!(owner.failure, None, "the chain's answer is not our failure");
    assert_eq!(metrics.get("execution_revert"), 1);
    assert_eq!(metrics.get("execution_receipt_success"), 0);
    assert_eq!(scripted.sent_count(), 1);
    assert!(stage.lane_is_idle(), "a terminal receipt frees the nonce");
}

/// §17: a stage configured for one wallet refuses an intent for another, before paying for
/// a single read.
#[tokio::test]
async fn a_signer_never_prices_a_transaction_for_another_account() {
    let endpoint = Scripted::new(ExecutionMode::SignOnly);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::SignOnly);
    let mut metrics = Metrics::default();
    let other = Address::from_slice(&[0xd7u8; 20]);
    let report = validation_run(&mut stage, &mut metrics, other).await;

    assert_eq!(report.reached, None, "the attempt never became a record");
    assert_eq!(report.execution_id, None);
    assert!(
        report.detail.contains("never signs for another account"),
        "{}",
        report.detail
    );
    assert_eq!(scripted.sent_count(), 0);
    assert!(report.signed.is_none());
    assert_eq!(metrics.get("execution_stopped"), 1);
}

/// §34/§T: an intent whose run needed a state override stops at the builder, so no
/// signature and no bytes exist. Stated through a validation intent because that is the
/// shape this file can build without a simulation run; §50's fixture drives the same rule
/// from real history.
#[tokio::test]
async fn an_override_funded_intent_never_reaches_bytes() {
    let endpoint = Scripted::new(ExecutionMode::Submit).answer(accepted());
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let mut intent = validation_intent(synthetic_address());
    intent.funding = SenderFunding::Overridden {
        detail: "scripted sender balance override in §55's matrix".to_string(),
    };
    let report = stage.on_validation(intent, LABEL, &mut metrics).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert!(
        report.detail.contains("override-dependent"),
        "{}",
        report.detail
    );
    assert_eq!(scripted.sent_count(), 0);
    assert!(report.signed.is_none());
    assert!(stage.lane_is_idle());
}

/// §J/§58: nothing a run reports carries a key. The synthetic scalar is searched for in both
/// renderings across every output a run produces.
#[tokio::test]
async fn no_output_of_a_run_names_the_private_key() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(true, synthetic_address())));
    let (mut stage, _scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;

    let haystack = format!(
        "{}\n{}\n{}\n{}",
        report.line(),
        report.to_json(),
        stage.ledger().to_json(),
        metrics.to_json(),
    );
    let secret = hex::encode(TEST_SCALAR);
    assert!(!haystack.contains(&secret), "a run leaked its key");
    assert!(!haystack.contains(&format!("0x{secret}")));
    assert!(!haystack.contains("private_key"));
    // What the run may say about the account is the public half, and it does say it.
    assert!(report.line().contains("execution_id="));
}

/// §37: the line a run prints carries the four identities, the status word and the hash once
/// one exists; the JSON row carries the lane's fate and the source of each number.
#[tokio::test]
async fn a_run_logs_the_four_identities_and_the_hash() {
    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(Some(receipt(true, synthetic_address())));
    let (mut stage, _scripted) = assemble(endpoint, ExecutionMode::Submit);
    let mut metrics = Metrics::default();
    let report = validation_run(&mut stage, &mut metrics, synthetic_address()).await;
    let line = report.line();

    for field in [
        "execution_id=",
        "opportunity_id=",
        "simulation_id=",
        "risk_decision_id=",
    ] {
        assert!(line.contains(field), "{field} missing from {line}");
    }
    assert!(line.contains("status=included"), "{line}");
    assert!(line.contains("transaction_hash=0x"), "{line}");

    let row = report.to_json();
    assert_eq!(row["status"], "included");
    assert_eq!(row["mode"], "submit");
    assert_eq!(row["sent"], true);
    assert_eq!(row["lane"], "released");
    assert_eq!(row["opportunity_id"], "m6-execution-validation-transaction");
    assert_eq!(row["sources"].as_array().map(Vec::len), Some(3));
}

/// The two ways a stage could be assembled wrongly are refused at assembly, not at the first
/// intent: a signer in another mode, and a build policy for another chain.
#[test]
fn a_stage_refuses_to_be_assembled_against_itself() {
    let endpoint = Arc::new(Scripted::new(ExecutionMode::BuildOnly));
    let abilities = Abilities {
        submitter: endpoint.clone(),
        fees: endpoint.clone(),
        nonces: endpoint.clone(),
        chain: endpoint,
    };

    // A `Submit` mode handed a key-less signer is §19's failure waiting to happen.
    let mismatched = ExecutionStage::new(
        abilities.clone(),
        Signer::without_key(ExecutionMode::BuildOnly),
        ExecutionSetup {
            mode: ExecutionMode::Submit,
            ..Default::default()
        },
        CHAIN,
        Clock::new(),
        ExecutionJournal::volatile(),
    );
    match mismatched {
        Err(ExecutionError::ModeGate(why)) => assert!(why.contains("one run has one mode")),
        Ok(_) => panic!("a mode mismatch must be refused"),
        Err(other) => panic!("a mode mismatch must be a mode gate, got {other}"),
    }

    let wrong_chain = ExecutionStage::new(
        abilities,
        Signer::without_key(ExecutionMode::BuildOnly),
        ExecutionSetup {
            mode: ExecutionMode::BuildOnly,
            build: BuildPolicy {
                expected_chain_id: CHAIN,
                ..Default::default()
            },
            ..Default::default()
        },
        42_424_242,
        Clock::new(),
        ExecutionJournal::volatile(),
    );
    match wrong_chain {
        Err(ExecutionError::ChainMismatch(why)) => {
            assert!(why.contains("91342") && why.contains("42424242"), "{why}")
        }
        Ok(_) => panic!("a chain mismatch must be refused"),
        Err(other) => panic!("a chain mismatch must be a chain error, got {other}"),
    }
}
