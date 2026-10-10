//! §40's matrix: the three submission answers, the four receipt answers, and the three
//! ways a run is supposed to stop before it sends.
//!
//! §40 permits a mock here, and only here: the *interface* to the endpoint can be
//! scripted, because what this file tests is what our code does with an answer, not
//! whether the endpoint exists. Everything else is the real thing — the synthetic key
//! (scalar 1) is the only invented input; the builder, the signer, the gate, the ledger,
//! the lane and the receipt tracker are `crates/execution/src`, and the receipt keeps the
//! shape measured in `data/evidence/m6/probe-submission-surface.txt`, L1 fee fields
//! included. Real-chain submission is §41's job and is recorded separately; nothing in
//! this file is evidence that a broadcast worked.
//!
//! The three stop-early cases share one assertion style: after the refusal the run must
//! hold *no bytes*, and checking that a thing did not happen is only meaningful when the
//! check can fail, so each of them also asks the real signer or the real gate directly and
//! reads its answer.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    BalanceEvidence, BlockBinding, Build, BuildPolicy, Claim, EndpointKind, ExecutionError,
    ExecutionKey, ExecutionLane, ExecutionMode, ExecutionRecord, ExecutionStatus,
    ExpectedTransaction, Freshness, GasPolicy, GateAttempt, GateCheck, GateFacts, GateOutcome,
    LaneRelease, Ledger, NonceEvidence, NonceReading, PreSubmitGate, Receipt, ReceiptPolicy,
    ReceiptStatus, ReceiptTracker, SenderFunding, SignedTransaction, Signer, SubmissionOutcome,
    TrackedReceipt, TransactionBuilder, TransactionIntent, TransactionSubmitter, TransactionType,
    UnsignedTransaction,
};
use evm_simulation::BlockPin;

/// §40's synthetic key: the scalar one, never the operator's wallet.
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
const BLOCK: u64 = 37_486_792;
/// The block this matrix expects the transaction to land in, one past the pinned head.
const MINED_BLOCK: u64 = BLOCK + 1;

fn target() -> Address {
    Address::from_slice(&[0x6bu8; 20])
}

/// The block hash the scripted endpoint holds at [`MINED_BLOCK`] unless a test says
/// otherwise — and the hash the scripted receipt claims, so they agree by construction.
fn canonical_block() -> B256 {
    B256::left_padding_from(&[9u8; 20])
}

/// The hash the intent pins, in every case in this file.
fn pinned_hash() -> B256 {
    B256::left_padding_from(&[7u8; 20])
}

fn signer(mode: ExecutionMode) -> Signer {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("a synthetic key is in range");
    Signer::from_key(mode, key)
}

fn unsigned(nonce: u64) -> UnsignedTransaction {
    UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id: CHAIN,
        nonce,
        to: Some(target()),
        value: U256::ZERO,
        gas_limit: 21_000,
        input: Bytes::from(vec![0x12u8, 0x34, 0x56, 0x78]),
        access_list: Vec::new(),
        max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
        max_fee_per_gas: Some(U256::from(1_000_370u64)),
    }
}

fn intent(nonce: u64, sender: Address) -> TransactionIntent {
    TransactionIntent::validation(
        BlockPin::new(BlockNumber(BLOCK), pinned_hash()),
        sender,
        &unsigned(nonce),
    )
    .expect("a validation intent over a call transaction")
}

/// The §13 policy for this chain, with the block gas limit and the fee numbers measured in
/// `data/evidence/m6/` rather than chosen here.
fn policy() -> BuildPolicy {
    BuildPolicy {
        expected_chain_id: CHAIN,
        maximum_gas_limit: 60_000_000,
        maximum_calldata_bytes: 4_096,
        require_unoverridden_state: true,
        gas: GasPolicy::Configured { gas_limit: 21_000 },
        simulated_gas_used: None,
    }
}

/// The build, sign and re-decode §14 already proves, assembled here so the matrix below
/// starts from a transaction that is real in every field.
fn prepare(nonce: u64, mode: ExecutionMode) -> Prepared {
    let signer = signer(mode);
    let sender = signer
        .address()
        .expect("a signer built from a key knows its address");
    let intent = intent(nonce, sender);
    let build = TransactionBuilder::build(&intent, &policy()).expect("the intent builds");
    let signed = signer.sign(&build.unsigned).expect("signing");
    let round_trip =
        TransactionBuilder::round_trip(&build, &signed).expect("the bytes describe the intent");
    Prepared {
        signer,
        sender,
        intent,
        build,
        signed,
        hash: round_trip.hash,
    }
}

