//! M12-F §8: the crash-recovery matrix F1–F9, run against a real ledger file.
//!
//! The method is the one an append-only log forces: a crash is a **prefix of the process's own
//! write stream**. So each case below drives a real run through [`ExecutionStage`] over a
//! scripted endpoint into a real file in a temp directory, cuts that file at a fact boundary
//! ([`crash_after`]), drops the handle — the process is gone, and nothing in this test can read
//! what it held in memory — and then re-enters the way §7 says an entry must: open the ledger,
//! build the stage from what it recovered, and only then attempt anything.
//!
//! Two of the cases (F5b, F8) are not prefix cuts at all. They damage the file *while a run is
//! driving it*, through a hook inside the scripted endpoint, because §9's question — can memory
//! and disk silently diverge? — only has an answer if the damage lands between two of the run's
//! own writes. A prefix cut cannot test it: the writer never learns anything.
//!
//! §8's closing bar is the reason this file exists rather than the unit tests in
//! [`evm_execution::journal`]: every assertion is about an **observed state change or an observed
//! send count**, read off the bytes and off the endpoint, not off what a function returned. The
//! endpoint counts every payload it was handed, so "a restarted process never re-broadcasts" is a
//! measured zero at the socket, and the ledger is re-read from disk, so "recovered as
//! `awaiting_receipt`" is a claim about a file rather than about a struct.
//!
//! Nothing here touches a network. The endpoints are scripted, the chains are fixtures, and every
//! directory is under the system temp dir and removed on the way out.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use alloy_primitives::{address, Address, B256, U256};
use async_trait::async_trait;
use evm_core::BlockNumber;
use evm_execution::{
    journal_file_name, journal_stamp, Abilities, AmountDerivation, ArbitrageExecutionPlan,
    BuildPolicy, ChainReader, EndpointKind, ExecutablePlan, ExecutionBinding, ExecutionError,
    ExecutionJournal, ExecutionKey, ExecutionMode, ExecutionSetup, ExecutionStage, ExecutionStatus,
    FeePolicy, FeeReading, FeeSource, Freshness, JournalFact, JournalRecord, JournalRecovery,
    LaneRelease, MarketKind, NonceReading, NonceSource, PlanLeg, PlanValidity, ProfitDenomination,
    ProfitPolicy, Receipt, ReceiptPolicy, ReceiptStatus, RecoveredExecution, RecoveredState,
    SenderFunding, Signer, SimulationContext, SimulationOutcome, StageReport, StepIdentity,
    SubmissionOutcome, TransactionSubmitter, TransactionType,
};
use evm_metrics::{Clock, Metrics};
use serde_json::{json, Value};

/// The chain the fixture trades on, and the one its damaged copies are pretending to be about.
const CHAIN: u64 = 91_342;
const OTHER_CHAIN: u64 = 91_343;

const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const SIM_BLOCK: u64 = 37_984_319;
const MINED_BLOCK: u64 = SIM_BLOCK + 1;
const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
const BALANCE_WEI: u128 = 20_000_000_000_000_000;
const SIMULATED_GAS: u64 = 210_000;
const SIMULATED_GAS_LIMIT: u64 = 230_000;
const CHAIN_GAS_USED: u64 = 198_400;

const EXECUTOR: Address = address!("00000000000000000000000000000000000000ee");
const RECIPIENT: Address = address!("00000000000000000000000000000000000000bb");
const TOKEN_A: Address = address!("00000000000000000000000000000000000000a1");
const TOKEN_B: Address = address!("00000000000000000000000000000000000000b1");
const PAIR_0: Address = address!("00000000000000000000000000000000000000f1");
const PAIR_1: Address = address!("00000000000000000000000000000000000000f2");

/// The two damages a live run can meet. Stored as a number because the endpoint is behind a
/// `Mutex`-free atomic and the hook has to fire from inside a trait method.
const NO_FAULT: usize = 0;
/// §8's F8 / §4.1: the ledger loses its bytes between the nonce read and the send, so the intent
/// line cannot land and `submit` must not be reached.
const TRUNCATE_BEFORE_SEND: usize = 1;
/// §8's F5 / §9: the ledger is unlinked and rewritten byte-for-byte while the run is mid-receipt,
/// so the next append would go to an inode the path no longer names.
const REPLACE_BEFORE_RECEIPT: usize = 2;

// ---------------------------------------------------------------------------
// scratch directories, prefix cuts, and the two ways to damage a live file
// ---------------------------------------------------------------------------

