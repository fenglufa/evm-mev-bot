//! M12-D §4: what survives a restart, and what must never be reused after one.
//!
//! The five cases §4 names are all about the *lifetime* of a value rather than the value
//! itself, so each test below puts one decision on one side of a boundary — a node that
//! answers differently, or a stage that was never given the earlier attempt — and then asks
//! what the code does with it. Three of them are the "must be kept" direction (a submitted
//! transaction's tracking, an execution's identity) and three are the "must not be reused"
//! direction (a stale block pin, a nonce the chain says is in flight, a balance nobody read).
//!
//! What is deliberately *not* here: a second execution state machine. Every test drives the
//! same [`ExecutionStage`], [`Ledger`], [`ExecutionLane`]-style allocator and gate that
//! `stage_matrix` and `lane_matrix` drive, over the §40 scripted endpoint. The only new
//! machinery is the ability to move one scripted node's nonce views between two attempts,
//! which is how "the node restarted" is represented at all: no stage in this file shares a
//! ledger with another, so a fact that carries across a restart does so because the *chain*
//! re-reads it, not because a process remembered it.
//!
//! §8's constraint holds throughout: the synthetic scalar-1 key, scripted answers, no real
//! funds, no real chain, no filesystem writes.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    Abilities, BuildPolicy, ChainReader, Claim, EndpointKind, ExecutionError, ExecutionKey,
    ExecutionMode, ExecutionRecord, ExecutionSetup, ExecutionStage, ExecutionStatus, FeePolicy,
    FeeReading, FeeSource, LaneRelease, Ledger, NonceReading, NonceSource, Receipt, ReceiptPolicy,
    ReceiptStatus, SignedTransaction, Signer, StageReport, SubmissionOutcome, TransactionIntent,
    TransactionSubmitter, TransactionType, UnsignedTransaction,
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
const MINED_BLOCK: u64 = BLOCK + 1;

/// The fee numbers measured in `data/evidence/m6/probe-read-surface-2.txt`, so the bill a
/// test computes is a real bill for this chain rather than a made-up one.
const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
const BALANCE_WEI: u128 = 20_000_000_000_000_000;

const LABEL: &str = "M12-D state lifetime transaction";

/// The block hash the candidate pins its decision to.
fn pinned_hash() -> B256 {
    B256::left_padding_from(&[7u8; 20])
}

/// What a restarted node answers at that same height when it reorged while it was down.
fn reorged_hash() -> B256 {
    B256::left_padding_from(&[0xeeu8; 20])
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
        max_priority_fee_per_gas: Some(U256::from(TIP)),
        max_fee_per_gas: Some(U256::from(BASE_FEE * 2 + TIP)),
    }
}

/// One candidate, decided against `pinned_hash()`. Cloning it is what "carried across the
/// restart" means: the same opportunity, the same simulation identity, the same state
/// binding — the value a restarted process would have to re-earn the right to spend.
fn validation_intent(sender: Address) -> TransactionIntent {
    TransactionIntent::validation(
        BlockPin::new(BlockNumber(BLOCK), pinned_hash()),
        sender,
        &unsigned(),
    )
    .expect("a validation intent over a call transaction")
}

/// The §40 endpoint, extended with the one thing `stage_matrix` does not need: nonce views
/// that can move *between* attempts, because that is the only way to write "the node
/// restarted" against a stage that is still running.
struct Scripted {
    endpoint: EndpointKind,
    allowed: bool,
    chain_id: u64,
    base_fee: U256,
    tip: U256,
    balance: U256,
    /// `(confirmed, pending)`, both views of one moment, and movable.
    nonce: Mutex<(u64, u64)>,
    blocks: HashMap<u64, B256>,
    binding_reads: AtomicUsize,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    sent: Mutex<Vec<Vec<u8>>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
}

