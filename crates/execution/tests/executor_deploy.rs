//! §46/§47 with a scripted endpoint: what the operator session sends, and what it refuses
//! to send.
//!
//! The creation path exists because the repository had no deployment tooling at all (§4's
//! audit), and a path that only runs against a live chain is a path whose refusals cannot
//! be tested. So every case here runs against the crate's four traits implemented by a
//! script whose answers have the shape measured on GIWA in
//! `data/evidence/m6/probe-read-surface-2.txt` and `probe-submission-surface.txt` — chain
//! 91 342, a 371 wei base fee, a 1 000 000 wei tip, an L1 fee field on the receipt.
//!
//! **Nothing in this file is evidence that a broadcast worked.** §30's real controlled
//! execution is recorded separately in `data/evidence/m10/real/`; what these cases decide
//! is the four things a scripted endpoint can decide:
//!
//! * **the bytes** — a creation reaches the endpoint with `to` absent, the creation code
//!   followed by the 32-byte operator argument, `value` zero, and a signature that recovers
//!   the configured sender (§23, §34, §37);
//! * **the ladder** — one step's nonce is held until that step's receipt resolves, so the
//!   §57 sequence cannot put two transactions on one nonce, and the two answers that leave
//!   a transaction unresolved (a poll budget spent, an unknown submission) keep the lane
//!   occupied rather than papering over it (§11, §25);
//! * **the refusal** — a head that is no longer canonical, a wallet that cannot pay the
//!   ceiling, a mode that may not broadcast: each stops before `eth_sendRawTransaction`, and
//!   the endpoint's call counter is what makes "before" a measured zero instead of a hope;
//! * **the answer** — a receipt with `status = 0` comes back as `Ok(Step)`, because §58's
//!   required evidence is a real revert and an error-typed revert would make it unproducible.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{address, b256, keccak256, Address, Bytes, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    decode_raw, recover_sender, Abilities, ChainHead, ChainReader, DeployPolicy, Deployer,
    EndpointKind, ExecutionError, ExecutionKey, ExecutionMode, FeePolicy, FeeReading, FeeSource,
    NonceReading, NonceSource, Receipt, ReceiptPolicy, ReceiptStatus, Signer, SubmissionOutcome,
    TransactionSubmitter, TransactionType, UnsignedTransaction,
};
use evm_protocol::{decode_calldata, ExecutorCall};

/// §40's synthetic key again — the scalar one, never the operator's wallet (§35).
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
const HEAD: u64 = 38_002_233;
/// The creation's ceiling, and the wallet the script hands out: enough for the ceiling plus
/// a wrap, which is the smallest pair that makes the two §33 refusals distinguishable.
const CREATION_GAS: u64 = 2_000_000;
const CALL_GAS: u64 = 600_000;
const BALANCE: u64 = 35_764_375_608_889_104;

/// Three distinct synthetic block hashes, spelled with leading zeros for the same reason
/// `crates/execution/src/profit.rs` spells its hashes that way: §17's key scan reads this
/// crate's files whole and a contiguous 64-hex-digit run is the shape of a private key, while
/// a zero-padded quantity is not one.
const HEAD_HASH: B256 = b256!("0000000000000000000000000000000000000000000000000000000000000011");
/// A different hash at the same height: the reorg the gate has to name.
const REORGED_AT_HEAD: B256 =
    b256!("0000000000000000000000000000000000000000000000000000000000000022");
const MINED_BLOCK: u64 = 38_002_234;
const MINED_HASH: B256 = b256!("0000000000000000000000000000000000000000000000000000000000000033");

/// The pair the allowlist step is about, as an address and not a secret.
const POOL: Address = address!("2a3ceafb0e5c3a2dfb6a2b0cd1b0e6d9b0e1a2b3");
const TOKEN: Address = address!("07d4af6e2bc8dd82beb06b4fd279df4c9028f26f");

fn head() -> ChainHead {
    ChainHead {
        number: HEAD,
        hash: HEAD_HASH,
    }
}

fn test_signer(mode: ExecutionMode) -> Signer {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(mode, key)
}

fn sender() -> Address {
    test_signer(ExecutionMode::Submit)
        .address()
        .expect("a signer built from a key knows its address")
}

fn policy() -> DeployPolicy {
    DeployPolicy {
        creation_gas_limit: CREATION_GAS,
        call_gas_limit: CALL_GAS,
        tx_type: TransactionType::DynamicFee,
        fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
        receipt: ReceiptPolicy {
            attempts: 2,
            between_attempts: Duration::from_millis(1),
        },
    }
}