/// One temp directory per case, created eagerly and removed on the way out.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock past 1970")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("m12f-crash-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory for the ledger");
        Self(dir)
    }

    fn dir(&self) -> &Path {
        &self.0
    }

    /// The ledger file this scratch's chain would use, whether or not it exists yet.
    fn journal(&self) -> PathBuf {
        journal_at(&self.0, CHAIN)
    }

    fn text(&self) -> String {
        std::fs::read_to_string(self.journal()).expect("the ledger the run wrote")
    }

    fn line_count(&self) -> usize {
        self.text().lines().count()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn journal_at(dir: &Path, chain_id: u64) -> PathBuf {
    dir.join(journal_file_name(chain_id))
}

/// Cut the file at a fact boundary: keep every line up to and including the first one carrying
/// `fact`, and write the result. This is the whole of the crash model — the handle stays alive in
/// the test, but nothing reads it again, which is what the process being gone means here.
///
/// Returns how many lines were kept, so a case can name the boundary it stopped at rather than
/// count lines by hand.
fn crash_after(path: &Path, fact: JournalFact) -> usize {
    let text = std::fs::read_to_string(path).expect("the ledger the run just wrote");
    let marker = format!("\"fact\":\"{}\"", fact.name());
    let lines: Vec<&str> = text.lines().collect();
    let index = lines
        .iter()
        .position(|line| line.contains(&marker))
        .unwrap_or_else(|| {
            panic!(
                "no line carries the fact {}, so this ledger cannot be cut at it: {text}",
                fact.name()
            )
        });
    let body = lines[..=index].join("\n");
    std::fs::write(path, format!("{body}\n")).expect("cut the ledger at a fact boundary");
    index + 1
}

fn count_facts(text: &str, fact: JournalFact) -> usize {
    let marker = format!("\"fact\":\"{}\"", fact.name());
    text.lines().filter(|line| line.contains(&marker)).count()
}

#[cfg(unix)]
fn inode_of(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .expect("the ledger has metadata")
        .ino()
}

/// Unlink the ledger and put a byte-identical copy in its place, so the path names a different
/// inode while its length says nothing changed. Returns whether the inode actually differs — a
/// filesystem that handed back the same number would make the case below a witness to nothing,
/// and the caller refuses to pretend otherwise.
#[cfg(unix)]
fn replace_in_place(path: &Path) -> bool {
    let bytes = std::fs::read(path).expect("the ledger the run is writing");
    let original = inode_of(path);
    for try_number in 0..8 {
        let swap = path.with_extension(format!("swap{try_number}"));
        std::fs::write(&swap, &bytes).expect("the replacement copy");
        if inode_of(&swap) != original {
            std::fs::remove_file(path).expect("unlinked the ledger");
            std::fs::rename(&swap, path).expect("put the copy in its place");
            return true;
        }
        std::fs::remove_file(&swap).ok();
    }
    false
}

// ---------------------------------------------------------------------------
// the fixture route, unchanged in shape from M10's lifecycle harness
// ---------------------------------------------------------------------------

fn sim_hash() -> B256 {
    B256::left_padding_from(&[7u8; 20])
}

fn mined_block_hash() -> B256 {
    B256::left_padding_from(&[9u8; 20])
}

fn run_one() -> B256 {
    B256::left_padding_from(&[5u8; 20])
}

fn run_two() -> B256 {
    B256::left_padding_from(&[6u8; 20])
}

fn synthetic_address() -> Address {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(ExecutionMode::SignOnly, key)
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
            correlation_id: "m12f-crash-route".to_string(),
            block_number: BlockNumber(SIM_BLOCK),
            block_hash: sim_hash(),
            state_fingerprint: format!("{SIM_BLOCK}:0"),
            simulation_id,
            outcome,
            funding: SenderFunding::RealState {
                source: "§34: the fixture funded the sender it spends from".to_string(),
            },
            market: MarketKind::ControlledFixture {
                proves: "crash recovery over a durable ledger".to_string(),
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

fn executable(sender: Address) -> ExecutablePlan {
    ExecutablePlan::new(
        route(sender, 1_000, succeeded(), 1_060, run_one()),
        &binding(),
    )
    .expect("the fixture route validates")
}

/// A second, differently sized run of the same route. §8's F9 asks whether a restarted process
/// reaches for *another* transaction when one of them is possibly live; a substitution has to be
/// a real alternative, not the same plan again.
fn substituted(sender: Address) -> ExecutablePlan {
    ExecutablePlan::new(
        route(sender, 2_000, succeeded(), 2_060, run_two()),
        &binding(),
    )
    .expect("twice the size is still a route")
}

// ---------------------------------------------------------------------------
// the scripted endpoint, with the ledger path and the fault it fires
// ---------------------------------------------------------------------------

struct Scripted {
    chain_id: u64,
    base_fee: U256,
    tip: U256,
    balance: U256,
    nonce: u64,
    blocks: HashMap<u64, B256>,
    answers: Mutex<VecDeque<SubmissionOutcome>>,
    receipts: Mutex<VecDeque<Option<Receipt>>>,
    sent: Mutex<Vec<Vec<u8>>>,
    calls: AtomicUsize,
    /// Which damage this endpoint inflicts, and on which file. `0` for every case that only
    /// crashes by prefix cut.
    fault: AtomicUsize,
    journal_file: Mutex<Option<PathBuf>>,
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
            fault: AtomicUsize::new(NO_FAULT),
            journal_file: Mutex::new(None),
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

    fn damage(self, fault: usize, journal_file: PathBuf) -> Self {
        self.fault.store(fault, Ordering::SeqCst);
        *self.journal_file.lock().expect("an unlocked path") = Some(journal_file);
        self
    }

    /// The damage, taken out of the slot so it fires once. A hook that fired on every read would
    /// turn one refused line into a permanent condition and stop proving the line it names.
    ///
    /// `hook` is which of this endpoint's methods is asking. The two damages the matrix injects
    /// land at different points of the same run — one before the intent line, one before the
    /// receipt line — and the nonce read happens first, so a slot that any hook could drain would
    /// always fire at the earliest one and the later damage would never be reached.
    fn fire(&self, hook: usize) {
        if self.fault.load(Ordering::SeqCst) != hook {
            return;
        }
        self.fault.store(NO_FAULT, Ordering::SeqCst);
        let Some(path) = self.journal_file.lock().expect("an unlocked path").clone() else {
            return;
        };
        match hook {
            TRUNCATE_BEFORE_SEND => {
                std::fs::write(&path, "").expect("emptied the ledger in place");
            }
            #[cfg(unix)]
            REPLACE_BEFORE_RECEIPT => {
                let replaced = replace_in_place(&path);
                assert!(
                    replaced,
                    "the scratch filesystem handed back the same inode, so this case cannot \
                     prove the replacement guard"
                );
            }
            other => panic!("no fault is numbered {other}"),
        }
    }

    fn sent_count(&self) -> usize {
        self.sent.lock().expect("an unlocked counter").len()
    }

    /// Every read and send the run asked for. A recovery that must not reach the network has to
    /// be able to say zero.
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
        self.fire(TRUNCATE_BEFORE_SEND);
        Ok(NonceReading {
            address,
            confirmed: self.nonce,
            pending: self.nonce,
            at_block: SIM_BLOCK,
            source: "scripted eth_getTransactionCount in §8's crash matrix".to_string(),
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
        transaction: &evm_execution::SignedTransaction,
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
        self.fire(REPLACE_BEFORE_RECEIPT);
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

fn accepted() -> SubmissionOutcome {
    SubmissionOutcome::Accepted {
        transaction_hash: None,
        endpoint: EndpointKind::PublicHttpRpc,
        detail: "scripted eth_sendRawTransaction acknowledgement".to_string(),
    }
}

/// §4.3's state: an answer that proves nothing either way. The reason string is what the journal
/// line quotes, and it has to be recognisable in the file for the "never converted to a refusal"
/// assertions below to be checkable.
fn unknown() -> SubmissionOutcome {
    SubmissionOutcome::Unknown {
        reason: "scripted: the node neither acknowledged nor refused the request".to_string(),
        endpoint: EndpointKind::PublicHttpRpc,
    }
}

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
        provenance: "scripted eth_getTransactionReceipt in §8's crash matrix".to_string(),
    }
}

// ---------------------------------------------------------------------------
// the entry, the way §7 says an entry is built
// ---------------------------------------------------------------------------

/// One process's worth of driving: the stage it built, the report its attempt produced, and the
/// counters it bumped.
struct Process {
    stage: ExecutionStage,
    report: StageReport,
    metrics: Metrics,
}

/// §7's ordering as a function, for the chain a case is testing: read and validate the ledger,
/// build the stage from what it recovered, and only then hand the caller something that could sign
/// or send. A ledger that refuses to open therefore returns before a stage exists — which is what
/// makes the endpoint's call count below a witness rather than a hope.
fn enter_chain(
    dir: &Path,
    chain_id: u64,
    scripted: Arc<Scripted>,
) -> evm_execution::Result<ExecutionStage> {
    let journal = ExecutionJournal::open(dir, chain_id, journal_stamp())?;
    let abilities = Abilities {
        submitter: scripted.clone(),
        fees: scripted.clone(),
        nonces: scripted.clone(),
        chain: scripted.clone(),
    };
    let setup = ExecutionSetup {
        mode: ExecutionMode::Submit,
        fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
        build: BuildPolicy {
            expected_chain_id: chain_id,
            ..Default::default()
        },
        receipts: ReceiptPolicy {
            attempts: 2,
            between_attempts: Duration::from_millis(1),
        },
    };
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    ExecutionStage::new(
        abilities,
        Signer::from_key(ExecutionMode::Submit, key),
        setup,
        chain_id,
        Clock::new(),
        journal,
    )
}

/// The entry as the production paths take it: this fixture's chain.
fn enter(dir: &Path, scripted: Arc<Scripted>) -> evm_execution::Result<ExecutionStage> {
    enter_chain(dir, CHAIN, scripted)
}

/// One process: enter, attempt the plan, and keep everything the assertions need to read.
async fn attempt(dir: &Path, scripted: Arc<Scripted>, plan: &ExecutablePlan) -> Process {
    let mut stage = enter(dir, scripted).expect("a ledger this build can open");
    let mut metrics = Metrics::default();
    let report = stage
        .on_arbitrage_plan(plan, &binding(), Freshness::Active, &mut metrics)
        .await;
    Process {
        stage,
        report,
        metrics,
    }
}

fn one_record(recovery: &JournalRecovery) -> &RecoveredExecution {
    assert_eq!(
        recovery.records.len(),
        1,
        "this crash left exactly one execution in the file, not {}",
        recovery.records.len()
    );
    recovery
        .records
        .first()
        .expect("the record one line was folded from")
}

/// A fresh endpoint with nothing queued: a restarted process that sends gets a refusal it did not
/// script, so a send in this case is always a bug rather than a fixture.
fn silent_endpoint() -> Arc<Scripted> {
    Arc::new(Scripted::new())
}

// ---------------------------------------------------------------------------
// §10's evidence channel
// ---------------------------------------------------------------------------

/// Append one measured row to `measured-rows.jsonl` in the directory `M12F_EVIDENCE_DIR` names.
/// Unset — which is every ordinary gate run — and nothing is written and no directory is made;
/// the variable is the whole of the file's mandate.
///
/// Four properties the committed tables depend on:
///
/// - every figure is read off the same objects the case's own assertions just used — the
///   endpoint's own arrival tally, the [`JournalRecovery`] this build re-read from disk, the
///   record's derived state and its `holds_lane()`, the ledger's line count, the refusal's
///   sentence — so a table cannot carry a number no test observed;
/// - each `record` call sits *after* that case's assertions, so a wrong figure panics the case
///   before it can be written down;
/// - a row is reproducible across runs: it names no wall clock, no temp directory, no inode and
///   no process id, because none of those is a fact about the ledger. What it does name are the
///   fixture's own stable hashes and the words production prints — and
///   `ledger_evidence.rs` re-reads those words out of `crates/execution/src/journal.rs`
///   rather than trusting this file to spell them, then re-assembles the published tables from
///   these rows;
/// - a field a case did not measure is *absent* rather than filled with a plausible value, since
///   an invented zero is the one figure an evidence table must never contain.
fn record(row: Value) {
    let Some(dir) = std::env::var_os("M12F_EVIDENCE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("measured-rows.jsonl"))
        .unwrap_or_else(|error| panic!("the evidence file could not be opened: {error}"));
    writeln!(file, "{row}").expect("an evidence row was appended");
}

/// A nonce the file names, or the honest statement that the case never read one. `null` is
/// written only where a case *proved* the absence; a figure it did not measure is left out of the
/// row altogether, which is what lets the gate tell the two apart.
fn nonce_value(nonce: Option<u64>) -> Value {
    nonce.map(|nonce| json!(nonce)).unwrap_or(Value::Null)
}

/// The same hash the journal line carries, in the shape §5's rules grade: 64 hex digits under an
/// `0x`. A signed payload is far longer, and the gate's length rule is what keeps one out.
fn hash_value(hash: Option<B256>) -> Value {
    hash.map(|hash| json!(format!("{hash:#x}")))
        .unwrap_or(Value::Null)
}

/// The lane reading, in the one word the tables need, from the case's own look at the stage or at
/// the file's occupancy list — never from the derived state, which the row already says through
/// `holds_lane`. Keeping the two readings separate is what makes `lane_word_matches_hold` a
/// cross-check rather than a restatement.
fn lane_word(held: bool) -> &'static str {
    if held {
        "held"
    } else {
        "idle"
    }
}

/// Whether the fact order a case read off the file puts `before` ahead of `after`. A vector that
/// holds neither word, or only one, answers `false`: the row then says the order was not measured
/// rather than claiming one the file does not show.
fn fact_precedes(facts: &[JournalFact], before: JournalFact, after: JournalFact) -> bool {
    match (
        facts.iter().position(|fact| *fact == before),
        facts.iter().position(|fact| *fact == after),
    ) {
        (Some(first), Some(second)) => first < second,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// F1 — a crash after the intent line and before the send
// ---------------------------------------------------------------------------

/// §8's F1, §4.2's fact 2, §4.3's lane rule. The file proves the bytes existed and were named; it
/// cannot prove the request never left, so recovery holds the nonce and refuses both a re-send and
/// a second transaction on the same number.
#[tokio::test]
async fn a_crash_between_the_intent_line_and_the_socket_leaves_the_nonce_possibly_in_flight() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f1-intent");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .receipt(Some(receipt(true, sender))),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    // The run itself was a success — that is the point: memory learned more than the prefix holds.
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Included));
    assert_eq!(first.sent_count(), 1, "the crashed process did send");
    let durable_nonce = crashed
        .report
        .record
        .as_ref()
        .expect("the run made a record")
        .nonce;
    let durable_hash = crashed
        .report
        .record
        .as_ref()
        .expect("the run made a record")
        .transaction_hash;

    let kept = crash_after(&scratch.journal(), JournalFact::SendIntentPersisted);
    drop(crashed);

    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("a clean prefix re-reads");
    let record = one_record(&recovery);
    assert_eq!(record.last_fact, JournalFact::SendIntentPersisted);
    assert_eq!(record.state, RecoveredState::PossiblyInFlight);
    assert!(
        !record.dispatched(),
        "the file has no dispatch line, so nothing may be claimed about the socket"
    );
    assert_eq!(
        record.nonce,
        Some(durable_nonce),
        "the nonce the crashed run minted is the nonce the file reserves"
    );
    assert_eq!(
        record.transaction_hash, durable_hash,
        "§5: the identity is the real local transaction hash, and it is durable"
    );
    assert_eq!(record.receipt_status, None, "no receipt was ever read here");
    assert!(
        record.needs_attention,
        "the summary flag is set for every record that still reserves a lane, so a reader of the \
         recovery alone is told to look"
    );
    assert_eq!(
        count_facts(&scratch.text(), JournalFact::AttentionRequired),
        0,
        "while §4.2's fact 8 is an event, not a summary: no attempt has been refused here yet, \
         so no line names one"
    );
    assert_eq!(
        recovery.lane_occupancy.len(),
        1,
        "and the lane this file reserves is one nonce: {:?}",
        recovery.lane_occupancy
    );

    // Re-enter as a new process and attempt the same plan. The endpoint is fresh, so every send it
    // counts is a send this test would have caused.
    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(
        second.sent_count(),
        0,
        "§4.3: a possibly-live transaction is never re-broadcast"
    );
    assert!(matches!(
        restarted.report.stopped_with,
        Some(ExecutionError::NonceUnavailable(_))
    ));
    assert_eq!(
        restarted.report.reached, None,
        "the attempt never became a record, so it cannot report a rung"
    );
    assert_eq!(
        count_facts(&scratch.text(), JournalFact::SendIntentPersisted),
        1,
        "the refused attempt wrote no second intent line, so the ledger still names one payload"
    );
    assert_eq!(
        count_facts(&scratch.text(), JournalFact::SendDispatched),
        0,
        "and no dispatch line appeared behind the cut"
    );
    assert_eq!(
        count_facts(&scratch.text(), JournalFact::AttentionRequired),
        1,
        "the refusal is §4.2's fact 8, written at the moment it happened"
    );
    assert_eq!(
        scratch.line_count(),
        kept + 1,
        "one line added by the restart, and only the attention one"
    );
    assert!(
        !restarted.stage.lane_is_idle(),
        "§11: the held nonce stays held"
    );

    // §10: what this case measured, in three rows. The `crate::` path is spelled out because a
    // case binds a local named `record` to the execution it recovered, and the row below quotes
    // that local rather than a copy of it.
    crate::record(json!({
        "table": "recovery_results",
        "section": "F1",
        "case": "a_crash_between_the_intent_line_and_the_socket_leaves_the_nonce_possibly_in_flight",
        "fault": "§8 F1 / §4.2's fact 2 / §4.3's prohibition on reading an intent as an absence",
        "damage": "process_exit",
        "crash_shape": "prefix cut at the intent line",
        "last_fact_before_crash": record.last_fact.name(),
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "dispatch_line_claimed": record.dispatched(),
        "receipt_read": record.receipt_status.is_some(),
        "needs_attention": record.needs_attention,
        "attention_lines_after_restart": count_facts(&scratch.text(), JournalFact::AttentionRequired),
        "records": recovery.records.len(),
        "lines": kept,
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F1",
        "case": "a_crash_between_the_intent_line_and_the_socket_leaves_the_nonce_possibly_in_flight",
        "fault": "§8 F1 / §4.1's boundary: the intent line landed before the socket was touched",
        "intent_seq": kept,
        "dispatch_seq": Value::Null,
        "send_arrivals": first.sent_count(),
        "local_hash": hash_value(durable_hash),
        "tracked_hash": hash_value(record.transaction_hash),
        "hashes_agree": durable_hash == record.transaction_hash,
        "line_count": scratch.line_count(),
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F1",
        "case": "a_crash_between_the_intent_line_and_the_socket_leaves_the_nonce_possibly_in_flight",
        "fault": "§8 F1 / §4.3's lane rule / §11's no-release",
        "nonce_held": nonce_value(Some(durable_nonce)),
        "lane_word": lane_word(!restarted.stage.lane_is_idle()),
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
        "restored_lane_entries": recovery.lane_occupancy.len(),
    }));
}

// ---------------------------------------------------------------------------
// F2 — an `Unknown` answer survives as `Unknown`
// ---------------------------------------------------------------------------

/// §8's F2, §4.3's core prohibition. The three lines the run wrote are in file order, the record
/// folds as `awaiting_receipt`, and no reading of the file turns the missing answer into a refusal.
#[tokio::test]
async fn an_unknown_answer_survives_the_restart_as_unknown_and_never_as_a_refusal() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f2-unknown");
    let first = Arc::new(Scripted::new().answer(unknown()));
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;

    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Signed));
    assert!(crashed.report.sent, "bytes left this process; §2.2 says so");
    assert!(matches!(crashed.report.lane, LaneRelease::Held { .. }));
    drop(crashed);

    let text = scratch.text();
    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("re-read from bytes");
    let record = one_record(&recovery);
    assert_eq!(
        record.facts,
        vec![
            JournalFact::SendIntentPersisted,
            JournalFact::SendDispatched,
            JournalFact::OutcomeUnknown,
        ],
        "the three lines the run wrote, in the order it wrote them"
    );
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert!(record.dispatched());
    assert!(
        record.transaction_hash.is_some(),
        "§4.3: the local hash is persisted"
    );
    assert_eq!(record.receipt_status, None);
    assert_eq!(
        record.last_fact.basis(),
        evm_execution::FactBasis::UnconfirmedInference,
        "§4.2: the unknown answer is marked an inference at the field level"
    );
    assert!(
        !text.contains(&format!(
            "\"fact\":\"{}\"",
            JournalFact::DefiniteRefusal.name()
        )),
        "§4.3's never-convert rule, read off the file rather than off a type"
    );
    assert!(text.contains("\"basis\":\"unconfirmed_inference\""));

    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(
        second.sent_count(),
        0,
        "a restarted process does not finish what the crashed one left unknown"
    );
    assert_eq!(restarted.report.reached, None);
    let again = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("still readable");
    assert_eq!(
        again.records[0].state,
        RecoveredState::AwaitingReceipt,
        "the attention line does not close the record"
    );

    crate::record(json!({
        "table": "recovery_results",
        "section": "F2",
        "case": "an_unknown_answer_survives_the_restart_as_unknown_and_never_as_a_refusal",
        "fault": "§8 F2 / §4.3's core prohibition: an Unknown is never read back as a refusal",
        "damage": "process_exit",
        "crash_shape": "the run's own three lines, re-read uncut",
        "last_fact_before_crash": record.last_fact.name(),
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "never_written_fact": JournalFact::DefiniteRefusal.name(),
        "facts_in_order": record
            .facts
            .iter()
            .map(|fact| fact.name())
            .collect::<Vec<_>>(),
        "last_fact_basis": record.last_fact.basis().name(),
        "dispatch_line_claimed": record.dispatched(),
        "receipt_read": record.receipt_status.is_some(),
        "records": recovery.records.len(),
        "lines": text.lines().count(),
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F2",
        "case": "an_unknown_answer_survives_the_restart_as_unknown_and_never_as_a_refusal",
        "fault": "§8 F2 / §4.2's fact ordering, read off the file rather than off a type",
        "intent_precedes_dispatch": fact_precedes(
            &record.facts,
            JournalFact::SendIntentPersisted,
            JournalFact::SendDispatched,
        ),
        "send_arrivals": first.sent_count(),
        "refusal_lines_in_file": count_facts(&text, JournalFact::DefiniteRefusal),
        "line_count": text.lines().count(),
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F2",
        "case": "an_unknown_answer_survives_the_restart_as_unknown_and_never_as_a_refusal",
        "fault": "§8 F2 / §4.3's lane rule across a second read",
        "holds_lane": again.records[0].state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
    }));
}