struct Prepared {
    signer: Signer,
    sender: Address,
    intent: TransactionIntent,
    build: Build,
    signed: SignedTransaction,
    hash: B256,
}

fn reading(address: Address, confirmed: u64, pending: u64) -> NonceReading {
    NonceReading {
        address,
        confirmed,
        pending,
        at_block: BLOCK,
        source: "scripted read in the §40 matrix".to_string(),
    }
}

/// The seven §32 facts, all in their passing shape. The baseline has to pass or the
/// refusal cases below prove nothing, so the first test says so out loud.
fn passing_facts(run: &Prepared) -> GateFacts {
    GateFacts {
        attempt: GateAttempt::Arbitrage {
            simulation_success: true,
            risk_accepted: true,
            freshness: Freshness::Active,
        },
        intent_chain_id: CHAIN,
        configured_chain_id: CHAIN,
        endpoint_chain_id: CHAIN,
        binding: BlockBinding::Confirmed {
            number: BLOCK,
            hash: pinned_hash(),
        },
        balance: BalanceEvidence::Sufficient {
            available_wei: U256::from(20_000_000_000_000u64),
            maximum_spend_wei: run
                .intent
                .unsigned()
                .maximum_cost_wei()
                .expect("a priced transaction"),
            source: "eth_getBalance at #37486791".to_string(),
        },
        nonce: NonceEvidence::Matches {
            nonce: run.build.unsigned.nonce,
            source: "eth_getTransactionCount(addr, \"pending\")".to_string(),
        },
    }
}

/// The scripted endpoint. It answers `submit` from a queue, hands receipts out of a
/// second queue, and remembers every payload it was handed — so "one send, not two" and
/// "no bytes at all" are countable facts rather than hopes.
///
/// Two of its behaviours are this file's own code rather than the crate's, and both are
/// written to match a documented contract instead of to make a test pass: it refuses to
/// send when `may_submit()` says no (the same guard
/// `crates/execution/src/giwa/sequencer_direct.rs` runs, and the trait's §20 requirement
/// at `submitter.rs`), and it stamps the requested hash onto the receipt it returns,
/// which is what `eth_getTransactionReceipt` does — a node does not answer a lookup for
/// one transaction with another's receipt unless it is buggy, which is what the
/// `verbatim` case below goes out of its way to simulate.
struct Scripted {
    endpoint: EndpointKind,
    allowed: bool,
    /// Answer lookups with the queued receipt exactly as stored, hash field included.
    verbatim: bool,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    sent: Mutex<Vec<Vec<u8>>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
    /// The block hash `eth_getBlockByNumber` would answer with at [`MINED_BLOCK`]. A
    /// different value from the receipt's is a reorg.
    canonical: Mutex<Option<B256>>,
}

impl Scripted {
    fn new(answers: Vec<SubmissionOutcome>, receipts: Vec<Option<Receipt>>) -> Self {
        Self {
            endpoint: EndpointKind::PublicHttpRpc,
            allowed: true,
            verbatim: false,
            answers: Mutex::new(answers.into()),
            sent: Mutex::new(Vec::new()),
            receipts: Mutex::new(receipts.into()),
            canonical: Mutex::new(None),
        }
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }

    fn first_payload(&self) -> Vec<u8> {
        self.sent
            .lock()
            .expect("an unlocked counter")
            .first()
            .cloned()
            .expect("the mock recorded a payload")
    }

    fn set_canonical(&self, hash: B256) {
        *self.canonical.lock().expect("an unlocked slot") = Some(hash);
    }

    /// What the endpoint holds at the height a receipt names.
    fn block_hash_at(&self, number: u64) -> Option<B256> {
        let named = {
            let slot = self.canonical.lock().expect("an unlocked slot");
            *slot
        };
        match named {
            Some(hash) => Some(hash),
            None if number == MINED_BLOCK => Some(canonical_block()),
            None => None,
        }
    }
}

