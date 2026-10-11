//! §46's arrow, assembled: the one place in this crate that runs the whole ladder.
//!
//! Every other module in `crates/execution` answers one question — what may this intent
//! be built into, who signs it, what did the node answer. This file is the only place
//! that *sequences* them, because §28's ladder and §55's completion criterion are facts
//! about a sequence: `RiskApproved → intent → builder → unsigned tx → signer → raw bytes
//! → submission → hash → receipt`, with each rung reached only from the one before it.
//!
//! Three shapes make the sequence checkable rather than remembered:
//!
//! * **Nothing is read that is not named** ([`Abilities`]): the price, the nonce, the
//!   canonical chain and the send are four traits, so the same ladder runs against the
//!   measured GIWA endpoint and against a scripted answer in a test — which is §40's
//!   permission to mock the *interface* and nothing else.
//! * **Every stop writes a reason** ([`StageReport`]): a run that stopped at `Built`
//!   because the mode may not sign is recorded as stopped-and-why, not as a failure, and
//!   a run that never became a record says so too (§24, §53, §55).
//! * **The lane and the record have owners** ([`Run`]): an attempt that never allocated a
//!   nonce must not release one, and bytes that reached a node are resolved by §25 rather
//!   than by hope.
//!
//! The key never passes through here as a value: [`Signer::from_env`] reads it once, at
//! construction, into a type that redacts itself (§17/§19). What this module can report is
//! the account it signs for, the bytes' hash, and what the chain said afterwards.

use std::sync::Arc;

use alloy_primitives::{Address, B256};

use evm_core::BlockNumber;
use evm_metrics::{Clock, Metrics};
use evm_simulation::SimulationResult;

use crate::arbitrage::{ExecutablePlan, ExecutionBinding};
use crate::builder::{BuildPolicy, GasPolicy, TransactionBuilder};
use crate::chain_read::{read_binding, ChainReader};
use crate::error::{ExecutionError, Result};
use crate::evidence::{SignedTransactionEvidence, SubmissionEvidence};
use crate::fee::{FeePolicy, FeeSource};
use crate::gate::{
    BalanceEvidence, Freshness, GateAttempt, GateFacts, GateOutcome, NonceEvidence, PlanBinding,
    PreSubmitGate,
};
use crate::giwa::GiwaSequencerDirect;
use crate::intent::{SenderFunding, TransactionIntent};
use crate::journal::{journal_stamp, ExecutionJournal, JournalFact, JournalRecord};
use crate::lifecycle::{
    execution_id_for, lane_held_unwritten, meter, Claim, ExecutionLane, ExecutionRecord,
    ExecutionStatus, LaneRelease, Ledger,
};
use crate::mode::ExecutionMode;
use crate::nonce::{NonceReading, NonceSource};
use crate::receipt::{
    ExpectedTransaction, ReceiptPolicy, ReceiptStatus, ReceiptTracker, TrackedReceipt,
};
use crate::signer::Signer;
use crate::submitter::{EndpointKind, SubmissionOutcome, TransactionSubmitter};

/// The four choices a run is born with, decided before it starts (§12, §13, §20, §26).
///
/// `mode` is here rather than at each call site because §20's point is that the ceiling
/// is set before anything exists that could want to raise it.
#[derive(Clone, Debug)]
pub struct ExecutionSetup {
    pub mode: ExecutionMode,
    pub fee: FeePolicy,
    pub build: BuildPolicy,
    pub receipts: ReceiptPolicy,
}

impl Default for ExecutionSetup {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::default(),
            // Two blocks of headroom over the pinned base fee, plus the node's own tip:
            // GIWA's blocks arrive about once a second, so the ceiling survives the time
            // it takes an intent to reach a node and does not pay more than the chain asks.
            fee: FeePolicy::BaseFeeHeadroom { headroom_blocks: 2 },
            build: BuildPolicy::default(),
            receipts: ReceiptPolicy::default(),
        }
    }
}

/// The endpoint surfaces the ladder reads, as one value.
///
/// Four traits rather than one concrete adapter because the ladder's pricing, nonce,
/// canonical-chain and send questions are four different contracts in §12, §11, §32 and
/// §21, and a test that can answer three of them must not have to implement the fourth.
#[derive(Clone)]
pub struct Abilities {
    pub submitter: Arc<dyn TransactionSubmitter + Send + Sync>,
    pub fees: Arc<dyn FeeSource + Send + Sync>,
    pub nonces: Arc<dyn NonceSource + Send + Sync>,
    pub chain: Arc<dyn ChainReader + Send + Sync>,
}

/// The four facts about an attempt that the stage cannot read for itself.
///
/// Grouped into one value because they answer one question — where did this attempt come
/// from — and because as four positional arguments they are two `&str`s and two enums that
/// a call site can silently transpose. This is also the whole of §44/§45's contract: the
/// stage is handed the pipeline's conclusion and its provenance, and reads nothing else
/// about pools, reserves or the opportunity itself.
#[derive(Clone, Debug)]
pub struct AttemptProvenance<'a> {
    /// Which M4 finding this attempt is for, verbatim — §37's log line and §52's evidence
    /// rows key on it.
    pub opportunity_id: &'a str,
    /// The state the simulation ran against, as the pipeline named it (§30's binding).
    pub state_fingerprint: &'a str,
    /// How the sender got the balance the simulation spent (§34's question).
    pub funding: SenderFunding,
    /// How current the market behind the finding still is (§16's freshness leg).
    pub freshness: Freshness,
}