impl Scripted {
    fn new(mode: ExecutionMode) -> Self {
        Self {
            endpoint: EndpointKind::PublicHttpRpc,
            allowed: mode.may_submit(),
            chain_id: CHAIN,
            base_fee: U256::from(BASE_FEE),
            tip: U256::from(TIP),
            balance: U256::from(BALANCE_WEI),
            nonce: Mutex::new((0, 0)),
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

    /// The node's two nonce views at the moment this endpoint is first read.
    fn with_nonces(self, confirmed: u64, pending: u64) -> Self {
        *self.nonce.lock().expect("an unlocked pair") = (confirmed, pending);
        self
    }

    /// The same node, later: a restart, a mined block, or a pool that emptied.
    fn set_nonces(&self, confirmed: u64, pending: u64) {
        *self.nonce.lock().expect("an unlocked pair") = (confirmed, pending);
    }

    /// The height a candidate pins to, answered with a different block.
    fn reorg_the_pinned_block(self) -> Self {
        Self {
            blocks: HashMap::from([(BLOCK, reorged_hash()), (MINED_BLOCK, mined_block_hash())]),
            ..self
        }
    }

    /// The node no longer holds `number` at all — the shape a restart that has not caught up
    /// leaves behind.
    fn without_height(self, number: u64) -> Self {
        let mut blocks = self.blocks.clone();
        blocks.remove(&number);
        Self { blocks, ..self }
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }

    fn binding_reads(&self) -> usize {
        self.binding_reads.load(Ordering::SeqCst)
    }

    /// The payload bytes the node was handed, in the order it got them.
    fn payload(&self, index: usize) -> Vec<u8> {
        self.sent
            .lock()
            .expect("an unlocked counter")
            .get(index)
            .cloned()
            .expect("this run sent that payload")
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
        let (confirmed, pending) = *self.nonce.lock().expect("an unlocked pair");
        Ok(NonceReading {
            address,
            confirmed,
            pending,
            at_block: BLOCK,
            source: format!(
                "scripted eth_getTransactionCount, confirmed {confirmed} and pending {pending}, \
                 in M12-D's state-lifetime matrix"
            ),
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

/// One process: a stage over a node, with the receipt budget cut to two fast reads so an
/// unanswered send costs milliseconds instead of §26's real twelve seconds.
fn assemble(endpoint: Scripted, mode: ExecutionMode) -> (ExecutionStage, Arc<Scripted>) {
    let scripted = Arc::new(endpoint);
    (stage_over(&scripted, mode), scripted)
}

/// A stage that shares a node with an earlier stage but nothing else: no ledger, no lane.
/// This is what a restart is from the code's side.
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
    ExecutionStage::new(abilities, test_signer(mode), setup, CHAIN, Clock::new())
        .expect("a stage over a scripted endpoint")
}

fn accepted() -> SubmissionOutcome {
    SubmissionOutcome::Accepted {
        transaction_hash: None,
        endpoint: EndpointKind::PublicHttpRpc,
        detail: "scripted eth_sendRawTransaction acknowledgement".to_string(),
    }
}

/// A receipt in the shape the M6 probe measured on this chain, L1 fields included.
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
        provenance: "scripted eth_getTransactionReceipt in M12-D's matrix".to_string(),
    }
}

fn held(stage: &ExecutionStage, report: &StageReport) -> ExecutionRecord {
    stage
        .ledger()
        .get(report.execution_id.as_ref().expect("the run made a record"))
        .expect("the ledger holds the record it claimed")
        .clone()
}

async fn run(
    stage: &mut ExecutionStage,
    metrics: &mut Metrics,
    intent: &TransactionIntent,
) -> StageReport {
    stage.on_validation(intent.clone(), LABEL, metrics).await
}

/// §4's case 1, both directions of the only fact that matters: a candidate is authorised
/// against *a block*, and a height outlives a block. The control run — same value, node that
/// still holds the hash — gets the transaction sent, so the refusals below are about the
/// binding and not about the intent being malformed or the mode forbidding a send.
#[tokio::test]
async fn a_candidate_carried_into_a_restarted_node_is_refused_before_a_send() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let mut metrics = Metrics::default();

    // Control: the node still holds the block the candidate was decided against.
    let (mut before, before_node) = assemble(
        Scripted::new(ExecutionMode::Submit).answer(accepted()),
        ExecutionMode::Submit,
    );
    let ok = run(&mut before, &mut metrics, &intent).await;
    assert_eq!(ok.reached, Some(ExecutionStatus::Submitted));
    assert_eq!(before_node.sent_count(), 1, "the control did send");

    // Restart one way: the height is now a different block.
    let (mut stage, reorged) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .reorg_the_pinned_block(),
        ExecutionMode::Submit,
    );
    let reorg = run(&mut stage, &mut metrics, &intent).await;

    assert_eq!(reorg.reached, Some(ExecutionStatus::Failed));
    assert!(
        reorg.detail.contains("block_binding_valid"),
        "{}",
        reorg.detail
    );
    assert!(
        reorg.detail.contains("no longer exists"),
        "{}",
        reorg.detail
    );
    let found = format!("{:#x}", reorged_hash());
    assert!(
        reorg.detail.contains(&found),
        "and the refusal names the block it found: {}",
        reorg.detail
    );
    assert_eq!(
        reorged.sent_count(),
        0,
        "no bytes reached the restarted node"
    );
    assert!(reorg.signed.is_none());
    assert!(stage.lane_is_idle(), "a refused pin spends no nonce");
    assert_eq!(
        reorged.binding_reads(),
        1,
        "the fresh attempt asked the node which block the height is, rather than trusting \
         the pin it carried in"
    );
    let record = held(&stage, &reorg);
    assert_eq!(record.status, ExecutionStatus::Failed);
    assert!(!record.was_sent(), "a refused pin never became a send");
    // The other half of §4's case 1: nothing from the earlier process is here to be reused.
    assert!(
        stage
            .ledger()
            .by_transaction(
                ok.transaction_hash
                    .expect("the control run names the transaction it sent")
            )
            .is_none(),
        "a new process holds no tracking of the old one's send, which is why the stale pin \
         has to be refused by a re-read rather than by a memory"
    );

    // Restart the other way: the node does not have that height yet.
    let (mut stage, behind) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .without_height(BLOCK),
        ExecutionMode::Submit,
    );
    let unverified = run(&mut stage, &mut metrics, &intent).await;
    assert_eq!(unverified.reached, Some(ExecutionStatus::Failed));
    assert!(
        unverified.detail.contains("not verified"),
        "{}",
        unverified.detail
    );
    assert_eq!(behind.sent_count(), 0);
    assert!(stage.lane_is_idle());
}