/// A deliberately tiny stand-in for `contracts/artifacts/ArbitrageExecutor.bin`: the shape
/// of a creation input is what these tests check, and the real bytecode's hash is evidence
/// in `data/evidence/m10/contract/`, not a fixture here.
fn creation_code() -> Vec<u8> {
    (0u8..64).collect()
}

fn receipt(success: bool, contract: Option<Address>, to: Option<Address>) -> Receipt {
    Receipt {
        transaction_hash: B256::ZERO,
        block_number: MINED_BLOCK,
        block_hash: MINED_HASH,
        transaction_index: 1,
        success,
        gas_used: 1_400_000,
        effective_gas_price: U256::from(BASE_FEE * 2 + TIP),
        cumulative_gas_used: Some(U256::from(1_400_000u64)),
        from: sender(),
        to,
        contract_address: contract,
        tx_type: Some(2),
        logs: Vec::new(),
        l1_fee: Some(U256::from(7_400_000_000u64)),
        l1_gas_price: Some(U256::from(1_000u64)),
        l1_gas_used: Some(U256::from(7_400_000u64)),
        l1_base_fee_scalar: Some(U256::from(1_000u64)),
        l1_blob_base_fee: Some(U256::ZERO),
        l1_blob_base_fee_scalar: Some(U256::ZERO),
        provenance: "scripted eth_getTransactionReceipt for the §46 creation path".to_string(),
    }
}

/// The endpoint: the four traits the session reads, every payload recorded, every call
/// counted, and the pending nonce view advanced only when a receipt is served — which is
/// what makes the lane tests below about the crate's discipline rather than the script's.
struct Scripted {
    chain_id: u64,
    balance: U256,
    blocks: Mutex<HashMap<u64, B256>>,
    nonce: AtomicU64,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
    sent: Mutex<Vec<Vec<u8>>>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new() -> Self {
        Self {
            chain_id: CHAIN,
            balance: U256::from(BALANCE),
            blocks: Mutex::new(HashMap::from([
                (HEAD, HEAD_HASH),
                (MINED_BLOCK, MINED_HASH),
            ])),
            nonce: AtomicU64::new(7),
            answers: Mutex::new(VecDeque::new()),
            receipts: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    /// An endpoint whose head moved: the height still exists and holds a different block.
    fn reorged(self) -> Self {
        self.blocks
            .lock()
            .expect("one writer")
            .insert(HEAD, REORGED_AT_HEAD);
        self
    }

    fn poorer_than(self, wei: u64) -> Self {
        let mut this = self;
        this.balance = U256::from(wei);
        this
    }

    fn on_chain(self, chain_id: u64) -> Self {
        let mut this = self;
        this.chain_id = chain_id;
        this
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

    fn accepted() -> SubmissionOutcome {
        SubmissionOutcome::Accepted {
            transaction_hash: None,
            endpoint: EndpointKind::PublicHttpRpc,
            detail: "eth_sendRawTransaction".to_string(),
        }
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked vector").len()
    }

    fn raw(&self, index: usize) -> Vec<u8> {
        self.sent
            .lock()
            .expect("an unlocked vector")
            .get(index)
            .cloned()
            .expect("the run handed over this payload")
    }

    fn decoded(&self, index: usize) -> evm_execution::DecodedTransaction {
        decode_raw(&self.raw(index), recover_sender).expect("the bytes handed to the node decode")
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn nonce_now(&self) -> u64 {
        self.nonce.load(Ordering::SeqCst)
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
            Some(U256::from(BASE_FEE)),
            Some(U256::from(TIP)),
            tx_type,
        )
    }

    async fn suggested_tip(&self) -> evm_execution::Result<Option<U256>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(U256::from(TIP)))
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
        let held = self.nonce.load(Ordering::SeqCst);
        Ok(NonceReading {
            address,
            confirmed: held,
            pending: held,
            at_block: HEAD,
            source: "scripted eth_getTransactionCount (confirmed and pending) for §46".to_string(),
        })
    }
}

#[async_trait]
impl ChainReader for Scripted {
    async fn block_hash_at(&self, number: BlockNumber) -> evm_execution::Result<Option<B256>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .blocks
            .lock()
            .expect("an unlocked map")
            .get(&number.0)
            .copied())
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
        transaction: &evm_execution::SignedTransaction,
    ) -> evm_execution::Result<SubmissionOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("an unlocked vector")
            .push(transaction.raw().to_vec());
        Ok(self
            .answers
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .unwrap_or_else(Self::accepted))
    }

    async fn receipt(&self, transaction_hash: B256) -> evm_execution::Result<Option<Receipt>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .receipts
            .lock()
            .expect("an unlocked queue")
            .pop_front()
            .flatten();
        if next.is_some() {
            // The chain executed it: the account's next transaction has one more nonce. This
            // is the only place the script advances, so a step that never resolved would
            // collide with the one before it on the nonce — which is what §11 forbids.
            self.nonce.fetch_add(1, Ordering::SeqCst);
        }
        Ok(next.map(|mut receipt| {
            receipt.transaction_hash = transaction_hash;
            receipt
        }))
    }
}