/// What one attempt leaves behind: §37's log line, §52/§53's evidence rows, and the
/// record as it ended.
#[derive(Clone, Debug)]
pub struct StageReport {
    pub execution_id: Option<String>,
    pub opportunity_id: String,
    pub simulation_id: B256,
    pub risk_decision_id: B256,
    pub mode: ExecutionMode,
    /// The rung the record ended at. `None` only when the attempt never became a record —
    /// which is the case §45 requires be visible: a run the risk layer never accepted has
    /// no execution id to log.
    pub reached: Option<ExecutionStatus>,
    pub transaction_hash: Option<B256>,
    /// Whether bytes reached a node. §2.2's second boundary.
    ///
    /// Read off the submission row when there is one, because the ladder's rung and this
    /// fact come apart in §25's case: an `Unknown` answer means the bytes are possibly
    /// live while the record honestly stays at `Signed` — no rung was granted.
    pub sent: bool,
    /// Why the run stopped where it did, in the words of the check that stopped it.
    pub detail: String,
    /// The same stop as a typed fact, not only as prose.
    ///
    /// §40's six classes are decided from `sent`, the receipt and *which check* refused, so a
    /// caller outside this file that wants to say `SubmissionFailed` rather than `None` has to
    /// be handed the error itself. `detail` cannot carry that: the sentence a submission refusal
    /// produces and the sentence a plan refusal produces are both strings, and collapsing them
    /// into one word is exactly what §40 forbids. `None` means no check refused — the ladder ran
    /// out of rungs, or the run stopped at a boundary that is not a failure (§20's mode, §26's
    /// budget).
    pub stopped_with: Option<ExecutionError>,
    /// What the receipt question finally answered, as the tracker classified it — the fact §40's
    /// last three classes are decided from.
    ///
    /// This is a field rather than something a caller reads off `reached` because the two come
    /// apart twice: a run that reached `Submitted` and then ran out of receipt budget has a
    /// `Timeout` answer and no failure, and a run whose receipt could not be bound to a canonical
    /// block has an answer the tracker calls `NotFound` while §25 keeps it a refusal about our
    /// reading, not about the chain. The latter is deliberately left `None` here: §40's six do not
    /// name it, and `ExecutionClass::decide` would then say `None` rather than invent a class.
    pub receipt_answer: Option<ReceiptStatus>,
    pub lane: LaneRelease,
    /// Where each number came from (§51: an unread fact is a fact about the run).
    pub sources: Vec<String>,
    pub signed: Option<SignedTransactionEvidence>,
    pub submission: Option<SubmissionEvidence>,
    pub record: Option<ExecutionRecord>,
}

impl StageReport {
    fn open(intent: &TransactionIntent, mode: ExecutionMode) -> Self {
        Self {
            execution_id: None,
            opportunity_id: intent.ids.opportunity_id.clone(),
            simulation_id: intent.ids.simulation_id,
            risk_decision_id: intent.ids.risk_decision_id,
            mode,
            reached: None,
            transaction_hash: None,
            sent: false,
            detail: String::new(),
            stopped_with: None,
            receipt_answer: None,
            lane: LaneRelease::Released,
            sources: Vec::new(),
            signed: None,
            submission: None,
            record: None,
        }
    }

    /// A run that never produced an intent: no ids exist yet, so the line carries the
    /// opportunity it was about and the refusal.
    fn refused(opportunity_id: &str, detail: String) -> Self {
        Self {
            execution_id: None,
            opportunity_id: opportunity_id.to_string(),
            simulation_id: B256::ZERO,
            risk_decision_id: B256::ZERO,
            mode: ExecutionMode::BuildOnly,
            reached: None,
            transaction_hash: None,
            sent: false,
            detail,
            stopped_with: None,
            receipt_answer: None,
            lane: LaneRelease::Released,
            sources: Vec::new(),
            signed: None,
            submission: None,
            record: None,
        }
    }

    /// [`StageReport::refused`] plus the §40 class the refusal belongs to. M10's two early
    /// refusals (a plan for another chain, a route REVM saw revert) have no record, no id and
    /// no bytes, so without this the typed reason would exist only inside the sentence.
    fn refused_by_plan(opportunity_id: &str, detail: String) -> Self {
        let mut report = Self::refused(opportunity_id, detail);
        report.stopped_with = Some(ExecutionError::PlanRejected(report.detail.clone()));
        report
    }

    /// §37's log line: the four ids, the hash once one exists, and nothing else about the
    /// account. The private key is not a field of any type in this file, which is what
    /// makes "never in a log" a property of the code rather than of this format string.
    pub fn line(&self) -> String {
        let status = match self.reached {
            Some(status) => status.name(),
            None => "no-record",
        };
        let hash = match self.transaction_hash {
            Some(hash) => format!(" transaction_hash={hash:#x}"),
            None => String::new(),
        };
        format!(
            "execution_id={} opportunity_id={} simulation_id={:#x} risk_decision_id={:#x} \
             status={status}{hash} — {}",
            self.execution_id.as_deref().unwrap_or("-"),
            self.opportunity_id,
            self.simulation_id,
            self.risk_decision_id,
            self.detail
        )
    }

    /// The `executions.jsonl` row.
    ///
    /// The §29 lifecycle record travels inside it, under `lifecycle`, because the
    /// attempt and its record are one answer — *how far did this get, and what did
    /// the chain say when it got there* — and splitting them across two files would
    /// make a reader join them by hand to learn that a `submitted` row had a
    /// receipt that never arrived.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "execution_id": self.execution_id,
            "opportunity_id": self.opportunity_id,
            "simulation_id": format!("{:#x}", self.simulation_id),
            "risk_decision_id": format!("{:#x}", self.risk_decision_id),
            "mode": self.mode.name(),
            "status": match self.reached {
                Some(status) => status.name(),
                None => "no-record",
            },
            "transaction_hash": self.transaction_hash.map(|hash| format!("{hash:#x}")),
            "sent": self.sent,
            "lane": match &self.lane {
                LaneRelease::Released => serde_json::Value::String("released".to_string()),
                LaneRelease::Held { reason } => serde_json::json!({ "held": reason }),
            },
            "sources": self.sources,
            "detail": self.detail,
            "stopped_with": self
                .stopped_with
                .as_ref()
                .map(|error| error.to_string()),
            "receipt_answer": self.receipt_answer.map(|status| status.name().to_string()),
            "lifecycle": self
                .record
                .as_ref()
                .map(|record| serde_json::to_value(record).expect("a record serializes")),
        })
    }
}

/// How an attempt ended: the ladder either ran out of rungs to climb, or stopped.
enum Halt {
    /// Something refused. The record ends `Failed` with §39's words.
    Failed(ExecutionError),
    /// Nothing failed: the mode, the endpoint or the receipt budget decided the run goes
    /// no further. The record keeps its rung and gains a `blocked_reason` (§24).
    Blocked(String),
}

impl From<ExecutionError> for Halt {
    fn from(error: ExecutionError) -> Self {
        Self::Failed(error)
    }
}