/// §4's case 2 plus case 5's join key: re-deciding the candidate against a new block is a
/// *new execution*, and the record of the send that already happened is untouched by it —
/// including the hash that every later lookup joins on.
#[test]
fn re_pinning_a_candidate_after_a_restart_starts_a_new_execution_and_keeps_the_old_record() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let mut ledger = Ledger::new();

    let Claim::New(first) = ledger.claim(&intent, ExecutionStatus::Detected, 0) else {
        panic!("the first claim on a fresh ledger is new");
    };
    for (step, at) in [
        (ExecutionStatus::Built, 10),
        (ExecutionStatus::Signed, 20),
        (ExecutionStatus::Submitted, 30),
    ] {
        ledger
            .advance(&first.execution_id, step, at)
            .unwrap_or_else(|error| panic!("{step:?}: {error}"));
    }
    let hash = B256::left_padding_from(&[0x5au8; 20]);
    ledger
        .attach_transaction_hash(&first.execution_id, hash)
        .expect("the record is in the ledger");
    let sent = ledger.get(&first.execution_id).expect("the record").clone();
    let written = serde_json::to_value(&sent).expect("a record serializes");

    // The same candidate, pinned to the block a restarted node now reports.
    let mut repinned = intent.clone();
    repinned.block_hash = reorged_hash();
    assert_ne!(
        repinned.state_binding(),
        intent.state_binding(),
        "the pinned hash is inside the binding, so a re-pinned candidate cannot claim the \
         earlier execution's slot"
    );
    let Claim::New(second) = ledger.claim(&repinned, ExecutionStatus::Detected, 40) else {
        panic!("a different state binding is a different execution");
    };
    assert_ne!(second.execution_id, first.execution_id);
    assert_ne!(second.idempotency_key, first.idempotency_key);
    assert_eq!(
        ledger.len(),
        2,
        "both exist; the restart did not overwrite the send"
    );
    let second_record = ledger
        .get(&second.execution_id)
        .expect("the new execution has its own record");
    assert_eq!(
        second_record.opportunity_id, intent.ids.opportunity_id,
        "it is the same opportunity"
    );
    assert_eq!(
        second_record.simulation_id, intent.ids.simulation_id,
        "and the same simulation identity"
    );
    assert_eq!(second_record.state_fingerprint, intent.state_fingerprint);
    assert_eq!(second_record.status, ExecutionStatus::Detected);
    assert_ne!(
        second_record.opportunity_block_hash, sent.opportunity_block_hash,
        "while the block it names is the one the restarted node reports"
    );

    // The submitted record is exactly what it was, and still joins by hash.
    let unchanged = ledger.get(&first.execution_id).expect("the old record");
    assert_eq!(unchanged, &sent);
    assert_eq!(
        serde_json::to_value(unchanged).expect("serializes"),
        written,
        "byte for byte in the shape the evidence file uses"
    );
    assert_eq!(unchanged.status, ExecutionStatus::Submitted);
    assert!(unchanged.was_sent());
    assert_eq!(
        ledger
            .by_transaction(hash)
            .expect("the join still works")
            .execution_id,
        first.execution_id
    );

    // Control for the other direction: the *same* binding, claimed again by a second
    // attempt, is §30's duplicate — one record, no second slot.
    let Claim::Existing(existing) = ledger.claim(&intent, ExecutionStatus::Detected, 50) else {
        panic!("the identical triple must not claim a second record")
    };
    assert_eq!(existing.execution_id, first.execution_id);
    assert_eq!(existing.status, ExecutionStatus::Submitted);
    assert_eq!(ledger.len(), 2);
}