fn assemble(endpoint: Scripted, mode: ExecutionMode) -> (Deployer, Arc<Scripted>) {
    let scripted = Arc::new(endpoint);
    let abilities = Abilities {
        submitter: scripted.clone(),
        fees: scripted.clone(),
        nonces: scripted.clone(),
        chain: scripted.clone(),
    };
    let deployer = Deployer::new(abilities, test_signer(mode), policy(), CHAIN)
        .expect("a submit-mode session on a named chain");
    (deployer, scripted)
}

/// A creation as the endpoint saw it: the transaction that made the executor.
fn created(executor: Address) -> Option<Receipt> {
    Some(receipt(true, Some(executor), None))
}

/// The address a real chain would report for the first creation this session sends. The
/// tests use it instead of a made-up literal so the address proof below is a comparison
/// between two independently produced numbers rather than a tautology.
fn deployed_at_nonce_seven() -> Address {
    evm_execution::create_address(sender(), 7)
}

#[tokio::test]
async fn a_deployment_sends_a_creation_and_reports_the_six_fields() {
    let executor = deployed_at_nonce_seven();
    let code = creation_code();
    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(Scripted::accepted())
            .receipt(created(executor)),
        ExecutionMode::Submit,
    );

    let deployment = deployer
        .deploy(head(), &code, sender(), "m10-v1")
        .await
        .expect("the scripted chain creates the contract");

    // The bytes: `to` absent, code then argument, no value, our sender on the signature.
    let seen = endpoint.decoded(0);
    let unsigned: &UnsignedTransaction = &seen.unsigned;
    assert_eq!(unsigned.to, None, "a creation has no destination");
    assert_eq!(
        unsigned.value,
        U256::ZERO,
        "§34: a deployment moves no native value"
    );
    assert_eq!(unsigned.chain_id, CHAIN);
    assert_eq!(unsigned.gas_limit, CREATION_GAS);
    assert_eq!(unsigned.nonce, 7);
    let expected_input = [
        code.as_slice(),
        evm_execution::constructor_arguments(sender()).as_ref(),
    ]
    .concat();
    assert_eq!(unsigned.input.as_ref(), expected_input.as_slice());
    assert_eq!(seen.sender, Some(sender()));

    // §46's six fields, each present and each from the chain rather than from us.
    assert_eq!(deployment.contract_address, Some(executor));
    assert_eq!(deployment.deployed(), Some(executor));
    assert_eq!(deployment.step.nonce, 7);
    assert_eq!(
        deployment.predicted_address, executor,
        "the address the encoding predicts is the address the receipt reports"
    );
    assert!(deployment.address_proved());
    assert_eq!(deployment.creation_code_bytes, code.len());
    assert_eq!(deployment.creation_code_hash, keccak256(&code));
    assert_eq!(deployment.abi_version, "m10-v1");
    assert_eq!(deployment.step.chain_id, CHAIN);
    assert_eq!(deployment.step.status, ReceiptStatus::Included);
    assert!(deployment.step.succeeded());

    let json = deployment.to_json();
    for field in [
        "contract_address",
        "deployment_tx",
        "deployment_block",
        "chain_id",
        "creation_code_hash",
        "abi_version",
    ] {
        assert!(!json[field].is_null(), "§46 field {field} is null: {json}");
    }
    assert_eq!(json["deployment_block"], serde_json::json!(MINED_BLOCK));
    assert_eq!(json["address_matches_prediction"], serde_json::json!(true));
}