// ---------------------------------------------------------------------------
// F3 / F4 — the two boundaries around the POST
// ---------------------------------------------------------------------------

/// §8's F3: the dispatch line is the last thing the file holds, so recovery says "the request went
/// out" and nothing more.
#[tokio::test]
async fn a_dispatch_line_with_no_answer_after_it_is_recovered_as_still_awaiting() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f3-dispatch");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .receipt(Some(receipt(true, sender))),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    let durable_hash = crashed
        .report
        .record
        .as_ref()
        .expect("a record")
        .transaction_hash;
    // `crash_after` answers with the line it stopped at, which is the number the row below quotes.
    let kept = crash_after(&scratch.journal(), JournalFact::SendDispatched);
    drop(crashed);

    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("a prefix with no answer");
    let record = one_record(&recovery);
    assert_eq!(record.last_fact, JournalFact::SendDispatched);
    assert!(record.dispatched());
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert_eq!(
        record.transaction_hash, durable_hash,
        "the hash the crashed run computed locally is the hash the file still names"
    );
    assert_ne!(
        record.state,
        RecoveredState::Resolved,
        "a dispatch line with no answer after it closes nothing"
    );

    crate::record(json!({
        "table": "recovery_results",
        "section": "F3",
        "case": "a_dispatch_line_with_no_answer_after_it_is_recovered_as_still_awaiting",
        "fault": "§8 F3 / §4.2's fact 3: a dispatch line says the request went out and nothing more",
        "damage": "process_exit",
        "crash_shape": "prefix cut at the dispatch line",
        "last_fact_before_crash": record.last_fact.name(),
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "dispatch_line_claimed": record.dispatched(),
        "records": recovery.records.len(),
        "lines": kept,
        "restarts": 0,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F3",
        "case": "a_dispatch_line_with_no_answer_after_it_is_recovered_as_still_awaiting",
        "fault": "§8 F3 / §5's identity rule: the hash the run computed is the hash the file names",
        "dispatch_seq": kept,
        "local_hash": hash_value(durable_hash),
        "tracked_hash": hash_value(record.transaction_hash),
        "hashes_agree": durable_hash == record.transaction_hash,
    }));
}