#[async_trait]
impl TransactionSubmitter for Scripted {
    fn endpoint(&self) -> EndpointKind {
        self.endpoint
    }

    fn may_submit(&self) -> bool {
        self.allowed && self.endpoint.may_broadcast()
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
            .unwrap_or_else(|| SubmissionOutcome::Rejected {
                reason: "the script ran out; §25 forbids guessing here".to_string(),
                endpoint: self.endpoint,
            }))
    }

    async fn receipt(&self, transaction_hash: B256) -> evm_execution::Result<Option<Receipt>> {
        let next = self.receipts.lock().expect("an unlocked queue").pop_front();
        Ok(next.flatten().map(|mut receipt| {
            if !self.verbatim {
                receipt.transaction_hash = transaction_hash;
            }
            receipt
        }))
    }
}

/// A receipt in the shape the probe measured on this chain, including the L1 fields.
fn receipt(success: bool, sender: Address) -> Receipt {
    Receipt {
        transaction_hash: B256::ZERO,
        block_number: MINED_BLOCK,
        block_hash: canonical_block(),
        transaction_index: 2,
        success,
        gas_used: 21_000,
        effective_gas_price: U256::from(1_000_370u64),
        cumulative_gas_used: Some(U256::from(84_000u64)),
        from: sender,
        to: Some(target()),
        contract_address: None,
        tx_type: Some(2),
        logs: Vec::new(),
        l1_fee: Some(U256::from(7_400_000_000u64)),
        l1_gas_price: Some(U256::from(1_088_519_061u64)),
        l1_gas_used: Some(U256::from(1_600u64)),
        l1_base_fee_scalar: Some(U256::from(1_368u64)),
        l1_blob_base_fee: Some(U256::from(62_294_004u64)),
        l1_blob_base_fee_scalar: Some(U256::from(801_949u64)),
        provenance: "eth_getTransactionReceipt in the §40 matrix".to_string(),
    }
}

fn tracker(attempts: usize) -> ReceiptTracker {
    ReceiptTracker::new(ReceiptPolicy {
        attempts,
        between_attempts: Duration::from_millis(1),
    })
}

fn expected(run: &Prepared) -> ExpectedTransaction {
    ExpectedTransaction {
        transaction_hash: run.hash,
        sender: run.sender,
        target: Some(target()),
        nonce: run.build.unsigned.nonce,
        chain_id: CHAIN,
    }
}

/// Run the real tracker against the scripted endpoint, with the block check the tracker
/// is required to make (§27's second clause).
async fn track(run: &Prepared, submitter: &Arc<Scripted>, attempts: usize) -> TrackedReceipt {
    let hash = run.hash;
    let reads = Arc::clone(submitter);
    let blocks = Arc::clone(submitter);
    tracker(attempts)
        .track(
            &expected(run),
            move || {
                let reads = Arc::clone(&reads);
                async move { reads.receipt(hash).await.map_err(|error| error.to_string()) }
            },
            move |number| {
                let blocks = Arc::clone(&blocks);
                async move { Ok(blocks.block_hash_at(number)) }
            },
        )
        .await
}

fn accepted(hash: B256) -> SubmissionOutcome {
    SubmissionOutcome::Accepted {
        transaction_hash: Some(hash),
        endpoint: EndpointKind::PublicHttpRpc,
        detail: "eth_sendRawTransaction".to_string(),
    }
}

fn rejected() -> SubmissionOutcome {
    SubmissionOutcome::Rejected {
        reason: "nonce too low".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    }
}

fn unknown() -> SubmissionOutcome {
    SubmissionOutcome::Unknown {
        reason: "connection reset before an answer".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    }
}