#[tokio::test]
async fn the_ladder_hands_the_nonce_over_only_when_a_step_resolves() {
    let executor = deployed_at_nonce_seven();
    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(Scripted::accepted())
            .receipt(created(executor))
            .answer(Scripted::accepted())
            .receipt(Some(receipt(true, None, Some(executor)))),
        ExecutionMode::Submit,
    );

    let deployment = deployer
        .deploy(head(), &creation_code(), sender(), "m10-v1")
        .await
        .expect("the creation resolves");
    let configured = deployer
        .call(
            head(),
            executor,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed".to_string(),
        )
        .await
        .expect("the next step runs once the first has a receipt");

    assert_eq!(deployment.step.nonce, 7);
    assert_eq!(configured.nonce, 8, "one step, one nonce, in order");
    assert_eq!(endpoint.nonce_now(), 9);
    assert_eq!(endpoint.sent_count(), 2);
    // The second transaction is a call, not a creation, and carries no value (§34).
    assert_eq!(endpoint.decoded(1).unsigned.to, Some(executor));
    assert_eq!(endpoint.decoded(1).unsigned.value, U256::ZERO);
    assert_eq!(endpoint.decoded(1).unsigned.gas_limit, CALL_GAS);
}

#[tokio::test]
async fn a_reverted_step_comes_back_as_an_answer_so_the_failure_can_be_evidenced() {
    // §58's shape: the transaction is included and the EVM refused it. An `Err` here would
    // make the required evidence impossible to produce.
    let executor = address!("4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d");
    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(Scripted::accepted())
            .receipt(Some(receipt(false, None, Some(executor))))
            .answer(Scripted::accepted())
            .receipt(Some(receipt(true, None, Some(executor)))),
        ExecutionMode::Submit,
    );

    let step = deployer
        .call(
            head(),
            executor,
            Bytes::from_static(&[0u8; 4]),
            "execute-min-output".to_string(),
        )
        .await
        .expect("a revert is a receipt, not a transport failure");

    assert!(step.reverted(), "the status is the chain's answer");
    assert!(!step.succeeded());
    assert_eq!(step.status, ReceiptStatus::Reverted);
    assert_eq!(step.to, Some(executor));
    assert_eq!(
        step.l1_fee(),
        Some(U256::from(7_400_000_000u64)),
        "a reverted step is still billed for its L1 data"
    );
    assert_eq!(step.gas_used(), Some(1_400_000));

    // And the lane is free again, because the transaction resolved.
    let next = deployer
        .call(
            head(),
            executor,
            Bytes::from_static(&[1u8; 4]),
            "setTokenAllowed".to_string(),
        )
        .await
        .expect("the revert released the nonce");
    assert_eq!(next.nonce, 8);
    assert_eq!(endpoint.sent_count(), 2);
}

#[tokio::test]
async fn a_step_without_a_receipt_holds_the_lane_rather_than_retrying() {
    // §25/§11: the poll budget spent says nothing about the transaction, so the next step
    // must fail on the occupied lane instead of sending a second one.
    let (mut deployer, endpoint) = assemble(
        Scripted::new().answer(Scripted::accepted()),
        ExecutionMode::Submit,
    );

    let error = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed".to_string(),
        )
        .await
        .expect_err("two null receipts is a timeout, not an inclusion");
    assert!(
        matches!(error, ExecutionError::ReceiptTimeout(_)),
        "the timeout is its own error class: {error}"
    );

    let second = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed-again".to_string(),
        )
        .await
        .expect_err("the lane is still held by the unresolved step");
    assert!(
        matches!(second, ExecutionError::NonceUnavailable(_)),
        "the refusal is the lane, not the chain: {second}"
    );
    assert_eq!(
        endpoint.sent_count(),
        1,
        "no second transaction was ever sent"
    );
}

#[tokio::test]
async fn an_unknown_submission_answer_holds_the_lane_too() {
    let (mut deployer, endpoint) = assemble(
        Scripted::new().answer(SubmissionOutcome::Unknown {
            reason: "connection reset".to_string(),
            endpoint: EndpointKind::PublicHttpRpc,
        }),
        ExecutionMode::Submit,
    );

    let error = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setTokenAllowed".to_string(),
        )
        .await
        .expect_err("we asked and do not know");
    assert!(
        matches!(error, ExecutionError::SubmissionUnknown(_)),
        "an unknown answer is not a rejection: {error}"
    );

    let second = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setTokenAllowed-again".to_string(),
        )
        .await
        .expect_err("a retry over the same nonce is what §25 forbids");
    assert!(matches!(second, ExecutionError::NonceUnavailable(_)));
    assert_eq!(endpoint.sent_count(), 1);
}