/// §8's F4: a node acknowledgement is recovered as an acknowledgement. §2.3's separation has to
/// survive the crash, because reading an acknowledgement as inclusion is the one misclassification
/// that would hand a live nonce back to a restarted process.
#[tokio::test]
async fn a_node_acknowledgement_is_recovered_as_an_acknowledgement_and_never_as_inclusion() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f4-accepted");
    // Accepted, and no receipt ever arrives: the file ends at the acknowledgement on its own.
    let first = Arc::new(Scripted::new().answer(accepted()));
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Submitted));
    assert_eq!(
        crashed.report.receipt_answer,
        Some(ReceiptStatus::Timeout),
        "§26: running out of receipt budget is not a failure"
    );
    assert!(crashed.report.stopped_with.is_none());
    drop(crashed);

    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("re-read");
    let record = one_record(&recovery);
    assert_eq!(record.last_fact, JournalFact::EndpointAccepted);
    assert!(!JournalFact::EndpointAccepted.is_terminal());
    assert_eq!(
        record.receipt_status, None,
        "an acknowledgement is not a receipt"
    );
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert_eq!(record.pinned_block, Some(SIM_BLOCK));
    assert_eq!(record.pinned_block_hash, Some(sim_hash()));

    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(second.sent_count(), 0, "and it is not resent to find out");
    assert!(matches!(
        restarted.report.stopped_with,
        Some(ExecutionError::NonceUnavailable(_))
    ));

    crate::record(json!({
        "table": "recovery_results",
        "section": "F4",
        "case": "a_node_acknowledgement_is_recovered_as_an_acknowledgement_and_never_as_inclusion",
        "fault": "§8 F4 / §2.3's separation: an acknowledgement is not an inclusion",
        "damage": "process_exit",
        "crash_shape": "the file ends at the acknowledgement, which the run never followed up",
        "last_fact_before_crash": record.last_fact.name(),
        "last_fact_is_terminal": record.last_fact.is_terminal(),
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "receipt_read": record.receipt_status.is_some(),
        "pinned_block": record.pinned_block,
        "pinned_block_hash": record.pinned_block_hash.map(|hash| format!("{hash:#x}")),
        "records": recovery.records.len(),
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F4",
        "case": "a_node_acknowledgement_is_recovered_as_an_acknowledgement_and_never_as_inclusion",
        "fault": "§8 F4 / §4.1's boundary, seen from the acknowledgement side",
        "send_arrivals": first.sent_count(),
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F4",
        "case": "a_node_acknowledgement_is_recovered_as_an_acknowledgement_and_never_as_inclusion",
        "fault": "§8 F4 / §4.3's lane rule: an open acknowledgement keeps the nonce",
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
        "restart_refused_at_the_lane": matches!(
            restarted.report.stopped_with,
            Some(ExecutionError::NonceUnavailable(_))
        ),
    }));
}

/// §8's F5: a receipt read in memory that never reached the file. This is the divergence §9 asks
/// about, and the recovery side of the answer: the file's ignorance wins.
#[tokio::test]
async fn a_receipt_read_in_memory_that_never_reached_the_file_leaves_the_record_awaiting() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f5-receipt-lost");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .receipt(Some(receipt(true, sender))),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    assert_eq!(
        crashed.report.receipt_answer,
        Some(ReceiptStatus::Included),
        "the crashed process really did read the receipt"
    );
    // What the memory side knew, kept as the one word the row below quotes. It is read off the
    // same field the assertion above compared, before the process's state is dropped.
    let receipt_read_in_memory = crashed.report.receipt_answer == Some(ReceiptStatus::Included);
    let kept = crash_after(&scratch.journal(), JournalFact::EndpointAccepted);
    drop(crashed);

    let text = scratch.text();
    assert_eq!(count_facts(&text, JournalFact::ReceiptObserved), 0);
    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("the prefix re-reads");
    let record = one_record(&recovery);
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert_eq!(record.receipt_status, None);

    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(second.sent_count(), 0);
    assert!(
        !restarted.stage.lane_is_idle(),
        "the nonce stays reserved until a receipt is read *again*, from the file's own state"
    );

    crate::record(json!({
        "table": "recovery_results",
        "section": "F5",
        "case": "a_receipt_read_in_memory_that_never_reached_the_file_leaves_the_record_awaiting",
        "fault": "§8 F5 / §9's no-silent-divergence: the file's ignorance wins over memory's knowledge",
        "damage": "process_exit",
        "crash_shape": "prefix cut at the acknowledgement, after the receipt was read in memory",
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "receipt_read": record.receipt_status.is_some(),
        "receipt_lines_in_file": count_facts(&text, JournalFact::ReceiptObserved),
        "receipt_read_in_memory_before_the_cut": receipt_read_in_memory,
        "records": recovery.records.len(),
        "lines": kept,
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F5",
        "case": "a_receipt_read_in_memory_that_never_reached_the_file_leaves_the_record_awaiting",
        "fault": "§8 F5 / §4.3's lane rule: a receipt memory held and the file never saw frees nothing",
        "lane_word": lane_word(!restarted.stage.lane_is_idle()),
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
    }));
}