#[tokio::test]
async fn an_accepted_transaction_walks_the_ladder_to_inclusion() {
    let run = prepare(7, ExecutionMode::Submit);
    assert_eq!(
        PreSubmitGate::evaluate(&passing_facts(&run)),
        GateOutcome::Passed,
        "the matrix's baseline facts have to pass, or the refusals below prove nothing"
    );

    let mut ledger = Ledger::new();
    let Claim::New(handle) = ledger.claim(&run.intent, ExecutionStatus::Detected, 0) else {
        panic!("the first claim on a fresh ledger is new")
    };
    for (step, at) in [
        (ExecutionStatus::Simulated, 10),
        (ExecutionStatus::RiskApproved, 20),
        (ExecutionStatus::Built, 30),
        (ExecutionStatus::Signed, 40),
    ] {
        ledger
            .advance(&handle.execution_id, step, at)
            .unwrap_or_else(|error| panic!("{step:?}: {error}"));
    }
    // §2.2, as a fact about the record rather than about this file's prose: the best
    // state a signed, never-sent transaction can report is `signed`.
    let untouched = ledger.get(&handle.execution_id).expect("the record");
    assert!(!untouched.was_sent());
    assert_eq!(untouched.transaction_hash, None);

    let submitter = Arc::new(Scripted::new(
        vec![accepted(run.hash)],
        vec![Some(receipt(true, run.sender))],
    ));
    let outcome = submitter
        .submit(&run.signed)
        .await
        .expect("the script answers");
    ledger
        .advance(&handle.execution_id, ExecutionStatus::Submitted, 50)
        .expect("a submitted step follows a signed one");

    // §27: what gets tracked is the hash of the bytes we signed, even though the answer
    // carried one too.
    let tracked_hash = outcome.tracked_hash(run.hash);
    assert_eq!(tracked_hash, run.hash);
    assert_eq!(
        submitter.first_payload(),
        run.signed.raw().to_vec(),
        "the endpoint received exactly the bytes whose hash is in the record"
    );
    ledger
        .attach_transaction_hash(&handle.execution_id, tracked_hash)
        .expect("the record is in the ledger");

    let tracked = track(&run, &submitter, 3).await;
    let TrackedReceipt::Included(bound) = tracked else {
        panic!("a bound success receipt must come back Included, got {tracked:?}")
    };
    let joined = ledger
        .attach_receipt(&bound, 60)
        .expect("the receipt joins by hash");
    assert_eq!(joined, Some(handle.execution_id.clone()));

    let record = ledger
        .by_transaction(run.hash)
        .expect("the join is by hash");
    assert_eq!(record.status, ExecutionStatus::Included);
    assert!(record.was_sent());
    assert_eq!(record.gas_used, Some(21_000));
    assert_eq!(record.effective_gas_price, Some(U256::from(1_000_370u64)));
    assert_eq!(
        record.l1_fee,
        Some(U256::from(7_400_000_000u64)),
        "the L1 bill is a separate number and must survive into the record"
    );
    assert_eq!(
        record.signed_at_ms,
        Some(40),
        "each rung stamps its own time once"
    );
    assert_eq!(record.submitted_at_ms, Some(50));
    assert_eq!(record.included_at_ms, Some(60));
    assert_eq!(submitter.sent_count(), 1, "one send for one transaction");
    assert_eq!(
        ledger.len(),
        1,
        "and the ladder did not create a second record"
    );
}

#[tokio::test]
async fn a_definite_refusal_fails_the_attempt_and_gives_the_nonce_back() {
    let run = prepare(8, ExecutionMode::Submit);
    let mut lane = ExecutionLane::new();
    let nonce = lane
        .allocate(&reading(run.sender, 8, 8))
        .expect("an idle lane takes the pending nonce");
    assert_eq!(nonce, 8);

    let submitter = Scripted::new(vec![rejected()], Vec::new());
    let outcome = submitter
        .submit(&run.signed)
        .await
        .expect("the node answered");
    assert_eq!(outcome, rejected());
    assert!(
        outcome.proven_not_in_flight(),
        "only this answer class proves nothing is in flight"
    );
    assert_eq!(
        lane.resolve_submission(&outcome),
        LaneRelease::Released,
        "a refusal releases the lane"
    );
    assert!(lane.is_idle());
    // §11's whole reason for tracking the nonce: a refused transaction did not spend it,
    // so the next attempt must use the same number rather than skip one.
    let reuse = lane
        .allocate(&reading(run.sender, 8, 8))
        .expect("a released lane can be taken again");
    assert_eq!(reuse, 8, "the nonce was never spent");

    let mut ledger = Ledger::new();
    let Claim::New(handle) = ledger.claim(&run.intent, ExecutionStatus::Signed, 0) else {
        panic!("first claim")
    };
    ledger
        .advance(&handle.execution_id, ExecutionStatus::Submitted, 1)
        .expect("we did hand the bytes over, whatever the node said");
    ledger
        .fail(
            &handle.execution_id,
            &ExecutionError::SubmissionRejected("nonce too low".to_string()),
            2,
        )
        .expect("a submitted record can fail");
    let record = ledger.get(&handle.execution_id).expect("the record");
    assert_eq!(record.status, ExecutionStatus::Failed);
    assert!(
        record.was_sent(),
        "a rejected submission was still sent, and §25's honesty requires the record to say so"
    );
    let failure = record.failure.as_ref().expect("the reason is recorded");
    assert!(
        failure.contains("nonce too low"),
        "the §39 entry carries the node's reason: {failure}"
    );
    assert_eq!(submitter.sent_count(), 1);
}