/// One attempt: the shape it was given, the progress it has made, and the data the stop
/// and close paths need.
struct Run {
    execution_id: Option<String>,
    report: StageReport,
    /// Which of §35's two paths this is, and the gas rule that follows from it (§13).
    attempt: GateAttempt,
    gas: GasPolicy,
    /// The measurement §13's preferred gas policy resolves against — absent for a
    /// transaction that was never simulated.
    simulated_gas_used: Option<u64>,
    /// The read the lane was allocated from, kept so §32's nonce leg quotes the same
    /// answer instead of a remembered one.
    nonce_reading: Option<NonceReading>,
    /// This attempt took the lane. An attempt that did not must not let it go: with one
    /// lane, a refused build releasing someone else's nonce is §11's failure, not a
    /// cleanup.
    owns_lane: bool,
    /// Bytes went to a node, so §25 — not §11 — decides whether the lane may be released.
    sent: bool,
    /// M12-F §9: at least one line about this attempt is already on disk, so the lane this
    /// attempt holds is now a fact a restarted process will rebuild. Releasing it here would
    /// fork memory against the file — the run would say "the nonce is free" while the ledger
    /// says "possibly in flight" — which is why a durable lane outranks `sent` in the release
    /// test, and why an attempt that persisted its intent and then lost the disk is the one
    /// outcome §4.1 wants it to have.
    lane_is_durable: bool,
}

/// §55's ladder as one object with one entry per §35 path.
pub struct ExecutionStage {
    abilities: Abilities,
    signer: Signer,
    setup: ExecutionSetup,
    lane: ExecutionLane,
    ledger: Ledger,
    journal: ExecutionJournal,
    clock: Clock,
    configured_chain_id: u64,
}

impl ExecutionStage {
    /// §6: connect to the configured endpoint and prove it answers for the expected chain
    /// before any intent can reach it.
    ///
    /// The same adapter is cloned into all four ability slots: the one connection answers
    /// the price, the nonce, the canonical chain and the send, and §23 measured that
    /// `eth_sendRawTransaction` is whitelisted at the URL this run was configured with.
    ///
    /// The endpoint *class* is `Unknown` and is not derived from the URL (M12-D §5). What
    /// this process can prove about its send is that it posted to the configured socket;
    /// who runs that socket, and where it forwards the bytes, are node-side facts it never
    /// reads, so claiming `public_http_rpc` here was a guess wearing an operator's clothes.
    /// [`crate::giwa::submission_provenance`] names the socket by digest instead.
    pub async fn connect(
        url: &str,
        expected_chain_id: u64,
        setup: ExecutionSetup,
        clock: Clock,
        journal: ExecutionJournal,
    ) -> Result<Self> {
        let adapter = Arc::new(
            GiwaSequencerDirect::connect(url, expected_chain_id, setup.mode, EndpointKind::Unknown)
                .await?,
        );
        let abilities = Abilities {
            submitter: adapter.clone(),
            fees: adapter.clone(),
            nonces: adapter.clone(),
            chain: adapter,
        };
        // §19: this is the only key read in the milestone, it happens once, and in
        // `BuildOnly` it reads nothing at all even when the variable is set.
        let signer = Signer::from_env(setup.mode)?;
        Self::new(abilities, signer, setup, expected_chain_id, clock, journal)
    }

    /// The stage over any endpoint that answers the four traits — the live adapter above,
    /// or a scripted one in a test (§40).
    ///
    /// `journal` is a required argument rather than something this function reaches for, and
    /// that is the whole of M12-F §9's concurrency answer: one append-only file can only be
    /// written by one handle, because two handles each keep their own sequence numbers and the
    /// next reader refuses their collision as `duplicate_sequence`. So the entry point opens the
    /// ledger once — which is also where §7's recovery runs, before a stage exists to sign or
    /// send anything — and hands the one handle down.
    pub fn new(
        abilities: Abilities,
        signer: Signer,
        mut setup: ExecutionSetup,
        configured_chain_id: u64,
        clock: Clock,
        journal: ExecutionJournal,
    ) -> Result<Self> {
        if signer.mode() != setup.mode {
            return Err(ExecutionError::ModeGate(format!(
                "the stage is in {} and was handed a signer in {}; one run has one mode (§20), \
                 and a stage that could sign in a mode that says it may not is the failure §19 \
                 exists to prevent",
                setup.mode.name(),
                signer.mode().name()
            )));
        }
        if setup.build.expected_chain_id == 0 {
            setup.build.expected_chain_id = configured_chain_id;
        } else if setup.build.expected_chain_id != configured_chain_id {
            return Err(ExecutionError::ChainMismatch(format!(
                "the build policy expects chain {} and this stage is configured for chain {}",
                setup.build.expected_chain_id, configured_chain_id
            )));
        }
        // §7's steps 1–4 happen here, at construction, because a stage that exists is a stage a
        // caller can send with. The journal handle this comes from already read and validated
        // every line — recovery that failed refused to open, and an open that succeeded says
        // what it found.
        let mut lane = ExecutionLane::new();
        for (address, nonce, execution_id) in journal.restored_lane() {
            lane.restore(*address, *nonce).map_err(|error| {
                crate::error::ExecutionError::NonceUnavailable(format!(
                    "{error}; the ledger also names {execution_id} as holding nonce {nonce}, so \
                     one lane was being claimed twice"
                ))
            })?;
        }
        Ok(Self {
            abilities,
            signer,
            setup,
            lane,
            ledger: Ledger::new(),
            journal,
            clock,
            configured_chain_id,
        })
    }