/// §8's F5 with a *real* write failure rather than a prefix cut, and §9's question at its sharpest:
/// the file is unlinked and rewritten byte-identically while the run is between its own two writes,
/// so a length check sees nothing and the append would land on an inode nobody will read again.
/// The refusal is the witness that memory did not quietly go ahead.
#[cfg(unix)]
#[tokio::test]
async fn a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it()
{
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f5b-replaced");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .receipt(Some(receipt(true, sender)))
            .damage(REPLACE_BEFORE_RECEIPT, scratch.journal()),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;

    assert_eq!(
        first.sent_count(),
        1,
        "the bytes did leave; this test does not pretend otherwise"
    );
    let stopped = match &crashed.report.stopped_with {
        Some(error) => error,
        None => panic!(
            "a journal that was replaced underneath the run cannot end the run quietly: {}",
            crashed.report.detail
        ),
    };
    assert!(
        matches!(stopped, ExecutionError::LedgerPersistence(_)),
        "§4.1's write-side error, got {stopped}"
    );
    assert!(
        stopped.to_string().contains("replaced_externally"),
        "and it names which damage: {stopped}"
    );
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Failed));

    // What the file holds, after the refusal. The refused line wrote nothing, so the ledger is a
    // clean prefix and its last fact is the acknowledgement the run already made durable.
    let text = scratch.text();
    assert_eq!(count_facts(&text, JournalFact::ReceiptObserved), 0);
    assert_eq!(count_facts(&text, JournalFact::EndpointAccepted), 1);
    assert!(text.ends_with('\n'), "and no partial line is left behind");

    // §9's no-divergence claim, checked on both sides: the run that could not write did not free
    // the nonce either, so memory and disk still agree that it is held.
    assert!(
        !crashed.stage.lane_is_idle(),
        "memory kept the lane it could not make durable"
    );
    // The refusal's class, taken while the process's state is still alive, because the row at the
    // end of the case has to quote it. Its sentence names the damage, and the case above has
    // already proved which word that was.
    let persistence_refused = matches!(stopped, ExecutionError::LedgerPersistence(_));
    drop(crashed);

    let recovery = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("the prefix re-reads");
    let record = one_record(&recovery);
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert_eq!(record.last_fact, JournalFact::EndpointAccepted);
    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(
        second.sent_count(),
        0,
        "and the restarted process is not the one that resolves it"
    );
    assert!(
        matches!(
            restarted.report.stopped_with,
            Some(ExecutionError::NonceUnavailable(_))
        ),
        "the acknowledgement the file holds still reserves the nonce: {:?}",
        restarted.report.stopped_with
    );
    assert!(!restarted.stage.lane_is_idle());

    crate::record(json!({
        "table": "recovery_results",
        "section": "F5",
        "case": "a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it",
        "fault": "§8 F5 / §9: a ledger replaced under a run leaves the file's own state to recover",
        "damage": "file_replaced_under_run",
        "crash_shape": "the file was unlinked and rewritten byte-for-byte mid-run, then re-read",
        "last_fact_before_crash": record.last_fact.name(),
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "receipt_lines_in_file": count_facts(&text, JournalFact::ReceiptObserved),
        "records": recovery.records.len(),
        "lines": text.lines().count(),
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F5",
        "case": "a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it",
        "fault": "§8 F5 / §4.1's write side: the line that could not be made durable wrote nothing",
        "send_arrivals": first.sent_count(),
        "persistence_refused": persistence_refused,
        "refused_line_landed": count_facts(&text, JournalFact::ReceiptObserved) != 0,
        "acknowledgement_lines": count_facts(&text, JournalFact::EndpointAccepted),
        "line_count": text.lines().count(),
    }));
    crate::record(json!({
        "table": "damage_controls",
        "section": "F5",
        "case": "a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it",
        "fault": "§8 F5 / §4.1's fail-closed write / §9's no-silent-divergence",
        "damage": "file_replaced_under_run",
        "fault_word": "replaced_externally",
        "refused_at": "append",
        "opened_before_damage": true,
        "sends_after_refusal": second.sent_count(),
        "file_still_present": scratch.journal().exists(),
        "durable_lines_after_refusal": text.lines().count(),
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F5",
        "case": "a_ledger_replaced_under_a_run_refuses_the_next_line_instead_of_forking_memory_against_it",
        "fault": "§8 F5 / §4.3's lane rule: memory kept what the file could not",
        "lane_word": lane_word(!restarted.stage.lane_is_idle()),
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
    }));
}

// ---------------------------------------------------------------------------
// F6 — every damage shape is detected and refused
// ---------------------------------------------------------------------------