/// §4's case 3 and §4's "重连不会重复预留": when the pool empties under a send that was
/// never answered, the lane keeps the nonce and the record keeps the tracking. A node that
/// forgot the transaction is not permission to spend the nonce again.
#[tokio::test]
async fn a_emptied_pending_view_after_a_send_deletes_no_tracking_and_holds_the_lane() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let mut metrics = Metrics::default();

    let endpoint = Scripted::new(ExecutionMode::Submit)
        .answer(accepted())
        .receipt(None)
        .receipt(None);
    let (mut stage, scripted) = assemble(endpoint, ExecutionMode::Submit);
    let first = run(&mut stage, &mut metrics, &intent).await;

    assert_eq!(first.reached, Some(ExecutionStatus::Submitted));
    assert_eq!(first.receipt_answer, Some(ReceiptStatus::Timeout));
    assert!(
        matches!(first.lane, LaneRelease::Held { .. }),
        "{:?}",
        first.lane
    );
    let record = held(&stage, &first);
    let hash = record
        .transaction_hash
        .expect("a submitted record names its transaction");
    assert!(record.was_sent());
    assert!(
        !stage.lane_is_idle(),
        "nonce {} is still reserved",
        record.nonce
    );
    assert_eq!(scripted.sent_count(), 1);

    // The node restarts and its pool is empty again — both views read the same number, the
    // number this lane spent its nonce on *before* the restart.
    scripted.set_nonces(0, 0);
    let second = run(&mut stage, &mut metrics, &intent).await;

    assert!(
        second.detail.contains("single execution lane"),
        "{}",
        second.detail
    );
    assert_eq!(
        scripted.sent_count(),
        1,
        "a cleared pool is not a reason to send the bytes a second time"
    );
    assert_eq!(
        stage.ledger().len(),
        1,
        "and the refusal happened before a claim, so no second record exists either"
    );
    let still_there = stage
        .ledger()
        .by_transaction(hash)
        .expect("the tracking survived the cleanup");
    assert_eq!(still_there.status, ExecutionStatus::Submitted);
    assert!(still_there.was_sent());
    assert_eq!(held(&stage, &first), record, "nothing about the send moved");
    assert!(!stage.lane_is_idle());
    assert_eq!(metrics.get("execution_attempt"), 2);
    assert_eq!(metrics.get("execution_submit_success"), 1, "still one send");

    // Control: the hold is a *wait*, not a leak. One terminal receipt resolves the same
    // intent, on a fresh process, to the same transaction hash.
    let (mut resolved, resolved_node) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .receipt(Some(receipt(true, sender))),
        ExecutionMode::Submit,
    );
    let landed = run(&mut resolved, &mut metrics, &intent).await;
    assert_eq!(landed.reached, Some(ExecutionStatus::Included));
    assert_eq!(landed.transaction_hash, Some(hash));
    // The other half of the control, asserted here so a reader can see which line carries it:
    // a run that got a receipt gives the lane back.
    assert_eq!(landed.lane, LaneRelease::Released);
    assert!(resolved.lane_is_idle());
    assert_eq!(resolved_node.sent_count(), 1);
    assert_eq!(
        resolved_node.payload(0),
        scripted.payload(0),
        "the same bytes both processes sent — which is precisely why only the chain's answer, \
         not a process's memory, decides whether a nonce may be spent"
    );
}