#[tokio::test]
async fn an_unknown_answer_holds_the_lane_until_a_read_resolves_it() {
    // §25's rule, as a fact about the allocator: we do not know, so we do not send again.
    let run = prepare(9, ExecutionMode::Submit);
    let mut lane = ExecutionLane::new();
    let held = lane
        .allocate(&reading(run.sender, 9, 9))
        .expect("the lane is idle");
    assert_eq!(held, 9);

    let submitter = Scripted::new(vec![unknown()], Vec::new());
    let outcome = submitter.submit(&run.signed).await.expect("no answer");
    assert!(!outcome.proven_not_in_flight());
    let LaneRelease::Held { reason } = lane.resolve_submission(&outcome) else {
        panic!("an unknown answer must not release the lane")
    };
    assert!(
        reason.contains("unknown"),
        "the reason has to name the answer class: {reason}"
    );
    assert_eq!(lane.outstanding(), Some((run.sender, 9)));

    let error = lane
        .allocate(&reading(run.sender, 9, 9))
        .expect_err("a second transaction on a held lane is the duplicate §25 forbids");
    assert!(
        error.to_string().contains("holding nonce 9"),
        "the refusal must name the nonce it is protecting: {error}"
    );
    assert_eq!(submitter.sent_count(), 1, "one send, then a held lane");

    // A timeout on the read does not resolve it either; only a known-absent answer does.
    let LaneRelease::Held { .. } = lane.resolve_receipt(ReceiptStatus::Timeout) else {
        panic!("a receipt timeout leaves the transaction possibly live")
    };
    assert_eq!(
        lane.resolve_receipt(ReceiptStatus::NotFound),
        LaneRelease::Released,
        "a receipt proven absent is the read that ends an unknown submission"
    );
    assert!(lane.is_idle());
}

#[tokio::test]
async fn a_reverted_receipt_is_a_chain_fact_and_not_a_crash() {
    let run = prepare(10, ExecutionMode::Submit);
    let submitter = Arc::new(Scripted::new(
        vec![accepted(run.hash)],
        vec![Some(receipt(false, run.sender))],
    ));
    let tracked = track(&run, &submitter, 3).await;
    let TrackedReceipt::Reverted(bound) = tracked else {
        panic!("status 0x0 is Reverted, got {tracked:?}")
    };
    assert_eq!(bound.outcome(), ReceiptStatus::Reverted);

    let mut record = ExecutionRecord::open(&run.intent, ExecutionStatus::Submitted, 0);
    record.attach_transaction_hash(run.hash);
    record
        .attach_receipt(&bound, 1)
        .expect("a reverted receipt still joins");
    assert_eq!(record.status, ExecutionStatus::Reverted);
    assert!(
        record.status.terminal(),
        "a revert is terminal: the chain ran the transaction"
    );
    assert!(record.was_sent());
    // The block stamp survives, because the transaction did execute (§P).
    assert_eq!(record.included_at_ms, Some(1));
    assert_eq!(record.gas_used, Some(21_000));
}

#[tokio::test]
async fn a_receipt_for_another_senders_transaction_is_unbound() {
    // §27: a receipt for somebody else's transaction is a provider error, and folding it
    // into `Reverted` would make our transaction look reverted when it looks unknown.
    let run = prepare(11, ExecutionMode::Submit);
    let foreign_sender = Address::from_slice(&[0xeeu8; 20]);
    let submitter = Arc::new(Scripted::new(
        vec![accepted(run.hash)],
        vec![Some(receipt(true, foreign_sender))],
    ));
    let tracked = track(&run, &submitter, 3).await;
    let TrackedReceipt::Unbound { reason, .. } = &tracked else {
        panic!("a receipt from another sender does not bind, got {tracked:?}")
    };
    assert!(
        reason.contains("recovers to"),
        "the reason must name both senders: {reason}"
    );
    assert_eq!(tracked.status(), ReceiptStatus::NotFound);
}