/// §8's F6 / §6: a damaged ledger is refused at startup, in the node's own words, and the refusal
/// writes nothing back. Each row edits a copy of a ledger a real run wrote, so the shapes are
/// shapes a crash can actually leave rather than ones this file invented.
#[tokio::test]
async fn a_damaged_ledger_refuses_to_open_and_the_run_never_reaches_the_endpoint() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let seed = Scratch::new("f6-seed");
    let first = Arc::new(Scripted::new().answer(unknown()));
    let crashed = attempt(seed.dir(), first.clone(), &plan).await;
    assert_eq!(
        first.sent_count(),
        1,
        "the seed ledger is one a real run wrote"
    );
    drop(crashed);
    let whole = seed.text();
    let lines: Vec<&str> = whole.lines().collect();
    assert!(lines.len() >= 4, "the seed ledger is a real one: {whole}");

    // §10: the seed, before any damage is cut out of it. The unknown line is named by production's
    // own word rather than by a copy of it, so a rename here fails the case rather than quietly
    // editing a file that no longer carries the fact the nine damages below are cut around.
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F6",
        "case": "a_damaged_ledger_refuses_to_open_and_the_run_never_reaches_the_endpoint",
        "fault": "§8 F6's seed: the nine damages are copies of a file a real run wrote",
        "send_arrivals": first.sent_count(),
        "line_count": lines.len(),
        "unknown_line_in_seed": whole.contains(&format!(
            "\"fact\":\"{}\"",
            JournalFact::OutcomeUnknown.name()
        )),
    }));

    /// Replace the file's last line and put the rest back. A damage that survives as parseable
    /// JSON is a damage the reader has to classify, which is what §6 asks for; a damage the parser
    /// rejects on its face would prove nothing about this build.
    fn with_edited_last_line(lines: &[&str], from: &str, to: &str) -> String {
        let last = lines[lines.len() - 1];
        let edited = last.replace(from, to);
        assert_ne!(
            edited, last,
            "the edit {from} → {to} has to land on the last line"
        );
        let mut body = lines[..lines.len() - 1].join("\n");
        body.push('\n');
        body.push_str(&edited);
        body.push('\n');
        body
    }

    let torn = {
        let mut body = whole.clone();
        // Drop the file's final newline and 24 bytes of the last line: a writer that stopped
        // mid-line, which is the residue §6's truncation question is about.
        body.pop();
        body.truncate(body.len() - 24);
        body
    };
    let blanked = {
        let mut body = String::new();
        for (index, line) in lines.iter().enumerate() {
            body.push_str(line);
            body.push('\n');
            if index == 1 {
                body.push('\n');
            }
        }
        body
    };

    let cases: Vec<(&str, u64, String)> = vec![
        ("emptied_externally", CHAIN, String::new()),
        ("missing_genesis", CHAIN, lines[1..].join("\n") + "\n"),
        ("torn_tail", CHAIN, torn),
        ("empty_line", CHAIN, blanked),
        (
            "unknown_fact",
            CHAIN,
            with_edited_last_line(
                &lines,
                "\"fact\":\"outcome_unknown\"",
                "\"fact\":\"outcome_uncertain\"",
            ),
        ),
        (
            "mismatched_basis",
            CHAIN,
            with_edited_last_line(
                &lines,
                "\"basis\":\"unconfirmed_inference\"",
                "\"basis\":\"observed_local\"",
            ),
        ),
        (
            "checksum_mismatch",
            CHAIN,
            // `detail` is the one field the reader never judges on its own, so a hand edit there
            // survives parsing and can only be caught by the line's own checksum.
            with_edited_last_line(&lines, "\"detail\":\"scripted:", "\"detail\":\"edited:"),
        ),
        (
            "unsupported_schema",
            CHAIN,
            // The version is checked before the checksum is recomputed, so a bumped number reads
            // as a schema this build does not define rather than as an edit.
            with_edited_last_line(&lines, "\"schema_version\":1", "\"schema_version\":2"),
        ),
        // A line about another chain cannot be faked by editing `chain_id`: the checksum covers it,
        // so the edit would surface as `checksum_mismatch`. The honest shape is a file written for
        // one chain read by an entry configured for another.
        ("foreign_chain", OTHER_CHAIN, whole.clone()),
    ];

    for (token, chain, bytes) in cases {
        let scratch = Scratch::new(&format!("f6-{token}"));
        let path = journal_at(scratch.dir(), chain);
        std::fs::write(&path, &bytes).expect("the damaged copy");
        let scripted = silent_endpoint();

        let error = match ExecutionJournal::open(scratch.dir(), chain, journal_stamp()) {
            Ok(journal) => panic!(
                "{token}: a damaged ledger was accepted as recoverable ({} lines kept reading it)",
                journal.entries()
            ),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(token),
            "{token}: the refusal has to name the damage it found, got {error}"
        );
        let reload_error = ExecutionJournal::reload(scratch.dir(), chain)
            .expect_err("reading the same bytes back answers the same way");
        assert!(
            reload_error.to_string().contains(token),
            "{token}: reload says {reload_error} while open said {error}"
        );

        // §7's ordering, measured at the entry rather than at the type: the entry refuses before a
        // stage exists, so nothing is read and nothing is sent.
        let entered = enter_chain(scratch.dir(), chain, scripted.clone()).err();
        assert!(
            entered.is_some(),
            "{token}: the production entry got a stage anyway"
        );

        assert_eq!(
            scripted.call_count(),
            0,
            "{token}: a run that cannot recover does not ask the node anything"
        );
        assert_eq!(scripted.sent_count(), 0, "{token}: and it does not send");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file is still there"),
            bytes,
            "{token}: §6's 「不得自动修复」 — the refusal appended nothing, so the bytes a human \
             needs in order to diagnose the damage are still the bytes that were found"
        );

        crate::record(json!({
            "table": "damage_controls",
            "section": "F6",
            "case": "a_damaged_ledger_refuses_to_open_and_the_run_never_reaches_the_endpoint",
            "fault": format!(
                "§8 F6 / §6's fail-closed rule: `{token}` was refused at the open, not repaired"
            ),
            "damage": token,
            "fault_word": token,
            "refused_at": "open",
            "chain": chain,
            "ledger_file": journal_file_name(chain),
            "damage_lines": bytes.lines().count(),
            "opened": false,
            "entry_refused": entered.is_some(),
            "calls_after_refusal": scripted.call_count(),
            "sends_after_refusal": scripted.sent_count(),
            "bytes_unchanged": std::fs::read_to_string(&path).expect("the file is still there")
                == bytes,
            "file_still_present": path.exists(),
        }));
    }
}

/// §8's F6 / §9's concurrency answer: one file, two handles. Each handle numbers its own lines
/// from what the file said when it opened, so the collision is a fact about the file that the next
/// reader — and only the next reader — can see.
#[test]
fn two_handles_on_one_file_collide_at_the_sequence_number_a_reader_checks() {
    let scratch = Scratch::new("f6b-two-handles");
    let sender = synthetic_address();
    let step = |label: &str| StepIdentity {
        label: label.to_string(),
        chain_id: CHAIN,
        sender,
        target: EXECUTOR,
        nonce: 0,
        pinned_block: Some(SIM_BLOCK),
        pinned_block_hash: Some(sim_hash()),
        signing_hash: run_one(),
    };

    let mut first = ExecutionJournal::open(scratch.dir(), CHAIN, journal_stamp()).expect("open");
    let mut second = ExecutionJournal::open(scratch.dir(), CHAIN, journal_stamp()).expect("twice");
    first
        .append(
            journal_stamp(),
            JournalRecord::from_step(&step("handle-a"), JournalFact::SendIntentPersisted),
        )
        .expect("the first handle wrote its line");
    second
        .append(
            journal_stamp(),
            JournalRecord::from_step(&step("handle-b"), JournalFact::SendIntentPersisted),
        )
        .expect("the second handle wrote its own, not knowing");
    // What each handle believes it wrote, read off the handle's own counter while the handle is
    // still alive — the row at the end of the case quotes these two numbers.
    let written_by_first = first.appended();
    let written_by_second = second.appended();
    drop(first);
    drop(second);

    // The third number the row needs: what the file itself holds. Both handles' lines are in it,
    // physically — neither overwrote the other — which is exactly why the two intent lines carry
    // the same sequence number and the next reader has to refuse.
    let lines_in_file = scratch.line_count();
    assert_eq!(
        lines_in_file,
        (written_by_first + written_by_second) as usize,
        "the collision is only real if the file holds every line either handle wrote; it holds \
         {lines_in_file} and the handles wrote {written_by_first} and {written_by_second}"
    );

    let error = ExecutionJournal::reload(scratch.dir(), CHAIN)
        .expect_err("two handles minted the same sequence number");
    assert!(
        error.to_string().contains("duplicate_sequence"),
        "and the next reader refuses rather than picking a winner: {error}"
    );
    // The same refusal a live handle meets before it can send anything else.
    let error = ExecutionJournal::open(scratch.dir(), CHAIN, journal_stamp())
        .expect_err("a reopening entry refuses the file too");
    assert!(
        error.to_string().contains("duplicate_sequence"),
        "and an entry that has to recover refuses on the spot, before it reads the node: {error}"
    );

    // §10: this case measured the reader alone — the two handles never reached a stage, so the row
    // says nothing about calls or sends, and the gate's `what_this_table_is_not` records that.
    crate::record(json!({
        "table": "damage_controls",
        "section": "F6",
        "case": "two_handles_on_one_file_collide_at_the_sequence_number_a_reader_checks",
        "fault": "§8 F6 / §9's sync boundary: one file driven by two handles is what the next reader refuses",
        "damage": "two_handles_one_file",
        "fault_word": "duplicate_sequence",
        "refused_at": "open",
        "chain": CHAIN,
        "ledger_file": journal_file_name(CHAIN),
        "handle_a_lines": written_by_first,
        "handle_b_lines": written_by_second,
        "file_lines": lines_in_file,
        "opened": false,
    }));
}

// ---------------------------------------------------------------------------
// F7 — the same file read twice answers the same, and a finished attempt is a duplicate
// ---------------------------------------------------------------------------