#[tokio::test]
async fn a_definite_refusal_gives_the_lane_back() {
    // The mirror image of the two cases above: only a node that said no proves the
    // transaction is absent, and only then may the ladder move on.
    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(SubmissionOutcome::Rejected {
                reason: "nonce too low".to_string(),
                endpoint: EndpointKind::PublicHttpRpc,
            })
            .answer(Scripted::accepted())
            .receipt(Some(receipt(true, None, Some(POOL)))),
        ExecutionMode::Submit,
    );

    let error = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed".to_string(),
        )
        .await
        .expect_err("the node said no");
    assert!(
        matches!(&error, ExecutionError::SubmissionRejected(reason) if reason.contains("nonce too low")),
        "the node's own words are in the refusal: {error}"
    );

    deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed-retried".to_string(),
        )
        .await
        .expect("the lane came back with the rejection");
    assert_eq!(endpoint.sent_count(), 2);
    assert_eq!(endpoint.decoded(1).unsigned.nonce, 7);
}

#[tokio::test]
async fn a_head_that_moved_stops_the_step_before_anything_is_priced() {
    let (mut deployer, endpoint) = assemble(Scripted::new().reorged(), ExecutionMode::Submit);

    let error = deployer
        .call(
            head(),
            POOL,
            Bytes::from_static(&[0u8; 4]),
            "setPairAllowed".to_string(),
        )
        .await
        .expect_err("the head this step is priced against is gone");
    match error {
        ExecutionError::ChainMismatch(reason) => {
            assert!(
                reason.contains("setPairAllowed"),
                "the refusal names the step: {reason}"
            );
            assert!(
                reason.contains(&format!("{REORGED_AT_HEAD:#x}")),
                "and quotes both hashes: {reason}"
            );
        }
        other => panic!("the wrong error class: {other}"),
    }
    assert_eq!(endpoint.sent_count(), 0, "nothing was sent");
    assert_eq!(
        endpoint.call_count(),
        1,
        "only the head read was paid for — no fee, no nonce, no balance"
    );
}

#[tokio::test]
async fn a_wallet_that_cannot_cover_the_ceiling_is_refused_before_the_send() {
    // The ceiling is `gas_limit * max_fee + value`; the script's wallet is one wei short of
    // it, so the refusal is about the arithmetic the crate did, not about a mood.
    let ceiling = U256::from(CREATION_GAS) * U256::from(BASE_FEE * 2 + TIP);
    let short = (ceiling - U256::from(1u64)).to::<u64>();
    let (mut deployer, endpoint) =
        assemble(Scripted::new().poorer_than(short), ExecutionMode::Submit);

    let error = deployer
        .deploy(head(), &creation_code(), sender(), "m10-v1")
        .await
        .expect_err("§33: the balance is read, and a short wallet does not get to try");
    match error {
        ExecutionError::InsufficientBalance(reason) => {
            assert!(
                reason.contains("deploy"),
                "the refusal names the step: {reason}"
            );
            assert!(
                reason.contains(&ceiling.to_string()),
                "and both numbers: {reason}"
            );
        }
        other => panic!("the wrong error class: {other}"),
    }
    assert_eq!(endpoint.sent_count(), 0);
    assert_eq!(
        endpoint.call_count(),
        3,
        "the head read, the fee read and the balance read happened; the nonce read and the \
         send did not"
    );
}

#[tokio::test]
async fn a_session_that_may_not_broadcast_is_refused_at_construction() {
    // Through the real constructor this time: `session_gate` is only a function, and the
    // claim that matters is that no `Deployer` value can exist without submit mode.
    for mode in [ExecutionMode::BuildOnly, ExecutionMode::SignOnly] {
        let scripted = Arc::new(Scripted::new());
        let abilities = Abilities {
            submitter: scripted.clone(),
            fees: scripted.clone(),
            nonces: scripted.clone(),
            chain: scripted.clone(),
        };
        let built = Deployer::new(abilities, test_signer(mode), policy(), CHAIN);
        let error = match built {
            Ok(_) => panic!("a session that cannot broadcast was constructed anyway"),
            Err(error) => error,
        };
        match error {
            ExecutionError::ModeGate(reason) => {
                assert!(
                    reason.contains(mode.name()),
                    "the gate names the mode: {reason}"
                )
            }
            other => panic!("the wrong error class for {mode:?}: {other}"),
        }
    }
}