/// §4's case 4 across a *process* boundary: a fresh stage has no lane to consult, so the bar
/// is what the chain reports. Both views apart (something is in the pool) is a refusal that
/// creates no record; both views together (it was mined) is a new nonce, not a reused one.
#[tokio::test]
async fn a_restarted_process_never_allocates_the_nonce_still_in_flight_and_spends_the_next() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let mut metrics = Metrics::default();

    // Process one spends nonce 0 and leaves it in the pool.
    let (mut one, one_node) = assemble(
        Scripted::new(ExecutionMode::Submit).answer(accepted()),
        ExecutionMode::Submit,
    );
    let sent = run(&mut one, &mut metrics, &intent).await;
    let first_nonce = held(&one, &sent).nonce;
    assert_eq!(first_nonce, 0);
    assert_eq!(one_node.sent_count(), 1);

    // Process two starts while that transaction is still only pending: the confirmed view
    // has not moved, the pending view has.
    let (mut two, two_node) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .with_nonces(0, 1),
        ExecutionMode::Submit,
    );
    let blocked = run(&mut two, &mut metrics, &intent).await;

    assert!(blocked.reached.is_none(), "{:?}", blocked.reached);
    assert!(
        blocked.detail.contains("did not issue"),
        "{}",
        blocked.detail
    );
    assert!(
        blocked.detail.contains("in the pending view"),
        "{}",
        blocked.detail
    );
    assert!(matches!(
        blocked.stopped_with,
        Some(ExecutionError::NonceUnavailable(_))
    ));
    assert_eq!(two_node.sent_count(), 0, "a duplicate was not risked");
    assert!(
        two.ledger().is_empty(),
        "the nonce is read before a claim, so a refusal leaves no record that looks like an \
         attempt"
    );
    assert!(two.lane_is_idle(), "a refusal must not take the lane");

    // Process three: the node mined it, the two views agree again, and the account's next
    // nonce is a number process one never held.
    let (mut three, three_node) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .with_nonces(1, 1),
        ExecutionMode::Submit,
    );
    let second = run(&mut three, &mut metrics, &intent).await;
    let spent = held(&three, &second).nonce;
    assert_eq!(
        spent,
        first_nonce + 1,
        "a different nonce, not a re-allocation"
    );
    assert_eq!(three_node.sent_count(), 1);

    // And a read that cannot be one moment is reported rather than smoothed over: a pending
    // view below the confirmed view would otherwise hand back a nonce that is already used.
    let (mut contradict, contradiction) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .with_nonces(5, 4),
        ExecutionMode::Submit,
    );
    let bad = run(&mut contradict, &mut metrics, &intent).await;
    assert!(
        bad.detail.contains("did not come from one moment"),
        "{}",
        bad.detail
    );
    assert_eq!(contradiction.sent_count(), 0);
    assert!(contradict.ledger().is_empty());
    assert!(contradict.lane_is_idle());

    // The four attempts are counted, and only the two that touched the chain got a record:
    // `execution_stopped` without a paired claim is the signature of §4's "no second
    // reservation", so a reader can tell it apart from a run that failed after a build.
    assert_eq!(metrics.get("execution_attempt"), 4);
    assert_eq!(metrics.get("execution_stopped"), 2);
    assert_eq!(metrics.get("execution_submit_success"), 2);
    assert_eq!(metrics.get("execution_gate_blocked"), 0);
}