#[tokio::test]
async fn a_receipt_for_another_transaction_hash_is_unbound() {
    // The first clause of §27, which the lookup-stamps-the-hash behaviour of a well-behaved
    // node can never exercise: an endpoint that answers a lookup with a receipt for a
    // different hash has told us about a different transaction.
    let run = prepare(12, ExecutionMode::Submit);
    let mut scripted = Scripted::new(vec![accepted(run.hash)], Vec::new());
    scripted.verbatim = true;
    let stranger = receipt(true, run.sender);
    scripted.receipts = Mutex::new(vec![Some(stranger)].into());
    let submitter = Arc::new(scripted);
    let tracked = track(&run, &submitter, 3).await;
    let TrackedReceipt::Unbound { reason, receipt } = tracked else {
        panic!("a receipt naming another transaction must not bind, got {tracked:?}")
    };
    assert_ne!(
        receipt.transaction_hash, run.hash,
        "the mock really did hand back a stranger's receipt"
    );
    assert!(
        reason.contains("not the"),
        "the reason names both hashes: {reason}"
    );
}

#[tokio::test]
async fn a_receipt_in_a_block_the_chain_does_not_name_is_unbound_too() {
    let run = prepare(13, ExecutionMode::Submit);
    let submitter = Arc::new(Scripted::new(
        vec![accepted(run.hash)],
        vec![Some(receipt(true, run.sender))],
    ));
    submitter.set_canonical(B256::left_padding_from(&[0xaau8; 20]));
    let tracked = track(&run, &submitter, 3).await;
    let TrackedReceipt::Unbound { reason, receipt } = tracked else {
        panic!("a non-canonical block must not produce Included, got {tracked:?}")
    };
    assert!(
        reason.contains("canonical"),
        "the reason has to say the block is not canonical: {reason}"
    );
    // The receipt itself is fine — sender, hash, target all bind. It is the block that
    // moved, which is why `Reverted`/`Included` would both be the wrong answer.
    assert_eq!(receipt.from, run.sender);
    assert!(receipt.success);
}

#[tokio::test]
async fn a_missing_receipt_times_out_without_becoming_a_failure() {
    let run = prepare(14, ExecutionMode::Submit);
    let submitter = Arc::new(Scripted::new(vec![accepted(run.hash)], vec![None, None]));
    let tracked = track(&run, &submitter, 2).await;
    let TrackedReceipt::Pending {
        attempts,
        ref last_answer,
    } = tracked
    else {
        panic!("no receipt at all is Pending, got {tracked:?}")
    };
    assert_eq!(attempts, 2, "the budget was spent and counted");
    assert!(
        last_answer.contains("null"),
        "the last answer is what the endpoint actually said: {last_answer}"
    );
    assert_eq!(tracked.status(), ReceiptStatus::Timeout);
    assert!(
        !tracked.status().terminal(),
        "§25: running out of reads is not an answer about the transaction"
    );

    let mut lane = ExecutionLane::new();
    lane.allocate(&reading(run.sender, 14, 14)).expect("idle");
    let LaneRelease::Held { reason } = lane.resolve_receipt(tracked.status()) else {
        panic!("a timeout keeps the nonce; the transaction may still land")
    };
    assert!(reason.contains("stays reserved"), "{reason}");
    // And the waiting itself sent nothing: this test only ran the read loop, and the
    // tracker holds no way to submit — which is why a timeout cannot become the blind
    // retry §25 forbids, rather than merely should not.
    assert_eq!(submitter.sent_count(), 0, "a receipt budget is all read");
}