/// §8's F7: recovery is idempotent and read-only. Two passes over one file give one answer, an
/// open that finds lines already written changes no byte, and the record a closed execution left
/// behind refuses a second attempt as the duplicate it is rather than sending it.
#[tokio::test]
async fn a_restarted_process_reads_the_same_record_twice_and_refuses_a_finished_attempt_as_duplicate(
) {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f7-idempotent");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .receipt(Some(receipt(true, sender))),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Included));
    drop(crashed);

    let bytes_before = scratch.text();
    let first_pass = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("pass one");
    let second_pass = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("pass two");
    assert_eq!(
        first_pass.records, second_pass.records,
        "one file, one answer"
    );
    assert_eq!(
        one_record(&first_pass).line(),
        one_record(&second_pass).line(),
        "and the line a human reads is the same string"
    );
    assert_eq!(one_record(&first_pass).state, RecoveredState::Resolved);
    assert!(
        first_pass.lane_occupancy.is_empty(),
        "a receipt closed the nonce: {:?}",
        first_pass.lane_occupancy
    );

    // Opening a ledger that already has lines writes nothing.
    let untouched = {
        let handle =
            ExecutionJournal::open(scratch.dir(), CHAIN, journal_stamp()).expect("re-open");
        assert!(
            !handle.recovery().fresh,
            "a file with lines is not a fresh one"
        );
        assert_eq!(handle.appended(), 0, "and re-opening appended nothing");
        scratch.text()
    };
    assert_eq!(untouched, bytes_before, "§7's recovery is a read");

    // The closed record still stands against a new attempt at the same state binding.
    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(
        second.sent_count(),
        0,
        "a finished execution is not run twice"
    );
    assert_eq!(restarted.metrics.get("execution_duplicate_in_ledger"), 1);
    assert!(
        restarted.report.detail.contains("M12-F §7"),
        "and it says which record it deferred to: {}",
        restarted.report.detail
    );
    assert_eq!(
        scratch.text(),
        bytes_before,
        "the duplicate refused wrote no line, so the ledger still describes one execution"
    );
    assert!(
        restarted.stage.lane_is_idle(),
        "and the nonce it minted for nothing went back"
    );

    // §10: three rows. The reopen's own counters (`appended()`, `recovery().fresh`) are asserted
    // inside the case but are not repeated here, because the row quotes the strings the equality
    // assertions compared rather than the handle that was already dropped.
    crate::record(json!({
        "table": "recovery_results",
        "section": "F7",
        "case": "a_restarted_process_reads_the_same_record_twice_and_refuses_a_finished_attempt_as_duplicate",
        "fault": "§8 F7 / §7's read-only recovery: one file, read twice, answers once",
        "damage": "clean_shutdown",
        "crash_shape": "no cut — the run finished, and the same file was read twice more",
        "recovered_state": one_record(&first_pass).state.name(),
        "second_pass_records_agree": first_pass.records == second_pass.records,
        "second_pass_line_agrees": one_record(&first_pass).line() == one_record(&second_pass).line(),
        "records": first_pass.records.len(),
        "lane_occupancy_after_resolution": first_pass.lane_occupancy.len(),
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F7",
        "case": "a_restarted_process_reads_the_same_record_twice_and_refuses_a_finished_attempt_as_duplicate",
        "fault": "§8 F7 / §7: recovery wrote no byte, and the duplicate it refused wrote none either",
        "lines_before_recovery": bytes_before.lines().count(),
        "lines_after_recovery": untouched.lines().count(),
        "first_pass_records": first_pass.records.len(),
        "second_pass_records": second_pass.records.len(),
        "send_arrivals": second.sent_count(),
        "duplicate_metric_lines": restarted.metrics.get("execution_duplicate_in_ledger"),
        "no_line_written_by_duplicate": scratch.text() == bytes_before,
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F7",
        "case": "a_restarted_process_reads_the_same_record_twice_and_refuses_a_finished_attempt_as_duplicate",
        "fault": "§8 F7 / §4.3's lane rule from the other side: a closed record holds nothing to hold",
        "nonce_held": Value::Null,
        "lane_word": lane_word(!restarted.stage.lane_is_idle()),
        "holds_lane": one_record(&first_pass).state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
        "restored_lane_entries": first_pass.lane_occupancy.len(),
    }));
}

// ---------------------------------------------------------------------------
// F8 — the ledger cannot be written
// ---------------------------------------------------------------------------

/// §8's F8 with §4.1's boundary and §6's fail-closed rule joined at the file: the ledger is emptied
/// between the nonce read and the send, so the intent line cannot land, `submit` is never reached,
/// and the *next* process refuses to read the emptied file as a fresh one.
#[tokio::test]
async fn a_ledger_emptied_before_the_intent_line_stops_the_send_and_the_next_process_refuses_it() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f8-emptied");
    let first = Arc::new(
        Scripted::new()
            .answer(accepted())
            .damage(TRUNCATE_BEFORE_SEND, scratch.journal()),
    );
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;

    assert_eq!(
        first.sent_count(),
        0,
        "§4.1's test: the HTTP send count is zero"
    );
    let error = match &crashed.report.stopped_with {
        Some(error) => error,
        None => panic!("a run that could not persist its intent must not end quietly"),
    };
    assert!(
        matches!(error, ExecutionError::LedgerPersistence(_)),
        "an explicit local persistence error, got {error}"
    );
    assert!(
        error.to_string().contains("truncated_externally"),
        "{error}"
    );
    assert!(
        !crashed.report.sent,
        "and the report does not call it submitted"
    );
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Failed));
    assert!(
        crashed.stage.journal().is_durable(),
        "§11: the failure is not a downgrade to memory-only mode"
    );
    assert!(
        scratch.text().is_empty(),
        "the damage is what the file still is"
    );

    let error = ExecutionJournal::open(scratch.dir(), CHAIN, journal_stamp())
        .expect_err("the next process refuses an emptied ledger rather than starting over");
    assert!(
        error.to_string().contains("emptied_externally"),
        "§6: 「不得在检测到损坏后自动创建空台账并继续交易」 — got {error}"
    );
    assert!(
        scratch.text().is_empty(),
        "and the refusal wrote no opening line, so the file is exactly as the damage left it"
    );
    assert_eq!(
        first.sent_count(),
        0,
        "nothing sent before, nothing sent by the refusal either"
    );

    // §10: the intent line's absence is the finding here, so the row names it `null` — the case
    // proved the file holds no line at all — and the send count stays at the zero it asserts.
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F8",
        "case": "a_ledger_emptied_before_the_intent_line_stops_the_send_and_the_next_process_refuses_it",
        "fault": "§8 F8 / §4.1's boundary: no intent line means no socket, and no memory-only mode",
        "intent_seq": Value::Null,
        "intent_lines_in_file": count_facts(&scratch.text(), JournalFact::SendIntentPersisted),
        "send_arrivals": first.sent_count(),
        "report_sent": crashed.report.sent,
        "stopped_at_failed_rung": crashed.report.reached == Some(ExecutionStatus::Failed),
        "persistence_refused": matches!(
            crashed.report.stopped_with,
            Some(ExecutionError::LedgerPersistence(_))
        ),
        "durable_handle": crashed.stage.journal().is_durable(),
        "line_count": scratch.text().lines().count(),
    }));
    crate::record(json!({
        "table": "damage_controls",
        "section": "F8",
        "case": "a_ledger_emptied_before_the_intent_line_stops_the_send_and_the_next_process_refuses_it",
        "fault": "§8 F8 / §4.1's fail-closed write: the intent line could not land, so the send never happened",
        "damage": "ledger_emptied_mid_run",
        "fault_word": "truncated_externally",
        "refused_at": "append",
        "opened_before_damage": true,
        "sends_after_refusal": first.sent_count(),
        "file_still_present": scratch.journal().exists(),
        "durable_lines_after_refusal": scratch.text().lines().count(),
    }));
    crate::record(json!({
        "table": "damage_controls",
        "section": "F8",
        "case": "a_ledger_emptied_before_the_intent_line_stops_the_send_and_the_next_process_refuses_it",
        "fault": "§8 F8 / §6's fail-closed rule: the next process refuses an emptied ledger instead of starting over",
        "damage": "ledger_emptied_before_open",
        "fault_word": "emptied_externally",
        "refused_at": "open",
        "chain": CHAIN,
        "ledger_file": journal_file_name(CHAIN),
        "opened": false,
        "sends_after_refusal": first.sent_count(),
        "file_still_present": scratch.journal().exists(),
        "durable_lines_after_refusal": scratch.text().lines().count(),
    }));
}

/// §8's F8, first shape: the ledger cannot be reached at all. The directory path is held by a
/// regular file, so `create_dir_all` fails at the entry — before a stage exists to sign or send.
#[tokio::test]
async fn a_ledger_path_that_cannot_be_created_stops_the_run_before_anything_is_read() {
    let scratch = Scratch::new("f8b-blocked-dir");
    let blocked = scratch.dir().join("ledger");
    std::fs::write(&blocked, "not a directory").expect("a file sitting where the ledger dir goes");
    let scripted = silent_endpoint();

    let error = enter(&blocked, scripted.clone())
        .err()
        .expect("an entry that cannot persist cannot open");
    assert!(
        matches!(error, ExecutionError::LedgerPersistence(_)),
        "§4.1's error, not a warning: {error}"
    );
    assert!(error.to_string().contains("io"), "{error}");
    assert_eq!(
        scripted.call_count(),
        0,
        "§7: recovery comes before the entry, and this one never reached it"
    );
    assert_eq!(scripted.sent_count(), 0);

    // §10: the case never got as far as naming a chain or a file — the directory itself refused —
    // so the row says only what this case observed: the refusal's class and the two zero counters.
    crate::record(json!({
        "table": "damage_controls",
        "section": "F8",
        "case": "a_ledger_path_that_cannot_be_created_stops_the_run_before_anything_is_read",
        "fault": "§8 F8 / §4.1's error type: an unreachable ledger dir is a refusal, not a warning",
        "damage": "ledger_dir_unreachable",
        "fault_word": "io",
        "refused_at": "open",
        "opened": false,
        "persistence_error": matches!(error, ExecutionError::LedgerPersistence(_)),
        "calls_after_refusal": scripted.call_count(),
        "sends_after_refusal": scripted.sent_count(),
    }));
}