    pub fn mode(&self) -> ExecutionMode {
        self.setup.mode
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The durable store this run writes into before it touches a socket (§4.1). Read-only
    /// through here: what it recovered is the entry point's business, and what it writes next is
    /// this stage's.
    pub fn journal(&self) -> &ExecutionJournal {
        &self.journal
    }

    pub fn lane_is_idle(&self) -> bool {
        self.lane.is_idle()
    }

    /// The account the stage's signer signs for, or the mode's reason it has none (§19's
    /// answer to "which wallet is this run allowed to move").
    pub fn signer_address(&self) -> Result<Address> {
        self.signer.address()
    }

    /// §44/§45's consumption point: M4's run and M5's decision in, the ladder out. The
    /// stage re-reads nothing about pools or reserves — only the price, the nonce and the
    /// canonical chain, which are facts about the moment of sending and not about the trade.
    pub async fn on_risk_decision(
        &mut self,
        run: &SimulationResult,
        decision: &evm_risk::RiskDecision,
        provenance: AttemptProvenance<'_>,
        metrics: &mut Metrics,
    ) -> StageReport {
        let intent = match TransactionIntent::from_run(
            run,
            decision,
            provenance.opportunity_id,
            provenance.state_fingerprint,
            provenance.funding,
        ) {
            Ok(intent) => intent,
            // §45's boundary, and acceptance K: a decision that is not Accept, a run that
            // did not complete or a multi-step sequence produce no intent, so nothing here
            // is built, priced, claimed or sent. The refusal is the whole output.
            Err(error) => {
                metrics.bump("execution_refused_before_claim");
                return StageReport::refused(
                    provenance.opportunity_id,
                    format!("no intent was formed, so nothing was built: {error}"),
                );
            }
        };
        let attempt = GateAttempt::Arbitrage {
            simulation_success: run.success(),
            risk_accepted: decision.accepted(),
            freshness: provenance.freshness,
        };
        self.drive(intent, attempt, Some(run.gas_used()), metrics)
            .await
    }

    /// §35's controlled validation transaction — the one path this milestone can honestly
    /// complete, and the only reason the gate has a second shape.
    ///
    /// `label` is what §42 requires the transaction be called, and the gate refuses an
    /// empty one: an unlabelled transaction would sit in the evidence looking exactly like
    /// an arbitrage that skipped the risk layer.
    pub async fn on_validation(
        &mut self,
        intent: TransactionIntent,
        label: &str,
        metrics: &mut Metrics,
    ) -> StageReport {
        let attempt = GateAttempt::Validation {
            label: label.to_string(),
        };
        self.drive(intent, attempt, None, metrics).await
    }

    /// §24/§56's consumption point for M10: a decided plan in, the one ladder out. Nothing here
    /// is a new stage — the intent, the builder, the signer, the submitter, the receipt tracker
    /// and the record are the same types `on_risk_decision` runs, which is what §24's ban on an
    /// `ArbitrageSigner` / `ArbitrageSubmitter` / `ArbitrageReceipt` asks for.
    ///
    /// Both refusals happen before `drive`, so before any read is paid for, because both are
    /// answers about the *plan* rather than about the moment of sending: §7's chain binding is
    /// settled by the configuration, and §25's simulation outcome is already in the plan. Naming
    /// a route that REVM saw revert as a build failure would collapse §40's classes into each
    /// other, which is the one thing this entry point exists to keep apart.
    pub async fn on_arbitrage_plan(
        &mut self,
        plan: &ExecutablePlan,
        binding: &ExecutionBinding,
        freshness: Freshness,
        metrics: &mut Metrics,
    ) -> StageReport {
        let correlation_id = plan.plan().simulation.correlation_id.clone();
        if binding.chain_id != self.configured_chain_id {
            metrics.bump("execution_refused_before_claim");
            return StageReport::refused_by_plan(
                &correlation_id,
                format!(
                    "§7: this stage is configured for chain {} and the plan was handed a \
                     binding for chain {}; the endpoint's own answer is not allowed to be the \
                     tie-breaker, so nothing was built",
                    self.configured_chain_id, binding.chain_id
                ),
            );
        }
        let outcome = &plan.plan().simulation.outcome;
        if !outcome.succeeded() {
            metrics.bump("execution_refused_before_claim");
            let revert = match outcome.revert() {
                Some(revert) => format!(", classified {revert}"),
                None => String::new(),
            };
            return StageReport::refused_by_plan(
                &correlation_id,
                format!(
                    "§25/§40: REVM's answer for this route was {}{revert}, so nothing was \
                     built, priced, claimed or sent",
                    outcome.name()
                ),
            );
        }
        let intent = plan.to_intent();
        let attempt = GateAttempt::Executor {
            plan: plan.binding_evidence(binding),
            // Re-derived from the plan rather than written `true`, so the gate's leg is the
            // plan's own fact and not this function's conclusion.
            simulation_success: outcome.succeeded(),
            freshness,
        };
        // §13's gas policy adds its margin to *this* number, so which of the two figures a
        // simulation reports decides the limit that goes on the wire. It is the limit the run
        // completed at, not the gas it consumed: under EIP-150 a nested frame receives at most
        // 63/64 of what its caller has left, so a transaction limited to the burn can starve the
        // deepest call — which is what §57's first two real attempts did at a 229,302 burn and a
        // 249,302 limit, while the identical call succeeds with headroom.
        self.drive(intent, attempt, outcome.proved_gas_limit(), metrics)
            .await
    }

    async fn drive(
        &mut self,
        mut intent: TransactionIntent,
        attempt: GateAttempt,
        simulated_gas_used: Option<u64>,
        metrics: &mut Metrics,
    ) -> StageReport {
        metrics.bump("execution_attempt");

        // §13's rule is exactly this split: a simulated transaction's gas limit must be
        // traceable to a measurement, and a transaction that was never simulated has no
        // measurement to be traceable to, so it carries the configured limit instead. Choosing
        // it here rather than accepting it from a caller is what stops a configured limit
        // reaching an arbitrage intent. M10's executor transaction belongs on the measured side
        // — REVM ran the whole route before these bytes existed (§25).
        let gas = if attempt.has_simulation() {
            self.setup.build.gas
        } else {
            GasPolicy::Configured {
                gas_limit: intent.gas_limit,
            }
        };
        let mut run = Run {
            execution_id: None,
            report: StageReport::open(&intent, self.setup.mode),
            attempt,
            gas,
            simulated_gas_used,
            nonce_reading: None,
            owns_lane: false,
            sent: false,
            lane_is_durable: false,
        };
        // The price and the nonce are read *before* the record is claimed, because the
        // record is the evidence of the transaction that was built: claiming first and
        // pricing after would leave every record carrying the nonce the simulation
        // assumed instead of the one the chain handed over.
        if let Err(error) = self.price(&mut intent, &mut run).await {
            if matches!(error, ExecutionError::NonceUnavailable(_)) {
                self.note_restored_hold(&intent, &mut run);
            }
            self.stop(&mut run, &error, metrics);
            self.close(&mut run);
            return run.report;
        }
        // §7's step 3, one rung ahead of the ledger's own §30 answer: the file may hold this
        // triple from a process that is gone, and the in-memory ledger has never seen it. Lines
        // *this* run wrote are deliberately out of scope — the ledger is the authority for those,
        // and §9 wants one store to answer each question rather than two to answer it differently.
        let idempotency_key = intent.ids.idempotency_key(&intent.state_binding());
        let derived_id = execution_id_for(&idempotency_key);
        let restored = self.journal.restored_executed(&derived_id).map(|record| {
            (
                record.execution_id.clone(),
                record.state.name(),
                record.last_fact,
                record.nonce,
            )
        });
        if let Some((execution_id, state, last_fact, nonce)) = restored {
            metrics.bump("execution_duplicate_in_ledger");
            self.hand_lane_back(&mut run);
            run.report.detail = format!(
                "M12-F §7: {execution_id} is already durable in the execution ledger (state \
                 {state}, last fact {}, nonce {}), so this attempt stopped before \
                 building a second transaction for the same state binding",
                last_fact.name(),
                nonce
                    .map(|nonce| nonce.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            run.execution_id = Some(execution_id.clone());
            run.report.execution_id = Some(execution_id);
            self.close(&mut run);
            return run.report;
        }
        let at = self.clock.now_ms();
        let id = match self.ledger.claim(&intent, ExecutionStatus::Detected, at) {
            Claim::New(handle) => handle.execution_id,
            Claim::Existing(existing) => {
                // §30: this (opportunity, simulation, state) triple already owns a record,
                // so a second transaction on it is not available — that is the whole of the
                // duplicate's handling, and it happens before a build exists. The lane this
                // attempt allocated and never used goes back.
                metrics.bump("execution_duplicate");
                self.hand_lane_back(&mut run);
                let status = existing.status.name();
                run.report.detail = format!(
                    "§30: {} already owns this state binding (it is at {}), so this attempt \
                     stopped before building a second transaction for it",
                    existing.execution_id, status
                );
                run.execution_id = Some(existing.execution_id.clone());
                self.close(&mut run);
                return run.report;
            }
        };
        run.execution_id = Some(id.clone());

        match self.ladder(&id, &intent, &mut run, metrics).await {
            Ok(()) => {}
            Err(Halt::Failed(error)) => self.stop(&mut run, &error, metrics),
            Err(Halt::Blocked(reason)) => self.block(&mut run, &reason, metrics),
        }
        self.close(&mut run);
        run.report
    }

    /// §12's price and §11's nonce, read against the pinned block and the account.
    async fn price(&mut self, intent: &mut TransactionIntent, run: &mut Run) -> Result<()> {
        // §17: the key signs for one account, and that account is the intent's sender.
        // Checked before any read is paid for, so a stage configured for one wallet cannot
        // produce a priced intent for another.
        if self.signer.mode().may_read_key() {
            let configured = self.signer.address()?;
            if configured != intent.sender {
                return Err(ExecutionError::SigningFailed(format!(
                    "the configured signer ({}) is {configured} and this intent's sender is \
                     {}; §17's signer never signs for another account",
                    self.signer.source(),
                    intent.sender
                )));
            }
        }
        let fees = self.abilities.fees.clone();
        let reading = fees
            .fee_reading(
                intent.block_number.0,
                intent.block_hash,
                intent.tx_type,
                &self.setup.fee,
            )
            .await?;
        let (max_fee_per_gas, max_priority_fee_per_gas) = reading.fields_for(intent.tx_type)?;
        intent.max_fee_per_gas = max_fee_per_gas;
        intent.max_priority_fee_per_gas = max_priority_fee_per_gas;
        run.report.sources.push(format!(
            "fee: {} (max_fee_per_gas={}, tx type {})",
            reading.provenance,
            reading.max_fee_per_gas,
            intent.tx_type.name()
        ));

        let nonces = self.abilities.nonces.clone();
        let reading = nonces.nonce(intent.sender).await?;
        let nonce = self.lane.allocate(&reading)?;
        run.owns_lane = true;
        run.report
            .sources
            .push(format!("nonce: {}", reading.source));
        intent.nonce = nonce;
        run.nonce_reading = Some(reading);
        Ok(())
    }

    /// The rungs, in §55's order. Every `?` stops the ladder and every stop is reported
    /// with the reason the crate's own type gave.
    async fn ladder(
        &mut self,
        id: &str,
        intent: &TransactionIntent,
        run: &mut Run,
        metrics: &mut Metrics,
    ) -> std::result::Result<(), Halt> {
        // The rungs the attempt already proves, and the two predicates split deliberately:
        // `Simulated` is a fact about whether an EVM ran (§25 covers M10's executor route too),
        // `RiskApproved` is a fact about whether a risk layer judged the run — and only an
        // arbitrage can claim that. Inventing either one for an attempt that does not have it
        // would be §36's fake M7 written into the lifecycle.
        if run.attempt.has_simulation() {
            self.ledger
                .advance(id, ExecutionStatus::Simulated, self.clock.now_ms())?;
        }
        if run.attempt.is_arbitrage() {
            self.ledger
                .advance(id, ExecutionStatus::RiskApproved, self.clock.now_ms())?;
        }

        // §38's identity travels with the plan leg that validated, so a record from any other
        // path keeps its `route_id: None` rather than borrowing a word it did not earn.
        if let GateAttempt::Executor {
            plan: PlanBinding::Valid { route_id, .. },
            ..
        } = &run.attempt
        {
            self.ledger.attach_route_id(id, route_id)?;
        }

        let mut policy = self.setup.build.clone();
        policy.gas = run.gas;
        policy.simulated_gas_used = run.simulated_gas_used;
        let build = TransactionBuilder::build(intent, &policy)?;
        let previous = self
            .ledger
            .advance(id, ExecutionStatus::Built, self.clock.now_ms())?;
        self.meter_rung(id, Some(previous), metrics);

        // §32's chain legs, read now rather than carried from an earlier block.
        let chain = self.abilities.chain.clone();
        // A chain id this code cannot read is a stop, not an `Unverified` leg: §32's gate
        // compares three numbers and has no variant for "the node never answered", so
        // letting one through would mean inventing the missing figure.
        let endpoint_chain_id = chain.endpoint_chain_id().await?;
        let binding = read_binding(&*chain, intent.block_number, intent.block_hash).await;

        let fees = self.abilities.fees.clone();
        let available = fees.balance(intent.sender, intent.block_number.0).await?;
        let maximum_spend_wei = build.unsigned.maximum_cost_wei()?;
        let source = format!(
            "native balance of {} read at pinned block {} (not \"latest\"), against this \
             build's maximum spend of gas_limit × max_fee + value — the L2 half of the bill: \
             an OP-stack endpoint also charges an L1 data fee on top of it, which this ceiling \
             does not carry",
            intent.sender, intent.block_number.0
        );
        run.report.sources.push(source.clone());
        let balance = if available >= maximum_spend_wei {
            BalanceEvidence::Sufficient {
                available_wei: available,
                maximum_spend_wei,
                source,
            }
        } else {
            BalanceEvidence::Insufficient {
                available_wei: available,
                maximum_spend_wei,
                source,
            }
        };

        let nonce = match (&run.nonce_reading, self.lane.outstanding()) {
            (Some(reading), Some((lane_sender, lane_nonce)))
                if reading.pending == intent.nonce
                    && lane_sender == intent.sender
                    && lane_nonce == intent.nonce =>
            {
                NonceEvidence::Matches {
                    nonce: intent.nonce,
                    source: reading.source.clone(),
                }
            }
            (Some(reading), _) => NonceEvidence::Differs {
                intent_nonce: intent.nonce,
                pending_nonce: reading.pending,
                source: reading.source.clone(),
            },
            (None, _) => NonceEvidence::Unverified("this attempt never read the nonce".to_string()),
        };

        let facts = GateFacts {
            attempt: run.attempt.clone(),
            intent_chain_id: intent.chain_id,
            configured_chain_id: self.configured_chain_id,
            endpoint_chain_id,
            binding,
            balance,
            nonce,
        };
        let outcome = PreSubmitGate::evaluate(&facts);
        if !outcome.passed() {
            metrics.bump("execution_gate_blocked");
            run.report.detail = outcome.describe();
            return Err(Halt::Failed(gate_error(&outcome)));
        }

        // §20: `BuildOnly` stops on its own terms at this rung. Reaching the signer here
        // anyway and recording the mode's refusal as a failure would report a run that did
        // exactly what it was allowed to do as a broken one.
        if !self.signer.mode().may_read_key() {
            let reason = format!(
                "{} stops at Built: no key was read and no bytes exist (§19, §20)",
                self.signer.mode().name()
            );
            self.journal_ready_not_sent(intent, &reason)
                .map_err(Halt::Failed)?;
            return Err(Halt::Blocked(reason));
        }

        let (signed, recovered) = self.signer.sign_and_recover(&build.unsigned)?;
        let evidence =
            SignedTransactionEvidence::from_build(&build, &signed, recovered).map_err(|why| {
                ExecutionError::BuildFailed(format!(
                    "the signature does not cover this build, so it cannot be recorded \
                     against it: {why}"
                ))
            })?;
        if !evidence.sender_matches_expectation() {
            return Err(Halt::Failed(ExecutionError::SigningFailed(format!(
                "the signature recovers {} and the build expected {}",
                evidence.recovered_sender, evidence.expected_sender
            ))));
        }
        let local_hash = signed.hash();
        self.ledger.attach_transaction_hash(id, local_hash)?;
        let previous = self
            .ledger
            .advance(id, ExecutionStatus::Signed, self.clock.now_ms())?;
        self.meter_rung(id, Some(previous), metrics);
        run.report.signed = Some(evidence);

        // §15: the bytes decoded back must be the transaction that went in. This runs
        // before the send because a codec that drifts is a reason not to send, and after
        // the sign because there is nothing to decode before it.
        TransactionBuilder::round_trip(&build, &signed)?;

        if !self.abilities.submitter.may_submit() {
            let reason = format!(
                "{} over a {} endpoint: the signed bytes exist and were never handed to a \
                 node (§20/§24)",
                self.signer.mode().name(),
                self.abilities.submitter.endpoint().name()
            );
            let at = self.clock.now_ms();
            run.report.submission = Some(SubmissionEvidence::blocked(
                Some(local_hash),
                self.abilities.submitter.endpoint(),
                reason.clone(),
                at,
            ));
            self.journal_ready_not_sent(intent, &reason)
                .map_err(Halt::Failed)?;
            return Err(Halt::Blocked(reason));
        }

        // §4.1's boundary, and it is a call rather than a comment: both the intent line and the
        // dispatch line are on disk before this process touches the socket, and an `Err` here
        // returns before `submit` is reached.
        let intent_line = self
            .persist_send_intent(intent, local_hash, run)
            .map_err(Halt::Failed)?;
        let submitter = self.abilities.submitter.clone();
        let outcome = submitter.submit(&signed).await?;
        metrics.bump(&format!("execution_submission_{}", outcome.status_word()));
        // From this line §25 owns the lane: whatever the answer was, bytes went to a node,
        // so only a definite refusal or a terminal receipt may let the nonce go.
        run.sent = true;
        let mut submission =
            SubmissionEvidence::from_outcome(local_hash, &outcome, self.clock.now_ms());
        run.report.submission = Some(submission.clone());
        // §9's ordering, and it is the same rule §4.1 states for the send: the fact goes to disk
        // before this process acts on it in memory. Appending first means a ledger that will not
        // take the answer leaves the nonce held — which is what the file will still say about a
        // `send_dispatched` line after a restart. Releasing before the append would open exactly
        // the window §9 forbids: this process freeing a nonce the ledger reserves.
        self.journal
            .append(
                journal_stamp(),
                JournalRecord::from_outcome(
                    &intent_line,
                    &outcome,
                    self.abilities.submitter.endpoint(),
                ),
            )
            .map_err(|error| {
                run.report.lane = lane_held_unwritten(&error);
                Halt::Failed(error)
            })?;
        run.report.lane = self.lane.resolve_submission(&outcome);
        match outcome {
            SubmissionOutcome::Rejected { reason, .. } => {
                // The one answer §25 lets be a failure: the node said no, the transaction
                // is not in flight, and the lane is already released above.
                return Err(Halt::Failed(ExecutionError::SubmissionRejected(reason)));
            }
            SubmissionOutcome::Unknown { reason, .. } => {
                return Err(Halt::Blocked(format!(
                    "§25: the node's answer leaves nonce {} possibly in flight, so nothing is \
                     resent and the lane stays held: {reason}",
                    intent.nonce
                )));
            }
            SubmissionOutcome::Accepted { .. } => {}
        }
        let previous = self
            .ledger
            .advance(id, ExecutionStatus::Submitted, self.clock.now_ms())?;
        self.meter_rung(id, Some(previous), metrics);

        let expected = ExpectedTransaction {
            transaction_hash: local_hash,
            sender: recovered,
            target: Some(intent.target),
            nonce: intent.nonce,
            chain_id: intent.chain_id,
        };
        let receipt_submitter = self.abilities.submitter.clone();
        let receipt_chain = self.abilities.chain.clone();
        let tracked = ReceiptTracker::new(self.setup.receipts)
            .track(
                &expected,
                move || {
                    let submitter = receipt_submitter.clone();
                    async move {
                        submitter
                            .receipt(local_hash)
                            .await
                            .map_err(|error| error.to_string())
                    }
                },
                move |number| {
                    let chain = receipt_chain.clone();
                    async move {
                        chain
                            .block_hash_at(BlockNumber(number))
                            .await
                            .map_err(|error| error.to_string())
                    }
                },
            )
            .await;
        // §40's receipt-side classes are decided from the tracker's own answer, not from the rung
        // the record ended at. `Unbound` is left unrecorded on purpose: §25 keeps it a refusal
        // about our reading of a receipt, and a class for it would be §40's collapse.
        run.report.receipt_answer = match &tracked {
            TrackedReceipt::Unbound { .. } => None,
            _ => Some(tracked.status()),
        };
        match tracked {
            TrackedReceipt::Included(receipt) | TrackedReceipt::Reverted(receipt) => {
                let outcome_was = receipt.outcome();
                match self.ledger.attach_receipt(&receipt, self.clock.now_ms())? {
                    Some(owner) => {
                        // The rung the receipt moves the record off is `Submitted`; §38's
                        // receipt latency is only measurable from the stamp that rung got.
                        self.meter_rung(&owner, Some(ExecutionStatus::Submitted), metrics);
                        submission = submission.with_receipt(&receipt);
                        run.report.submission = Some(submission);
                        // Durable before remembered, for the reason given at the submission line
                        // above: a receipt the ledger would not take must not have freed this
                        // nonce, because the file still holds the transaction unconfirmed and a
                        // restart would recover a hold this process had already dropped.
                        self.journal
                            .append(
                                journal_stamp(),
                                JournalRecord::from_intent(intent, JournalFact::ReceiptObserved)
                                    .with_hash(Some(local_hash))
                                    .with_status(outcome_was.name())
                                    .with_detail(format!(
                                    "receipt read in block {} — the chain's own answer, and the \
                                     only fact that closes the nonce §4.3 held open",
                                    receipt.block_number
                                )),
                            )
                            .map_err(|error| {
                                run.report.lane = lane_held_unwritten(&error);
                                Halt::Failed(error)
                            })?;
                        run.report.lane = self.lane.resolve_receipt(outcome_was);
                        // §2.4: included is not profitable. The receipt says what the chain
                        // charged and nothing here turns that into a realised gain — that
                        // comparison is M7's (§56).
                        run.report.detail = if outcome_was == ReceiptStatus::Reverted {
                            // §P: the chain executed it and the call failed. Recorded as
                            // `Reverted`, never as a success and never as a profit.
                            format!(
                                "reverted in block {} — the chain executed the transaction and \
                                 the call failed; nothing about this run is profit",
                                receipt.block_number
                            )
                        } else {
                            format!(
                                "included in block {} (gas_used={}, effective_gas_price={}, \
                                 l1_fee={:?})",
                                receipt.block_number,
                                receipt.gas_used,
                                receipt.effective_gas_price,
                                receipt.l1_fee
                            )
                        };
                    }
                    None => {
                        return Err(Halt::Failed(ExecutionError::ReceiptBinding(format!(
                            "the ledger holds no record for {local_hash:#x}, so the receipt of \
                             transaction {} could not be attached to anything",
                            receipt.transaction_hash
                        ))));
                    }
                }
            }
            TrackedReceipt::Pending {
                attempts,
                last_answer,
            } => {
                return Err(Halt::Blocked(format!(
                    "§26: no receipt after {attempts} attempts ({}); the transaction may still \
                     land, so it is not a failure and nothing is resent",
                    last_answer
                )));
            }
            TrackedReceipt::Unbound { receipt, reason } => {
                return Err(Halt::Failed(ExecutionError::ReceiptBinding(format!(
                    "{reason}; the receipt in block {} is not evidence about a canonical block \
                     and the record was not moved",
                    receipt.block_number
                ))));
            }
        }
        Ok(())
    }

    /// A refusal: the record ends `Failed` with §39's reason, and a lane this attempt
    /// allocated and never sent with goes back (§11).
    fn stop(&mut self, run: &mut Run, error: &ExecutionError, metrics: &mut Metrics) {
        metrics.bump("execution_stopped");
        let at = self.clock.now_ms();
        run.report.stopped_with = Some(error.clone());
        if run.report.detail.is_empty() {
            run.report.detail = error.to_string();
        } else {
            run.report.detail = format!("{} ({error})", run.report.detail);
        }
        let id = run.execution_id.clone();
        if let Some(id) = id {
            match self.ledger.fail(&id, error, at) {
                Ok(previous) => self.meter_rung(&id, Some(previous), metrics),
                Err(record_error) => {
                    run.report.detail = format!(
                        "{} (and the record refused to close: {record_error})",
                        run.report.detail
                    );
                }
            }
        }
        self.hand_lane_back(run);
    }

    /// A stop that is not a failure (§24's endpoint, §20's mode, §26's budget): the record
    /// keeps the rung it reached and gains the reason it went no further.
    fn block(&mut self, run: &mut Run, reason: &str, metrics: &mut Metrics) {
        metrics.bump("execution_blocked");
        if run.report.detail.is_empty() {
            run.report.detail = reason.to_string();
        }
        let id = run.execution_id.clone();
        if let Some(id) = id {
            if let Err(record_error) = self.ledger.block(&id, reason) {
                run.report.detail = format!(
                    "{} (and the record refused the blocked reason: {record_error})",
                    run.report.detail
                );
            }
        }
        self.hand_lane_back(run);
    }

    /// §4.2's fact 1 at the moment it becomes one: bytes or an intent exist and this run is not
    /// allowed to send them. The line is written where the refusal happens rather than at the end
    /// of the run because after a restart "the mode refused" is a fact about the file, and a
    /// record of it that a crash can lose is a record of nothing.
    ///
    /// A refusal here is what §4.1 asks for: the run stops with an explicit local error instead
    /// of carrying on as though the ledger had agreed. Nothing was sent either way, so the
    /// failure cannot make a duplicate transaction — but it does make an unauditable one, and
    /// that is the difference this milestone exists to keep.
    fn journal_ready_not_sent(
        &mut self,
        intent: &TransactionIntent,
        reason: &str,
    ) -> std::result::Result<(), ExecutionError> {
        let at = journal_stamp();
        let record = JournalRecord::from_intent(intent, JournalFact::ReadyNotSent)
            .with_endpoint(self.abilities.submitter.endpoint())
            .with_detail(reason.to_string());
        self.journal.append(at, record).map(|_| ())
    }

    /// §4.1's boundary as one call: the intent line and the dispatch line, both durable, both
    /// written before this process touches the socket. The two lines are one call because §8's
    /// F3 and F4 are crashes on either side of the POST, and the pair is what lets recovery tell
    /// "signed, never dispatched" from "dispatched, answer lost" — the first is a fact about this
    /// process, the second is §4.3's `Unknown` and is not resolvable from disk.
    ///
    /// Returns the record the node's answer and the receipt are stamped from, so no caller can
    /// write either of those with an identity that differs from the intent line. An `Err` means
    /// at least one line is missing and `submit` must not be reached; `lane_is_durable` is set as
    /// soon as the first line lands, because from that moment §9 forbids freeing the nonce even
    /// though nothing was dispatched.
    fn persist_send_intent(
        &mut self,
        intent: &TransactionIntent,
        local_hash: B256,
        run: &mut Run,
    ) -> std::result::Result<JournalRecord, ExecutionError> {
        let at = journal_stamp();
        let intent_line = JournalRecord::from_intent(intent, JournalFact::SendIntentPersisted)
            .with_hash(Some(local_hash))
            .with_endpoint(self.abilities.submitter.endpoint())
            .with_detail(format!(
            "signed bytes exist with local hash {local_hash:#x} and this line is on disk before \
             the request is dispatched (§4.1)"
        ));
        self.journal.append(at, intent_line.clone())?;
        run.lane_is_durable = true;
        let dispatch_line = JournalRecord::from_intent(intent, JournalFact::SendDispatched)
            .with_hash(Some(local_hash))
            .with_endpoint(self.abilities.submitter.endpoint())
            .with_detail(
                "the request went to the configured socket and the answer is not known yet; \
                 everything between this line and the next is what §25 forbids resending"
                    .to_string(),
            );
        self.journal.append(at, dispatch_line)?;
        Ok(intent_line)
    }

    /// §4.2's fact 8, written at the one moment the need stops being an inference: a fresh
    /// attempt could not allocate its nonce because a record from a process that is gone still
    /// reserves it. The line names that record, so "a human must look at this" survives the
    /// process that noticed it.
    ///
    /// A failure here is diagnostic only and does not change the run's stop — the run was
    /// already refusing to send, and a ledger that cannot say why must not turn a refusal into
    /// a different answer.
    fn note_restored_hold(&mut self, intent: &TransactionIntent, run: &mut Run) {
        let Some((address, nonce, execution_id)) = self
            .journal
            .restored_lane()
            .iter()
            .find(|(holder, _, _)| *holder == intent.sender)
            .cloned()
        else {
            return;
        };
        let Some(record) = self.journal.recovery().by_execution_id(&execution_id) else {
            return;
        };
        let line = JournalRecord::from_recovered(
            record,
            JournalFact::AttentionRequired,
            format!(
                "a new attempt for {} could not allocate nonce {nonce} at {address} because \
                 this record, written by a process that is gone, still reserves it; §4.3 keeps \
                 it unresolved and §11 forbids releasing it to let the new attempt through",
                intent.ids.opportunity_id
            ),
        );
        let at = journal_stamp();
        match self.journal.append(at, line) {
            Ok(outcome) => run.report.sources.push(format!(
                "ledger: attention_required written for {execution_id} holding nonce {nonce} ({})",
                outcome.name()
            )),
            Err(error) => run
                .report
                .sources
                .push(format!("ledger: attention_required refused: {error}")),
        }
    }

    /// §11's rule for a lane this attempt allocated and never sent with, plus §9's M12-F
    /// exception: once a line about this attempt is on disk, memory may not free what the file
    /// still reserves. A restarted process rebuilds its lane from the ledger, so releasing here
    /// would let two processes hold one nonce — which is why the durable fact outranks `sent`.
    fn hand_lane_back(&mut self, run: &mut Run) {
        if !run.owns_lane || run.sent {
            return;
        }
        if run.lane_is_durable {
            run.report.lane = LaneRelease::Held {
                reason: "M12-F §9: a line about this attempt is durable in the execution \
                         ledger, so its nonce stays reserved even though this process reached \
                         no dispatch and no node answer"
                    .to_string(),
            };
            return;
        }
        run.report.lane = self.lane.release_unsent();
        run.owns_lane = false;
    }

    /// Fill in what the ledger now holds, so the report and the record cannot disagree.
    fn close(&mut self, run: &mut Run) {
        let id = match &run.execution_id {
            Some(id) => id.clone(),
            None => return,
        };
        if let Some(record) = self.ledger.get(&id) {
            run.report.execution_id = Some(id);
            run.report.reached = Some(record.status);
            run.report.transaction_hash = record.transaction_hash;
            // §25's `Unknown` answer is the case this has to get right: bytes left the
            // process and no rung was granted, so the record is still `Signed` and
            // `was_sent()` — a question about the ladder — answers false. The submission
            // row already encodes what the node said, so the execution row reads its
            // answer from the same fact and the two cannot disagree about whether a
            // transaction may be live.
            run.report.sent = match &run.report.submission {
                Some(submission) => submission.was_sent(),
                None => record.was_sent(),
            };
            run.report.record = Some(record.clone());
        }
    }

    fn meter_rung(
        &self,
        execution_id: &str,
        previous: Option<ExecutionStatus>,
        metrics: &mut Metrics,
    ) {
        if let Some(record) = self.ledger.get(execution_id) {
            meter(metrics, previous, record);
        }
    }
}

/// §39: the gate's own taxonomy entry for the first failure. `GateOutcome::error()` is
/// `None` only for a pass, which every caller of this has already excluded; the fallback
/// keeps the refusal's words rather than inventing an unrelated error.
fn gate_error(outcome: &GateOutcome) -> ExecutionError {
    match outcome.error() {
        Some(error) => error,
        None => ExecutionError::InvalidIntent(outcome.describe()),
    }
}