/// §4's funding half of case 4: the bill is re-computed and re-afforded on every attempt, so
/// a restarted process cannot spend against money it never read. The threshold is not typed
/// in here — it is the cost the control run's own record measured.
#[tokio::test]
async fn a_restarted_process_stops_when_the_balance_it_read_cannot_cover_the_bill() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let mut metrics = Metrics::default();

    // Control: the account can pay for the transaction, and does.
    let (mut funded, funded_node) = assemble(
        Scripted::new(ExecutionMode::Submit).answer(accepted()),
        ExecutionMode::Submit,
    );
    let paid = run(&mut funded, &mut metrics, &intent).await;
    let bill = held(&funded, &paid)
        .estimated_execution_cost_wei
        .expect("a priced intent carries its own maximum spend");
    assert!(bill > U256::ZERO);
    assert_eq!(funded_node.sent_count(), 1);

    // Restart: gas was spent, and the account now holds one wei less than this bill. The
    // fee reading is the same, so the same transaction is on the table.
    let short = bill - U256::from(1u64);
    let (mut stage, broke) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .with_balance(short),
        ExecutionMode::Submit,
    );
    let report = run(&mut stage, &mut metrics, &intent).await;

    assert_eq!(report.reached, Some(ExecutionStatus::Failed));
    assert!(
        report.detail.contains("balance_sufficient"),
        "{}",
        report.detail
    );
    assert!(
        report.detail.contains(&format!("{short} wei available")),
        "{}",
        report.detail
    );
    assert!(
        report.detail.contains(&format!("{bill} wei is the")),
        "{}",
        report.detail
    );
    let record = held(&stage, &report);
    assert_eq!(
        record.estimated_execution_cost_wei,
        Some(bill),
        "the restarted process re-priced the same bill it was told it could not pay"
    );
    assert_eq!(record.status, ExecutionStatus::Failed);
    assert_eq!(broke.sent_count(), 0);
    assert!(report.signed.is_none());
    assert!(
        stage.lane_is_idle(),
        "the nonce was allocated and then refused, so it goes back"
    );
    assert_eq!(metrics.get("execution_gate_blocked"), 1);

    // Control for the boundary itself: at exactly the bill the run is admitted, so the
    // refusal above is the comparison and not an off-by-one in the test.
    let (mut enough, enough_node) = assemble(
        Scripted::new(ExecutionMode::Submit)
            .answer(accepted())
            .with_balance(bill),
        ExecutionMode::Submit,
    );
    let at_the_bill = run(&mut enough, &mut metrics, &intent).await;
    assert_eq!(at_the_bill.reached, Some(ExecutionStatus::Submitted));
    assert_eq!(enough_node.sent_count(), 1);
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("a package under crates/ sits two levels below the workspace root")
        .to_path_buf()
}

/// One crate's `.rs` files, gathered deterministically.
fn source_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("the crate directory is readable");
    let mut paths = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            source_files(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            into.push(path);
        }
    }
}