#[test]
fn a_risk_rejection_stops_before_any_bytes_exist() {
    let run = prepare(15, ExecutionMode::Submit);
    let mut facts = passing_facts(&run);
    facts.attempt = GateAttempt::Arbitrage {
        simulation_success: true,
        risk_accepted: false,
        freshness: Freshness::Active,
    };
    let outcome = PreSubmitGate::evaluate(&facts);
    let GateOutcome::Blocked { failures } = &outcome else {
        panic!("a rejected risk decision blocks")
    };
    assert_eq!(
        failures.first().expect("one failure").check,
        GateCheck::RiskAccepted,
        "the refusal names the check that failed"
    );

    let mut ledger = Ledger::new();
    let Claim::New(handle) = ledger.claim(&run.intent, ExecutionStatus::Simulated, 0) else {
        panic!("first claim")
    };
    // §39: the caller records the gate's own taxonomy entry, not a string of its own.
    let error = outcome.error().expect("a blocked outcome maps to an error");
    assert!(matches!(error, ExecutionError::InvalidIntent(_)), "{error}");
    ledger
        .fail(&handle.execution_id, &error, 1)
        .expect("an unfinished record can fail");
    let record = ledger.get(&handle.execution_id).expect("the record");
    assert_eq!(record.status, ExecutionStatus::Failed);
    assert!(
        !record.was_sent(),
        "a run that never reached Submitted must not claim it sent something"
    );
    assert_eq!(record.transaction_hash, None, "no bytes, no hash");
}

#[test]
fn a_stale_opportunity_blocks_and_build_only_cannot_sign() {
    let run = prepare(16, ExecutionMode::Submit);
    let mut facts = passing_facts(&run);
    facts.attempt = GateAttempt::Arbitrage {
        simulation_success: true,
        risk_accepted: true,
        freshness: Freshness::Stale {
            reason: "the block this state was read at is two behind".to_string(),
        },
    };
    let outcome = PreSubmitGate::evaluate(&facts);
    let GateOutcome::Blocked { failures } = &outcome else {
        panic!("staleness blocks")
    };
    assert!(failures
        .iter()
        .any(|failure| failure.check == GateCheck::OpportunityFresh));
    assert!(
        matches!(
            outcome.error().expect("a blocked outcome maps to an error"),
            ExecutionError::StaleOpportunity(_)
        ),
        "staleness is its own §39 entry, not a build failure"
    );

    // The refusal is not only in the gate's report: the same transaction, handed to a
    // signer in the mode a decision-support run starts in, produces no signature at all.
    let build_only = signer(ExecutionMode::BuildOnly);
    let error = build_only
        .sign(&run.build.unsigned)
        .expect_err("build-only does not sign");
    assert!(matches!(error, ExecutionError::ModeGate(_)), "{error}");
    assert_eq!(build_only.mode(), ExecutionMode::BuildOnly);
    assert_eq!(ExecutionMode::default(), ExecutionMode::BuildOnly);
}

#[test]
fn an_insufficient_balance_blocks_with_both_numbers_in_wei() {
    let run = prepare(17, ExecutionMode::Submit);
    let maximum_spend = run
        .build
        .unsigned
        .maximum_cost_wei()
        .expect("a priced transaction");
    let available = maximum_spend - U256::from(1u64);
    let mut facts = passing_facts(&run);
    facts.balance = BalanceEvidence::Insufficient {
        available_wei: available,
        maximum_spend_wei: maximum_spend,
        source: "eth_getBalance at #37486791".to_string(),
    };
    let GateOutcome::Blocked { failures } = PreSubmitGate::evaluate(&facts) else {
        panic!("a short balance blocks")
    };
    let failure = failures
        .iter()
        .find(|failure| failure.check == GateCheck::BalanceSufficient)
        .expect("the balance failure");
    assert!(
        failure.reason.contains(&maximum_spend.to_string())
            && failure.reason.contains(&available.to_string()),
        "the refusal has to carry both wei figures so a reader can check the arithmetic: {}",
        failure.reason
    );
    // 21 000 gas at the measured fee ceiling, which is the number §33 compares against the
    // wallet's real balance.
    assert_eq!(maximum_spend, U256::from(21_007_770_000u64));
}