// ---------------------------------------------------------------------------
// F9 — nothing is substituted for the transaction that is possibly live
// ---------------------------------------------------------------------------

/// §8's F9, §4.3's second and third prohibitions: no substitute transaction and no released nonce
/// while one is possibly live — and the need for a human is written once, not once per restart.
#[tokio::test]
async fn a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let other = substituted(sender);
    let scratch = Scratch::new("f9-substitute");
    let first = Arc::new(Scripted::new().answer(unknown()));
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    let held_id =
        one_record(&ExecutionJournal::reload(scratch.dir(), CHAIN).expect("the unknown ledger"))
            .execution_id
            .clone();
    drop(crashed);

    // A different size of the same route is a different attempt, and it still cannot be sent: the
    // lane is the one thing a restart may not reallocate around.
    let second = silent_endpoint();
    let substituted_run = attempt(scratch.dir(), second.clone(), &other).await;
    assert_eq!(
        second.sent_count(),
        0,
        "§4.3: no substitute transaction goes out while one is possibly live"
    );
    assert!(matches!(
        substituted_run.report.stopped_with,
        Some(ExecutionError::NonceUnavailable(_))
    ));
    // The same reading the assertion above made, kept because the process's state is dropped below
    // and the row at the end of the case still has to quote it.
    let substitute_refused = matches!(
        substituted_run.report.stopped_with,
        Some(ExecutionError::NonceUnavailable(_))
    );
    let text = scratch.text();
    assert_eq!(
        count_facts(&text, JournalFact::SendIntentPersisted),
        1,
        "and the ledger still names exactly the crashed run's payload"
    );
    let attention_line = text
        .lines()
        .find(|line| line.contains("\"fact\":\"attention_required\""))
        .expect("the refusal was written down");
    assert!(
        attention_line.contains(&held_id),
        "the line names the record it is holding, so a reader knows what to look at"
    );
    drop(substituted_run);

    // A third process meets the same refusal and adds no second note about it.
    let third = silent_endpoint();
    let again = attempt(scratch.dir(), third.clone(), &plan).await;
    assert_eq!(third.sent_count(), 0);
    assert_eq!(
        count_facts(&scratch.text(), JournalFact::AttentionRequired),
        1,
        "one need, written once, however many processes meet it"
    );
    let again_read = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("still readable");
    let record = one_record(&again_read);
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);
    assert!(record.needs_attention);
    assert!(!again.stage.lane_is_idle());

    // §10: the execution id the attention line names is deliberately not copied into the row —
    // the row carries the boolean the case asserted about it, which keeps the table free of
    // anything that looks like a payload.
    crate::record(json!({
        "table": "recovery_results",
        "section": "F9",
        "case": "a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once",
        "fault": "§8 F9 / §4.3's second prohibition: a hold survives two more processes and closes nothing",
        "damage": "process_exit",
        "crash_shape": "the unknown run's own lines, met again and again by later processes",
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "needs_attention": record.needs_attention,
        "intent_lines_after_substitute": count_facts(&text, JournalFact::SendIntentPersisted),
        "attention_lines_after_every_restart": count_facts(
            &scratch.text(),
            JournalFact::AttentionRequired,
        ),
        "attention_line_names_the_held_record": attention_line.contains(&held_id),
        "records": again_read.records.len(),
        "lines": scratch.text().lines().count(),
        "restarts": 2,
    }));
    crate::record(json!({
        "table": "persistence_boundary",
        "section": "F9",
        "case": "a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once",
        "fault": "§8 F9 / §4.3's third prohibition: a substituted attempt is refused before it can write an intent",
        "substitute_send_arrivals": second.sent_count(),
        "third_process_send_arrivals": third.sent_count(),
        "substitute_refused_at_the_lane": substitute_refused,
        "intent_lines_in_file": count_facts(&text, JournalFact::SendIntentPersisted),
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F9",
        "case": "a_recovered_hold_refuses_a_substituted_attempt_and_writes_the_need_for_a_human_once",
        "fault": "§8 F9 / §11's no-release: the third process still finds the lane reserved",
        "lane_word": lane_word(!again.stage.lane_is_idle()),
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": third.sent_count(),
    }));
}

/// §8's F9, receipt side: a null receipt is never an answer about the transaction. §4.3 forbids
/// releasing the nonce on one, so the record stays unresolved across a restart and nothing is
/// resent to make it resolve.
#[tokio::test]
async fn an_accepted_transaction_whose_receipt_never_arrives_keeps_its_nonce_across_a_restart() {
    let sender = synthetic_address();
    let plan = executable(sender);
    let scratch = Scratch::new("f9-null-receipt");
    let first = Arc::new(Scripted::new().answer(accepted()));
    let crashed = attempt(scratch.dir(), first.clone(), &plan).await;
    assert_eq!(crashed.report.reached, Some(ExecutionStatus::Submitted));
    assert_eq!(
        crashed.report.receipt_answer,
        Some(ReceiptStatus::Timeout),
        "the budget ran out; §26 says that is a failure to learn, not a failure of the trade"
    );
    drop(crashed);

    let text = scratch.text();
    assert_eq!(
        count_facts(&text, JournalFact::DefiniteRefusal),
        0,
        "a null receipt is never written as a refusal"
    );
    let reread = ExecutionJournal::reload(scratch.dir(), CHAIN).expect("re-read");
    let record = one_record(&reread);
    assert_eq!(record.state, RecoveredState::AwaitingReceipt);

    // A restarted process never gets to ask the node again: the restored hold refuses it at the
    // nonce step, which is §4.3's point — the missing receipt is not evidence of absence.
    let second = silent_endpoint();
    let restarted = attempt(scratch.dir(), second.clone(), &plan).await;
    assert_eq!(
        second.sent_count(),
        0,
        "and it does not re-send to ask again"
    );
    assert!(
        matches!(
            restarted.report.stopped_with,
            Some(ExecutionError::NonceUnavailable(_))
        ),
        "the refusal is the lane still being held: {:?}",
        restarted.report.stopped_with
    );
    assert!(
        !restarted.stage.lane_is_idle(),
        "§4.3: no receipt, no release"
    );

    // §10: this case measured the file's refusal to call a null receipt an answer, and the lane
    // that consequence holds. It read no nonce number and no send tally for the first process, so
    // the rows carry neither.
    crate::record(json!({
        "table": "recovery_results",
        "section": "F9",
        "case": "an_accepted_transaction_whose_receipt_never_arrives_keeps_its_nonce_across_a_restart",
        "fault": "§8 F9 / §4.3: a null receipt is not an answer, so the record stays open across the restart",
        "damage": "process_exit",
        "crash_shape": "the run ended with the acknowledgement as its last word; the receipt never arrived",
        "recovered_state": record.state.name(),
        "never_recovered_as": "resolved",
        "never_written_fact": JournalFact::DefiniteRefusal.name(),
        "refusal_lines_in_file": count_facts(&text, JournalFact::DefiniteRefusal),
        "receipt_read": record.receipt_status.is_some(),
        "records": reread.records.len(),
        "lines": text.lines().count(),
        "restarts": 1,
    }));
    crate::record(json!({
        "table": "lane_recovery",
        "section": "F9",
        "case": "an_accepted_transaction_whose_receipt_never_arrives_keeps_its_nonce_across_a_restart",
        "fault": "§8 F9 / §4.3's lane rule: a receipt that never arrived releases nothing",
        "lane_word": lane_word(!restarted.stage.lane_is_idle()),
        "holds_lane": record.state.holds_lane(),
        "resubmissions_after_restart": second.sent_count(),
        "restart_refused_at_the_lane": matches!(
            restarted.report.stopped_with,
            Some(ExecutionError::NonceUnavailable(_))
        ),
    }));
}