/// A production file's text: everything above its own `#[cfg(test)]` gate, with comment
/// lines dropped so a `///` example or a `//` note is never counted as a call site.
fn production_text(text: &str) -> String {
    let body = match text.find("\n#[cfg(test)]") {
        Some(at) => &text[..at],
        None => text,
    };
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every production file in the workspace, keyed by its path relative to the root.
fn production_files() -> Vec<(String, String)> {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut crates = fs::read_dir(&crates_dir)
        .expect("crates/ is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    crates.sort();
    let mut files = Vec::new();
    for crate_dir in crates {
        let src = crate_dir.join("src");
        if !src.is_dir() {
            continue;
        }
        let mut paths = Vec::new();
        source_files(&src, &mut paths);
        for path in paths {
            let text = fs::read_to_string(&path).expect("a source file is readable");
            let relative = path
                .strip_prefix(&root)
                .expect("every file gathered is under the root")
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, production_text(&text)));
        }
    }
    files
}

fn production_hits_containing(files: &[(String, String)], needles: &[&str]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, text)| {
            text.lines().any(|line| {
                needles
                    .iter()
                    .any(|needle| line.contains(needle) && !line.trim_start().starts_with("//"))
            })
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// §4's case 5, read as a fact about the code rather than about one run: a restarted process
/// has nothing to resume *from*. The lifecycle ledger is per-process, the record is written
/// into evidence and never read back, and the only durable state an attempt is authorised
/// against is the chain's own — which is what the four tests above are re-reading.
#[test]
fn no_production_path_resumes_execution_from_a_record() {
    let files = production_files();
    assert!(
        files.len() > 100,
        "the scan has to see the whole workspace to say anything about it: {} files",
        files.len()
    );

    // (a) The only ledgers production owns are the two ladders that create them.
    let ledgers = production_hits_containing(&files, &["Ledger::new()"]);
    assert_eq!(
        ledgers,
        vec![
            "crates/execution/src/sequence.rs".to_string(),
            "crates/execution/src/stage.rs".to_string(),
        ],
        "a third ledger would mean a second execution state machine"
    );

    // (b) No production line deserializes a lifecycle record — the direction a resume would
    //     need. All three generic forms are checked because `from_str` and `from_value` are
    //     different ways of writing the same intention.
    let resumed = production_hits_containing(
        &files,
        &[
            "from_value::<ExecutionRecord>",
            "from_str::<ExecutionRecord>",
            "from_slice::<ExecutionRecord>",
            "from_value::<Vec<ExecutionRecord>>",
        ],
    );
    assert!(
        resumed.is_empty(),
        "execution is resumed from a record in {resumed:?}, which contradicts §4"
    );

    // (c) Positive controls — the scanner can see each of these directions when it exists,
    //     so (b) is a finding rather than a blind spot.
    let decoders = production_hits_containing(&files, &["chain_block_from_value"]);
    assert!(
        decoders.contains(&"crates/chain/src/head.rs".to_string()),
        "the scan must find a real production deserializer: {decoders:?}"
    );
    let writers = production_hits_containing(&files, &["EvidenceFile::Executions"]);
    assert!(
        writers.contains(&"crates/pipeline/src/arbitrage.rs".to_string()),
        "the scan must find the one-way write of the executions row: {writers:?}"
    );

    // (d) And the `#[cfg(test)]` cut is load-bearing: lifecycle.rs constructs ledgers four
    //     times, every one of them below its own test gate.
    let lifecycle = workspace_root().join("crates/execution/src/lifecycle.rs");
    let text = fs::read_to_string(&lifecycle).expect("lifecycle.rs is readable");
    let raw = text.matches("Ledger::new()").count();
    let production = production_text(&text).matches("Ledger::new()").count();
    assert_eq!(raw, 4, "the planted control assumes four sites: {raw}");
    assert_eq!(
        production, 0,
        "every one of them is a test, so the scan above measured production only"
    );
}

/// §4's case 3 read at the artifact layer: the identity a restarted process re-derives from
/// the transaction's own content is the identity the evidence recorded before the restart, so
/// a log line and a fresh record name one execution even though nothing was carried over.
#[test]
fn the_identity_a_restart_rederives_names_the_execution_the_evidence_recorded() {
    let sender = synthetic_address();
    let intent = validation_intent(sender);
    let hash = B256::left_padding_from(&[0x5au8; 20]);

    let mut before = Ledger::new();
    let Claim::New(first) = before.claim(&intent, ExecutionStatus::Detected, 1_000) else {
        panic!("the first claim on a fresh ledger is new");
    };
    for (step, at) in [
        (ExecutionStatus::Built, 1_010),
        (ExecutionStatus::Signed, 1_020),
        (ExecutionStatus::Submitted, 1_030),
    ] {
        before
            .advance(&first.execution_id, step, at)
            .unwrap_or_else(|error| panic!("{step:?}: {error}"));
    }
    before
        .attach_transaction_hash(&first.execution_id, hash)
        .expect("the record is in the ledger");

    // What the evidence file holds, read back as a value.
    let written = before.to_json();
    let reread: Vec<ExecutionRecord> =
        serde_json::from_value(written.clone()).expect("the evidence row deserializes");
    assert_eq!(reread.len(), 1);
    let reread = reread.first().expect("one row");
    assert_eq!(
        reread,
        before
            .get(&first.execution_id)
            .expect("the ledger holds what it wrote")
    );
    let row = written
        .as_array()
        .expect("the ledger's evidence shape is an array")
        .first()
        .expect("one row")
        .as_object()
        .expect("an execution record is an object");
    for field in [
        "transaction_hash",
        "opportunity_block",
        "opportunity_block_hash",
        "state_fingerprint",
        "nonce",
        "status",
        "blocked_reason",
        "failure",
    ] {
        assert!(
            row.contains_key(field),
            "the evidence row carries no {field} key, so a reader after a restart could not \
             ask the question this milestone is about"
        );
    }
    assert_eq!(
        row["status"],
        serde_json::json!(ExecutionStatus::Submitted.name()),
        "and the rung is written as the word a reader of the file sees"
    );
    assert_eq!(
        row["transaction_hash"],
        serde_json::to_value(hash).expect("a hash serializes"),
        "with the hash every later lookup joins on"
    );

    // A restarted process re-derives the same identity from the same content.
    let mut after = Ledger::new();
    let Claim::New(again) = after.claim(&intent, ExecutionStatus::Detected, 1_000) else {
        panic!("an empty ledger cannot hold the earlier claim");
    };
    assert_eq!(again.execution_id, reread.execution_id);
    assert_eq!(again.idempotency_key, reread.idempotency_key);
    for (step, at) in [
        (ExecutionStatus::Built, 1_010),
        (ExecutionStatus::Signed, 1_020),
        (ExecutionStatus::Submitted, 1_030),
    ] {
        after
            .advance(&again.execution_id, step, at)
            .unwrap_or_else(|error| panic!("{step:?}: {error}"));
    }
    after
        .attach_transaction_hash(&again.execution_id, hash)
        .expect("the record is in the ledger");

    // Artifact-level equality, not just field-level: the bytes a restart would have resumed
    // from and the bytes it writes are the same.
    assert_eq!(after.to_json(), written);

    // Control for the other half of §4's case 1: re-pin the block and the identity moves,
    // which is why re-deriving from content is safe while reusing a remembered send is not.
    let mut repinned = intent.clone();
    repinned.block_hash = reorged_hash();
    let mut third = Ledger::new();
    let Claim::New(other) = third.claim(&repinned, ExecutionStatus::Detected, 1_000) else {
        panic!("a different binding is a different execution");
    };
    assert_ne!(other.execution_id, reread.execution_id);

    // The class-2 fields the reader of a restart cares about survive the round trip
    // individually, with the hash they join on.
    assert_eq!(reread.transaction_hash, Some(hash));
    assert_eq!(reread.status, ExecutionStatus::Submitted);
    assert_eq!(reread.opportunity_block, BLOCK);
    assert_eq!(reread.opportunity_block_hash, pinned_hash());
    assert_eq!(reread.nonce, intent.nonce);
    assert_eq!(reread.state_fingerprint, intent.state_fingerprint);
    assert_eq!(reread.failure, None);
    assert_eq!(reread.blocked_reason, None);
    assert!(reread.was_sent());
    assert_eq!(
        reread.state_fingerprint,
        format!("validation block {BLOCK}"),
        "and the fingerprint says which block it was decided against"
    );
}