#[test]
fn a_duplicate_claim_returns_the_record_that_already_owns_the_state() {
    // §30: two triggers from one state update must not become two transactions.
    let run = prepare(18, ExecutionMode::Submit);
    let mut ledger = Ledger::new();
    let Claim::New(handle) = ledger.claim(&run.intent, ExecutionStatus::Detected, 0) else {
        panic!("first claim")
    };
    ledger
        .advance(&handle.execution_id, ExecutionStatus::Built, 1)
        .expect("built");
    let Claim::Existing(existing) = ledger.claim(&run.intent, ExecutionStatus::Detected, 2) else {
        panic!("the same triple cannot be new twice")
    };
    assert_eq!(existing.status, ExecutionStatus::Built);
    assert_eq!(existing.execution_id, handle.execution_id);
    assert_eq!(ledger.len(), 1, "and the ledger still holds one record");
    // A different state version is a different execution, so the dedup key is the state
    // and not merely the opportunity.
    let mut moved = intent(18, run.sender);
    moved.state_fingerprint = String::from("a later state version");
    assert!(matches!(
        ledger.claim(&moved, ExecutionStatus::Detected, 3),
        Claim::New(_)
    ));
    assert_eq!(ledger.len(), 2);
}

#[tokio::test]
async fn a_sign_only_run_holds_bytes_and_has_no_way_to_send_them() {
    // §19/§20: the mode decides the capability, and no caller mistake gets past it. The
    // production halves here are `ExecutionMode::may_submit`, `EndpointKind::may_broadcast`
    // and `Signer::sign`; the scripted endpoint enforces the same guard the GIWA adapter
    // does, so the observable fact is that zero bytes left the process.
    let run = prepare(19, ExecutionMode::SignOnly);
    assert!(
        !run.signer.mode().may_submit(),
        "sign-only must not describe itself as a submitter"
    );
    assert!(
        !EndpointKind::Recorded.may_broadcast(),
        "and a recorded endpoint can never broadcast, in any mode"
    );
    for endpoint in [
        EndpointKind::PublicHttpRpc,
        EndpointKind::FlashblocksHttpRpc,
        EndpointKind::Recorded,
    ] {
        let may_send = run.signer.mode().may_submit() && endpoint.may_broadcast();
        assert!(
            !may_send,
            "no endpoint makes Submit's capability available to {}",
            run.signer.mode().name()
        );
    }

    let mut submitter = Scripted::new(vec![accepted(run.hash)], Vec::new());
    submitter.allowed = false;
    assert!(!submitter.may_submit());
    let error = submitter
        .submit(&run.signed)
        .await
        .expect_err("a run without the capability is refused");
    assert!(matches!(error, ExecutionError::ModeGate(_)), "{error}");
    assert_eq!(
        submitter.sent_count(),
        0,
        "the refusal happened before any send"
    );
    // The bytes exist and are complete — this is a run that chose not to send, not one
    // that failed to build.
    assert_eq!(run.signed.hash(), run.hash);
}

#[test]
fn an_intent_whose_run_needed_a_state_override_never_reaches_bytes() {
    // §34 and acceptance item T. Every M4/M5 simulation request funds its own sender
    // through `preflight()`'s setup override, so an intent derived from such a run is a
    // statement about a transaction no real account could send. The builder is where that
    // is decided, and it decides it before it produces any bytes.
    let mut arbitrage = intent(20, Address::from_slice(&[0xd4u8; 20]));
    assert!(
        !arbitrage.funding.derived_from_overridden_state(),
        "a validation intent is built from real state and says so"
    );
    arbitrage.funding = SenderFunding::Overridden {
        detail: "§58's test sender funded for 1 step by the simulation's setup override"
            .to_string(),
    };
    let error = TransactionBuilder::build(&arbitrage, &policy())
        .expect_err("§34 refuses an override-funded intent");
    assert!(
        matches!(error, ExecutionError::OverrideDependent(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("setup override"),
        "the refusal quotes how the run was funded, so the evidence names the scaffolding: \
         {error}"
    );
    assert!(
        BuildPolicy::default().require_unoverridden_state,
        "and the policy that refuses is the default one, not a choice this test made"
    );

    // The same fact travels into the record, which is what a reader of the evidence
    // file sees (§T asks to see it, not to infer it).
    let run = prepare(21, ExecutionMode::Submit);
    assert_eq!(
        ExecutionRecord::open(&run.intent, ExecutionStatus::Detected, 0).state_funding,
        run.intent.funding.describe()
    );
}