#[tokio::test]
async fn a_node_on_another_chain_is_reported_before_the_first_transaction() {
    let (deployer, endpoint) = assemble(Scripted::new().on_chain(1), ExecutionMode::Submit);
    let error = deployer
        .verify_chain()
        .await
        .expect_err("the endpoint answers for chain 1 and the session is for 91 342");
    match error {
        ExecutionError::ChainMismatch(reason) => {
            assert!(
                reason.contains("91342") && reason.contains("chain 1"),
                "{reason}"
            )
        }
        other => panic!("the wrong error class: {other}"),
    }
    assert_eq!(endpoint.sent_count(), 0);
    assert_eq!(endpoint.call_count(), 1);
}

#[tokio::test]
async fn a_funding_step_has_to_actually_move_value() {
    let executor = address!("4200000000000000000000000000000000000006");
    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(Scripted::accepted())
            .receipt(Some(receipt(true, None, Some(executor)))),
        ExecutionMode::Submit,
    );

    let zero = deployer
        .funding_call(
            head(),
            executor,
            Bytes::from_static(&[0xd0, 0xe3, 0x0b, 0x01]),
            U256::ZERO,
            "wrap-native".to_string(),
        )
        .await
        .expect_err("a deposit of nothing is not a funding step");
    assert!(matches!(zero, ExecutionError::InvalidIntent(_)));
    assert_eq!(endpoint.sent_count(), 0, "and it cost no send");

    let wrapped = deployer
        .funding_call(
            head(),
            executor,
            Bytes::from_static(&[0xd0, 0xe3, 0x0b, 0x01]),
            U256::from(1_000_000_000_000_000u64),
            "wrap-native".to_string(),
        )
        .await
        .expect("a wrap that moves something is a legitimate step");
    assert_eq!(wrapped.value_wei, U256::from(1_000_000_000_000_000u64));
    assert_eq!(
        endpoint.decoded(0).unsigned.value,
        U256::from(1_000_000_000_000_000u64),
        "and the chain sees the same number we claim it did"
    );
    // The ceiling the §33 check used included the value, not just the gas.
    assert!(wrapped.maximum_spend_wei > U256::from(CREATION_GAS) * U256::from(TIP));
}

#[tokio::test]
async fn an_execute_reaches_the_endpoint_byte_for_byte_as_the_plan_encoded_it() {
    // §37's determinism claim applied to the operator path: the calldata that leaves this
    // crate is the calldata the plan built, and the node's copy decodes back to the same
    // call — no re-encoding, no field reordering, no default filled in on the way.
    let executor = address!("4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d");
    let legs = vec![evm_protocol::ExecutorLeg {
        pool: POOL,
        token_in: TOKEN,
        token_out: executor,
        amount_in: U256::from(1_000u64),
        amount_out: U256::from(1_060u64),
        min_amount_out: U256::from(1_050u64),
    }];
    let call = ExecutorCall::Execute {
        legs: legs.clone(),
        input_token: TOKEN,
        amount_in: U256::from(1_000u64),
        min_final_amount: U256::from(1_050u64),
        recipient: sender(),
    };
    let calldata = call.encode();

    let (mut deployer, endpoint) = assemble(
        Scripted::new()
            .answer(Scripted::accepted())
            .receipt(Some(receipt(true, None, Some(executor)))),
        ExecutionMode::Submit,
    );
    let step = deployer
        .call(head(), executor, calldata.clone(), "execute".to_string())
        .await
        .expect("the execute step sends");

    assert_eq!(step.input_bytes, calldata.len());
    assert_eq!(step.input_hash, keccak256(calldata.as_ref()));
    let seen = endpoint.decoded(0);
    assert_eq!(seen.unsigned.input, calldata);
    assert_eq!(
        seen.unsigned.to,
        Some(executor),
        "§36: the configured executor"
    );
    match decode_calldata(&seen.unsigned.input).expect("the node's copy decodes") {
        ExecutorCall::Execute {
            legs: found_legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => {
            assert_eq!(found_legs, legs);
            assert_eq!(input_token, TOKEN);
            assert_eq!(amount_in, U256::from(1_000u64));
            assert_eq!(min_final_amount, U256::from(1_050u64));
            assert_eq!(recipient, sender());
        }
        other => panic!("the endpoint decoded a different call: {other:?}"),
    }
}
