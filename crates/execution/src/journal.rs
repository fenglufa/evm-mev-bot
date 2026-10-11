//! M12-F §5/§6: the durable execution journal.
//!
//! M12-E made one answer safe to hold — [`crate::submitter::SubmissionOutcome::Unknown`] —
//! and §25's rule for it is that nothing is resent and the nonce stays reserved. That rule
//! only means anything while the process lives: the lane and the ledger it is stated in are
//! in-memory (`stage.rs` and `sequence.rs` build both fresh at connect), so an exit between
//! the POST and the answer used to leave a process that knew *nothing* — not the hash, not
//! the nonce, not the fact that it had ever asked. This module is the part that survives:
//! every send-capable moment is written before it happens, and a restarted process reads the
//! file back and inherits exactly those three facts.
//!
//! Three rules give the design its shape:
//!
//! * **The journal is the send's predecessor, not its note-taker.** §4.1 says a run that
//!   cannot persist must not enter the send path, so an append that fails is an
//!   [`ExecutionError::LedgerPersistence`] returned *before* the socket is touched, and the
//!   only fallback available to a caller is to stop. There is no memory-only mode in a
//!   durable handle, and the handle is a required argument of every constructor that can
//!   reach a signer.
//! * **A line is a fact, not a conclusion.** [`JournalFact`] names what happened and
//!   [`FactBasis`] names what this process is entitled to claim about it; the recovery state
//!   ([`RecoveredExecution::state`]) is derived from the sequence of facts and is never
//!   written as one. `EndpointAccepted` and `ReceiptObserved` are different lines precisely
//!   because an acknowledgement is not an inclusion (§2.3), and a missing receipt is not a
//!   missing transaction. §4.2's eight facts a recovery has to tell apart are nine kinds
//!   here.
//! * **Recovery is read-only.** Opening a journal validates what is there and refuses
//!   ([`JournalFault`]) rather than repairing it; no load path appends, so recovering the
//!   same file twice gives the same answer twice (§6's duplicate question, §8's F7). The one
//!   write at open is the opening line of a file that has no lines yet.
//!
//! What is deliberately *not* here: the raw signed bytes (a journal line carrying them is a
//! bearer instrument on disk — hash, sender, nonce, chain and pin are all recovery needs,
//! and §5 forbids assuming every transaction is safe to store in plaintext); any endpoint URL
//! or credential (a line carries [`crate::submitter::EndpointKind`]'s word, and detail
//! strings reach this module already scrubbed the way [`crate::giwa`] scrubs a send reason);
//! and any resend, replacement or background reconciliation — §3.2 forbids all three, so
//! recovery's entire output is to hold a lane and answer questions.
//!
//! The guarantee range, stated once because §6 asks for it: an append is
//! `write` → `flush` → `sync_all` on a file opened `O_APPEND`, and only the fsync's return is
//! treated as "this fact is durable". That covers a **process crash**, and on a filesystem
//! whose `fsync` reaches the medium an **OS crash** too. It does *not* cover a **hardware
//! power-off** behind a disk cache that acknowledges early, and this module never claims it
//! does. What is detected in all three ranges is a torn tail — a last line without its
//! newline, or whose checksum differs from its bytes — and the answer to that is a refusal,
//! never a truncation.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, Address, B256};
use serde_json::{json, Value};

use crate::error::{ExecutionError, Result};
use crate::intent::TransactionIntent;
use crate::lifecycle::execution_id_for;
use crate::submitter::{EndpointKind, SubmissionOutcome};

/// §6's version gate, a number rather than a string so that a v2 file and a v1 reader
/// disagree on exactly one field. A build that no longer understands this constant refuses;
/// it does not migrate silently.
pub const JOURNAL_SCHEMA_VERSION: u64 = 1;

/// The environment variable a run can point the journal at, and the default it falls back to.
/// Named once so §7's production entries cannot each invent a third answer, in the shape
/// M12B §D2 gave the rest of the execution configuration.
pub const LEDGER_DIR_ENV: &str = "GIWA_EXECUTION_LEDGER_DIR";
pub const DEFAULT_LEDGER_DIR: &str = "data/ledger";

/// The word that means "this line belongs to the file, not to an execution".
const GENESIS_ID: &str = "journal";

/// The file name for a chain. One file per chain id, because §6 asks whether a program can
/// read a foreign record, and the cheapest honest answer is that a line naming another chain
/// is a fault rather than a second set of rows to keep straight.
pub fn journal_file_name(chain_id: u64) -> String {
    format!("execution-journal-{chain_id}.jsonl")
}

/// The directory a run uses when nothing names one.
pub fn default_ledger_dir() -> PathBuf {
    PathBuf::from(DEFAULT_LEDGER_DIR)
}

/// Resolve the ledger directory the way the pipeline resolves every other run directory:
/// environment first, then the milestone's default.
pub fn ledger_dir() -> PathBuf {
    match std::env::var(LEDGER_DIR_ENV) {
        Ok(text) if !text.trim().is_empty() => PathBuf::from(text),
        _ => default_ledger_dir(),
    }
}

/// The stamp a durable line carries: wall-clock Unix milliseconds, never this run's monotonic
/// clock.
///
/// M8.1 §39 keeps the two clocks apart on purpose, and this file is the chain-facing side of
/// that split. [`evm_metrics::Clock::now_ms`] counts from the moment a process began, so two
/// lines written by two processes would carry numbers that cannot be compared — which is
/// exactly the case §4.2 exists for, where a restart has to tell what happened before it died
/// from what happened after. The in-memory execution ledger keeps the monotonic stamp: its
/// rows only ever live inside one run.
pub fn journal_stamp() -> u64 {
    evm_metrics::unix_ms()
}

/// What a journal line asserts: §4.2's list of facts a recovery has to tell apart, plus the
/// file's own opening line, which is excluded from the derived record states rather than
/// counted among them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalFact {
    /// Not one of §4.2's facts: the file's opening line, written when a journal has no lines
    /// yet. Carries the schema version and the chain, so "this file predates what this build
    /// understands" is a fact on disk rather than an inference from a timestamp.
    JournalOpened,
    /// §4.2 fact 1. Bytes or an intent exist and the run is *not allowed* to send them: the
    /// mode refuses the key, or the endpoint refuses the broadcast.
    ReadyNotSent,
    /// §4.2 fact 2, and §4.1's boundary: signed bytes exist, their hash is on disk, and the
    /// send has not been started.
    SendIntentPersisted,
    /// §4.2 fact 3: the request went to the socket and the answer is not known yet. Between
    /// this line and the next lies everything §25 forbids resending.
    SendDispatched,
    /// §4.2 fact 4: the node acknowledged *these* bytes by their own hash. An acknowledgement
    /// and only that — inclusion is [`JournalFact::ReceiptObserved`].
    EndpointAccepted,
    /// §4.2 fact 5: a definite refusal, the one answer that proves the bytes are absent.
    DefiniteRefusal,
    /// §4.3's safety state as a durable line: an answer that proves nothing either way.
    OutcomeUnknown,
    /// §4.2's facts 6 and 7 in one line whose `status` word is `included` or `reverted`: the
    /// receipt was read, and the chain's own verdict came with it.
    ReceiptObserved,
    /// §4.2 fact 8: a record this build cannot close and a human must look at. Written at the
    /// moment a fresh attempt is refused because a recovered record still holds its nonce —
    /// which is when the need stops being an inference and becomes an event.
    AttentionRequired,
}

impl JournalFact {
    pub fn name(self) -> &'static str {
        match self {
            Self::JournalOpened => "journal_opened",
            Self::ReadyNotSent => "ready_not_sent",
            Self::SendIntentPersisted => "send_intent_persisted",
            Self::SendDispatched => "send_dispatched",
            Self::EndpointAccepted => "endpoint_accepted",
            Self::DefiniteRefusal => "definite_refusal",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::ReceiptObserved => "receipt_observed",
            Self::AttentionRequired => "attention_required",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::JournalOpened,
            Self::ReadyNotSent,
            Self::SendIntentPersisted,
            Self::SendDispatched,
            Self::EndpointAccepted,
            Self::DefiniteRefusal,
            Self::OutcomeUnknown,
            Self::ReceiptObserved,
            Self::AttentionRequired,
        ]
        .into_iter()
        .find(|fact| fact.name() == word)
    }

    /// Whether this line ends the question "did it leave, and did anyone answer". A record
    /// whose last line is terminal holds no lane and needs no receipt read.
    ///
    /// `EndpointAccepted` is deliberately not terminal — that is §2.3's whole point, and
    /// reading an acknowledgement as a closed record is the one misclassification that would
    /// let a restarted process reuse a live nonce.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ReadyNotSent | Self::DefiniteRefusal | Self::ReceiptObserved
        )
    }

    /// Whether the file proves the run got as far as the socket: false for an intent that was
    /// never dispatched, true from the dispatch line onward.
    pub fn proves_dispatch(self) -> bool {
        matches!(
            self,
            Self::SendDispatched
                | Self::EndpointAccepted
                | Self::DefiniteRefusal
                | Self::OutcomeUnknown
                | Self::ReceiptObserved
        )
    }

    /// The basis each fact is entitled to, fixed by the fact rather than by the caller, so a
    /// writer cannot promote its own guess to an observation.
    pub fn basis(self) -> FactBasis {
        match self {
            Self::JournalOpened
            | Self::ReadyNotSent
            | Self::SendIntentPersisted
            | Self::SendDispatched => FactBasis::ObservedLocal,
            Self::EndpointAccepted | Self::DefiniteRefusal | Self::ReceiptObserved => {
                FactBasis::ObservedNodeAnswer
            }
            Self::OutcomeUnknown | Self::AttentionRequired => FactBasis::UnconfirmedInference,
        }
    }
}

/// §4.2's other half: not only which fact, but what this process may claim about it. A reader
/// of one line has to be able to tell an observation from a conclusion without knowing how
/// the writer was built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactBasis {
    /// Something this process did or refused to do — the strongest claim the journal makes.
    ObservedLocal,
    /// Something a node said, quoted rather than interpreted. An answer about another
    /// transaction is not an acknowledgement of ours, which is why `EndpointAccepted` sits
    /// here and not in `ObservedLocal`.
    ObservedNodeAnswer,
    /// A conclusion drawn from an absence: no answer, no receipt, no verdict. Marked as an
    /// inference everywhere, because §4.3's `Unknown` → `Rejected` prohibition has to be
    /// enforceable at the field level rather than by care.
    UnconfirmedInference,
}

impl FactBasis {
    pub fn name(self) -> &'static str {
        match self {
            Self::ObservedLocal => "observed_local",
            Self::ObservedNodeAnswer => "observed_node_answer",
            Self::UnconfirmedInference => "unconfirmed_inference",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "observed_local" => Some(Self::ObservedLocal),
            "observed_node_answer" => Some(Self::ObservedNodeAnswer),
            "unconfirmed_inference" => Some(Self::UnconfirmedInference),
            _ => None,
        }
    }
}

/// The identity of a step that was never formed as a [`TransactionIntent`]: the operator's
/// deploy and configuration transactions.
///
/// §5 asks a signed transaction's identity to be the transaction's own hash rather than a
/// counter or a clock, and before the bytes are signed there is still one deterministic thing to
/// name — the hash the signature will cover. Two runs that build the same step from the same head
/// agree on it; two that disagree are two steps, which is exactly what a duplicate check has to
/// be able to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepIdentity {
    pub label: String,
    pub chain_id: u64,
    pub sender: Address,
    /// `Address::ZERO` for a creation, where the transaction has no `to`.
    pub target: Address,
    pub nonce: u64,
    pub pinned_block: Option<u64>,
    pub pinned_block_hash: Option<B256>,
    pub signing_hash: B256,
}

impl StepIdentity {
    /// The §30 key: the operator's own label and the payload's pre-signature hash. The label
    /// alone would let two different steps share one identity; the hash alone would make a human
    /// reading the file guess which step the line described.
    pub fn idempotency_key(&self) -> String {
        format!("{}:{:#x}", self.label, self.signing_hash)
    }
}

/// What a caller hands over for one line. Every field is one §5 asks recovery to have;
/// nothing here is a raw request, a raw response, a signature, a key or a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalRecord {
    pub fact: JournalFact,
    pub chain_id: u64,
    pub execution_id: String,
    pub idempotency_key: String,
    pub opportunity_id: String,
    pub sender: Address,
    pub target: Address,
    pub nonce: Option<u64>,
    pub transaction_hash: Option<B256>,
    pub pinned_block: Option<u64>,
    pub pinned_block_hash: Option<B256>,
    pub endpoint_class: Option<String>,
    /// Which step of a route this is. `None` for a lone transaction, which is what lets §8's
    /// F7 tell "the same fact written twice" apart from "two steps of one execution".
    pub position: Option<usize>,
    /// The word a receipt, an outcome or a gate produced. Never re-derived from `detail`.
    pub status: Option<String>,
    pub detail: String,
}

impl JournalRecord {
    /// The identity half, taken from the intent the attempt is already built on: the line and
    /// the intent cannot be made to disagree about the sender, because both read one field.
    pub fn from_intent(intent: &TransactionIntent, fact: JournalFact) -> Self {
        let idempotency_key = intent.ids.idempotency_key(&intent.state_binding());
        Self {
            fact,
            chain_id: intent.chain_id,
            execution_id: execution_id_for(&idempotency_key),
            idempotency_key,
            opportunity_id: intent.ids.opportunity_id.clone(),
            sender: intent.sender,
            target: intent.target,
            nonce: Some(intent.nonce),
            transaction_hash: None,
            pinned_block: Some(intent.block_number.0),
            pinned_block_hash: Some(intent.block_hash),
            endpoint_class: None,
            position: None,
            status: None,
            detail: String::new(),
        }
    }

    /// The same half for a recovered record, so `AttentionRequired` can name the execution and
    /// the nonce it is holding without an intent — the attempt that got refused never reached a
    /// build.
    pub fn from_recovered(
        recovered: &RecoveredExecution,
        fact: JournalFact,
        detail: String,
    ) -> Self {
        Self {
            fact,
            chain_id: recovered.chain_id,
            execution_id: recovered.execution_id.clone(),
            idempotency_key: recovered.idempotency_key.clone(),
            opportunity_id: recovered.opportunity_id.clone(),
            sender: recovered.sender,
            target: recovered.target,
            nonce: recovered.nonce,
            transaction_hash: recovered.transaction_hash,
            pinned_block: recovered.pinned_block,
            pinned_block_hash: recovered.pinned_block_hash,
            endpoint_class: None,
            position: None,
            status: Some(recovered.state.name().to_string()),
            detail,
        }
    }

    /// The same half for an operator step that never went through the intent layer — M10's
    /// deploy, its configuration calls, and the funding wrap (§7 counts the deployer as a
    /// production entry that has to persist before it sends).
    pub fn from_step(step: &StepIdentity, fact: JournalFact) -> Self {
        let idempotency_key = step.idempotency_key();
        Self {
            fact,
            chain_id: step.chain_id,
            execution_id: execution_id_for(&idempotency_key),
            idempotency_key,
            opportunity_id: step.label.clone(),
            sender: step.sender,
            target: step.target,
            nonce: Some(step.nonce),
            transaction_hash: None,
            pinned_block: step.pinned_block,
            pinned_block_hash: step.pinned_block_hash,
            endpoint_class: None,
            position: None,
            status: None,
            detail: String::new(),
        }
    }

    pub fn with_hash(mut self, transaction_hash: Option<B256>) -> Self {
        self.transaction_hash = transaction_hash;
        self
    }

    pub fn with_position(mut self, position: Option<usize>) -> Self {
        self.position = position;
        self
    }

    /// Re-key a step's line onto the record its run owns. §54's route has one execution id and
    /// one §30 key — `position`, not a second identity, is what keeps its steps' lines apart —
    /// and the change has to be made here rather than at each call site so no writer can land a
    /// step under an id the ledger never claimed.
    pub fn with_execution(
        mut self,
        execution_id: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        self.execution_id = execution_id.into();
        self.idempotency_key = idempotency_key.into();
        self
    }

    pub fn with_endpoint(mut self, endpoint: EndpointKind) -> Self {
        self.endpoint_class = Some(endpoint.name().to_string());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn with_fact(mut self, fact: JournalFact) -> Self {
        self.fact = fact;
        self
    }

    /// The opening line's record: no execution, no account, no pin. A zero address is not a
    /// wallet here, and `execution_id` says so in the first field a reader checks.
    fn genesis(chain_id: u64) -> Self {
        Self {
            fact: JournalFact::JournalOpened,
            chain_id,
            execution_id: GENESIS_ID.to_string(),
            idempotency_key: GENESIS_ID.to_string(),
            opportunity_id: GENESIS_ID.to_string(),
            sender: Address::ZERO,
            target: Address::ZERO,
            nonce: None,
            transaction_hash: None,
            pinned_block: None,
            pinned_block_hash: None,
            endpoint_class: None,
            position: None,
            status: Some(format!("v{JOURNAL_SCHEMA_VERSION}")),
            detail: format!(
                "journal opened at schema v{JOURNAL_SCHEMA_VERSION} for chain {chain_id} by a \
                 build that recovers execution state from this file and refuses anything it \
                 cannot read"
            ),
        }
    }

    /// The three send answers, taken from the outcome so the classification M12-E made *is* the
    /// classification written: `Accepted` cannot arrive as `ReceiptObserved` and `Unknown`
    /// cannot arrive as `DefiniteRefusal`. §4.3's never-convert rule lives at the one place an
    /// outcome is read.
    pub fn from_outcome(base: &Self, outcome: &SubmissionOutcome, endpoint: EndpointKind) -> Self {
        let fact = match outcome {
            SubmissionOutcome::Accepted { .. } => JournalFact::EndpointAccepted,
            SubmissionOutcome::Rejected { .. } => JournalFact::DefiniteRefusal,
            SubmissionOutcome::Unknown { .. } => JournalFact::OutcomeUnknown,
        };
        base.clone()
            .with_fact(fact)
            .with_endpoint(endpoint)
            .with_status(outcome.status_word())
            .with_detail(outcome_reason(outcome))
    }
}

/// The node's own words. The endpoint does not appear in them, for the reason M12-E gave at
/// the send: a journal is read by people who do not have this process's environment, and a URL
/// is a credential-shaped hole they cannot audit shut.
fn outcome_reason(outcome: &SubmissionOutcome) -> String {
    match outcome {
        SubmissionOutcome::Accepted { detail, .. } => detail.clone(),
        SubmissionOutcome::Rejected { reason, .. } => reason.clone(),
        SubmissionOutcome::Unknown { reason, .. } => reason.clone(),
    }
}

/// One validated line: a record plus the sequence number, timestamp and checksum the handle
/// gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    pub schema_version: u64,
    pub seq: u64,
    pub written_at_ms: u64,
    pub record: JournalRecord,
    pub checksum: String,
}

impl JournalEntry {
    fn new(seq: u64, written_at_ms: u64, record: JournalRecord) -> Self {
        let mut entry = Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            seq,
            written_at_ms,
            record,
            checksum: String::new(),
        };
        entry.checksum = checksum_of(&entry.preimage());
        entry
    }

    /// The bytes the checksum covers: every field length-prefixed, free text kept inside a
    /// length-prefixed segment so a delimiter cannot fake a boundary.
    ///
    /// Hand-built rather than taken from the JSON object because a hash over a map whose key
    /// order a library may change is not a checksum of anything. This shape is the module's own
    /// contract: a reader and a writer agree because both call this function.
    fn preimage(&self) -> String {
        let record = &self.record;
        [
            sized("v", &self.schema_version.to_string()),
            sized("s", &self.seq.to_string()),
            sized("t", &self.written_at_ms.to_string()),
            sized("f", record.fact.name()),
            sized("b", record.fact.basis().name()),
            sized("c", &record.chain_id.to_string()),
            sized("e", &record.execution_id),
            sized("k", &record.idempotency_key),
            sized("o", &record.opportunity_id),
            sized("g", &format!("{:?}", record.sender)),
            sized("d", &format!("{:?}", record.target)),
            sized("n", &opt_num(record.nonce)),
            sized("h", &opt_hash(record.transaction_hash)),
            sized("p", &opt_num(record.pinned_block)),
            sized("u", &opt_hash(record.pinned_block_hash)),
            sized("x", record.endpoint_class.as_deref().unwrap_or("-")),
            sized(
                "i",
                &record
                    .position
                    .map(|position| position.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            sized("z", record.status.as_deref().unwrap_or("-")),
            sized("m", &record.detail),
        ]
        .join("|")
    }

    fn to_line(&self) -> String {
        let record = &self.record;
        let value = json!({
            "schema_version": self.schema_version,
            "seq": self.seq,
            "written_at_ms": self.written_at_ms,
            "fact": record.fact.name(),
            "basis": record.fact.basis().name(),
            "chain_id": record.chain_id,
            "execution_id": record.execution_id,
            "idempotency_key": record.idempotency_key,
            "opportunity_id": record.opportunity_id,
            "sender": format!("{:?}", record.sender),
            "target": format!("{:?}", record.target),
            "nonce": record.nonce,
            "transaction_hash": record.transaction_hash.map(|hash| format!("{hash:#x}")),
            "pinned_block": record.pinned_block,
            "pinned_block_hash": record.pinned_block_hash.map(|hash| format!("{hash:#x}")),
            "endpoint_class": record.endpoint_class,
            "position": record.position,
            "status": record.status,
            "detail": record.detail,
            "checksum": self.checksum,
        });
        format!("{value}\n")
    }
}

fn sized(tag: &str, value: &str) -> String {
    format!("{tag}{}:{}", value.len(), value)
}

fn opt_num(value: Option<u64>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn opt_hash(value: Option<B256>) -> String {
    value
        .map(|hash| format!("{hash:#x}"))
        .unwrap_or_else(|| "-".to_string())
}

fn checksum_of(preimage: &str) -> String {
    format!("{:#x}", keccak256(preimage.as_bytes()))
}

/// Why a journal cannot be used: at load, because its bytes do not describe a state this
/// build may safely resume from; at append, because the fact asked for contradicts the facts
/// already durable. §6 wants a diagnostic per case, recoverable or not, and every arm names
/// the line number so a reader can go look at the bytes rather than take this build's word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalFault {
    /// The file has lines and does not begin with the line that says what it is.
    MissingGenesis { path: PathBuf },
    /// The file is there and holds nothing. Nothing in this build's own operation leaves an empty
    /// ledger behind — [`ExecutionJournal::open`] creates the file and writes its opening line
    /// before it returns — so an empty one means a journal that had lines lost them. §6's
    /// 「不得在检测到损坏后自动创建空台账并继续交易」 makes that a refusal rather than a fresh
    /// start: a process that reopened it as new would trade with no memory of a nonce that may
    /// still be live, which is the whole failure this milestone exists to prevent.
    EmptiedExternally { path: PathBuf },
    /// A line is not a JSON object. When it is the last line and the file has no trailing
    /// newline, the writer stopped in the middle of it — §6's half-written residue.
    NotJson {
        line_number: usize,
        torn_tail: bool,
        detail: String,
    },
    /// A blank line among the records: something was there and is now gone.
    EmptyLine { line_number: usize },
    /// A line without a field this build needs in order to judge it.
    MissingField {
        line_number: usize,
        field: &'static str,
    },
    /// A field that is present but is not the type or the shape it claims.
    BadField {
        line_number: usize,
        field: &'static str,
        detail: String,
    },
    /// A fact word this build does not have. Not skipped: §6 forbids getting past a record by
    /// ignoring it.
    UnknownFact { line_number: usize, word: String },
    /// A basis word that is not one of the three, or one that does not belong to the fact it
    /// is attached to.
    MismatchedBasis {
        line_number: usize,
        fact: String,
        basis: String,
    },
    /// The recomputed checksum differs from the one the line carries: the bytes were edited, or
    /// they were never written whole.
    ChecksumMismatch { line_number: usize, seq: u64 },
    /// A schema version this build was not compiled to read.
    UnsupportedSchema {
        line_number: usize,
        found: u64,
        supported: u64,
    },
    /// A line naming a chain other than the one the file belongs to.
    ForeignChain {
        line_number: usize,
        found: u64,
        expected: u64,
    },
    /// Two lines with one sequence number: one file written by two handles, which is the
    /// double-open §9's sync boundary exists to prevent.
    DuplicateSequence { line_number: usize, seq: u64 },
    /// A sequence number that does not follow the one before it.
    SequenceOutOfOrder {
        line_number: usize,
        seq: u64,
        previous: u64,
    },
    /// One execution answering differently about who sent it, on which chain, under which key,
    /// or with which nonce — §6's conflicting-nonce question.
    ConflictingIdentity {
        line_number: usize,
        execution_id: String,
        field: &'static str,
        first: String,
        second: String,
    },
    /// One step of one execution carrying two transaction hashes: one nonce, two payloads,
    /// which is §11's duplicate written down instead of prevented.
    ConflictingTransactionHash {
        line_number: usize,
        execution_id: String,
        position: Option<usize>,
        first: String,
        second: String,
    },
    /// Two executions both unresolved and both holding a nonce. §11 gives this build one lane,
    /// so the file describes a state no single process drove, and recovering either one means
    /// guessing which half of it is lying.
    TwoUnresolvedLanes { first: String, second: String },
    /// The file got shorter while this handle held it: someone truncated, replaced or rewrote
    /// it from underneath the run.
    TruncatedExternally {
        expected_bytes: u64,
        actual_bytes: u64,
    },
    /// The path still exists and is still at least as long as this handle made it, but it is a
    /// different file: someone unlinked it and wrote another one in its place, so every line
    /// this handle appends from here goes to an inode nobody will ever read again. A length
    /// check cannot see this — the replacement is often byte-identical, which is exactly what a
    /// copy-and-truncate log rotation writes — while §9 forbids the run going on with a fact
    /// only its own memory holds.
    ReplacedExternally {
        path: PathBuf,
        opened_inode: u64,
        current_inode: u64,
    },
    /// The journal could not be opened, read or written at all.
    Io { path: PathBuf, detail: String },
}

impl JournalFault {
    /// The token a program groups by; `describe` is the sentence a human reads. M8.4.4 §29's
    /// rule, applied here: a record carrying only the sentence cannot be asked which refusal
    /// fired.
    pub fn name(&self) -> &'static str {
        match self {
            Self::MissingGenesis { .. } => "missing_genesis",
            Self::EmptiedExternally { .. } => "emptied_externally",
            Self::NotJson {
                torn_tail: true, ..
            } => "torn_tail",
            Self::NotJson { .. } => "not_json",
            Self::EmptyLine { .. } => "empty_line",
            Self::MissingField { .. } => "missing_field",
            Self::BadField { .. } => "bad_field",
            Self::UnknownFact { .. } => "unknown_fact",
            Self::MismatchedBasis { .. } => "mismatched_basis",
            Self::ChecksumMismatch { .. } => "checksum_mismatch",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::ForeignChain { .. } => "foreign_chain",
            Self::DuplicateSequence { .. } => "duplicate_sequence",
            Self::SequenceOutOfOrder { .. } => "sequence_out_of_order",
            Self::ConflictingIdentity { .. } => "conflicting_identity",
            Self::ConflictingTransactionHash { .. } => "conflicting_transaction_hash",
            Self::TwoUnresolvedLanes { .. } => "two_unresolved_lanes",
            Self::TruncatedExternally { .. } => "truncated_externally",
            Self::ReplacedExternally { .. } => "replaced_externally",
            Self::Io { .. } => "io",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::MissingGenesis { path } => format!(
                "{} has lines but no opening line, so this file's identity and schema version \
                 cannot be established; it is not read further and nothing is appended to it",
                path.display()
            ),
            Self::EmptiedExternally { path } => format!(
                "{} exists and holds no line at all. A journal writes its opening line as part of \
                 being created, so an empty one is a ledger whose records were removed rather \
                 than a fresh file: this build refuses to trade on it and appends nothing to it. \
                 A human decides whether the records can be recovered from elsewhere or the path \
                 is retired; only then is the file removed",
                path.display()
            ),
            Self::NotJson {
                line_number,
                torn_tail,
                detail,
            } => format!(
                "line {line_number} is not a JSON object ({detail}): {}",
                if *torn_tail {
                    "it is also the last line and the file has no trailing newline, which is \
                     what an interrupted write leaves behind — the record was NOT dropped, and \
                     no append follows this refusal"
                } else {
                    "the record was NOT dropped, and no append follows this refusal"
                }
            ),
            Self::EmptyLine { line_number } => format!(
                "line {line_number} is blank: a journal line is never empty, so something was \
                 removed from this file; it is not read further"
            ),
            Self::MissingField { line_number, field } => format!(
                "line {line_number} carries no {field}, and a record cannot be judged on a \
                 field it does not have"
            ),
            Self::BadField {
                line_number,
                field,
                detail,
            } => format!("line {line_number} has a {field} this build cannot read ({detail})"),
            Self::UnknownFact { line_number, word } => format!(
                "line {line_number} asserts the fact `{word}`, which this build does not \
                 define; it is not skipped, because skipping a fact is how an unknown \
                 submission quietly becomes an absent one"
            ),
            Self::MismatchedBasis {
                line_number,
                fact,
                basis,
            } => format!(
                "line {line_number} pairs the fact `{fact}` with the basis `{basis}`, and this \
                 build fixes one basis per fact so that a writer cannot promote a guess to an \
                 observation"
            ),
            Self::ChecksumMismatch { line_number, seq } => format!(
                "line {line_number} (seq {seq}) does not match the checksum it carries, so its \
                 bytes were edited or were never written whole; nothing is assumed about what \
                 it said"
            ),
            Self::UnsupportedSchema {
                line_number,
                found,
                supported,
            } => format!(
                "line {line_number} is schema v{found} and this build reads v{supported}: a v1 \
                 program that meets a newer file reports it rather than guessing at fields it \
                 has never seen"
            ),
            Self::ForeignChain {
                line_number,
                found,
                expected,
            } => format!(
                "line {line_number} belongs to chain {found} in a journal for chain {expected}: \
                 one file, one chain, and a record from elsewhere is a fault"
            ),
            Self::DuplicateSequence { line_number, seq } => format!(
                "line {line_number} repeats sequence number {seq}, so one file was written by \
                 two handles; the records past that point are not trusted"
            ),
            Self::SequenceOutOfOrder {
                line_number,
                seq,
                previous,
            } => format!(
                "line {line_number} is sequence {seq} after {previous}: this is an append log, \
                 so a number that goes backwards means bytes were rewritten"
            ),
            Self::ConflictingIdentity {
                line_number,
                execution_id,
                field,
                first,
                second,
            } => format!(
                "{execution_id} answers differently about its {field} at line {line_number}: it \
                 was {first} and is {second}; one execution has one identity"
            ),
            Self::ConflictingTransactionHash {
                line_number,
                execution_id,
                position,
                first,
                second,
            } => format!(
                "{execution_id} step {} carries two transaction hashes at line {line_number} \
                 ({first} and {second}): one position in one execution is one signed payload",
                position
                    .map(|position| (position + 1).to_string())
                    .unwrap_or_else(|| "(lone)".to_string())
            ),
            Self::TwoUnresolvedLanes { first, second } => format!(
                "{first} and {second} are both unresolved and both hold a nonce; §11 gives this \
                 build one lane, so recovering either would mean choosing which record the file \
                 is lying about"
            ),
            Self::TruncatedExternally {
                expected_bytes,
                actual_bytes,
            } => format!(
                "the journal file is {actual_bytes} bytes where this handle wrote \
                 {expected_bytes}: it was truncated or replaced while open, and appending to a \
                 file that has lost bytes would make the surviving records unverifiable"
            ),
            Self::ReplacedExternally {
                path,
                opened_inode,
                current_inode,
            } => format!(
                "{} is inode {current_inode} while this handle writes inode {opened_inode}: the \
                 journal was replaced from underneath the run, so every line appended from here \
                 would land on a file no restart will read; §9 forbids the run going on with a \
                 fact only its own memory holds, and this append is refused",
                path.display()
            ),
            Self::Io { path, detail } => {
                format!(
                    "{} could not be used as a journal: {detail}",
                    path.display()
                )
            }
        }
    }
}

/// What the file says about one execution once every line has been read. The state is derived
/// from the facts and never written as one (§4.2's separation of intent from fact).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveredState {
    /// Nothing left to decide: never dispatched, definitively refused, or settled by a
    /// receipt. The lane this record held is not held.
    Resolved,
    /// §4.2's "intent persisted, maybe never sent": an intent line with no dispatch after it.
    /// The local file cannot prove the request never left this process — §4.2 says a post-crash
    /// state proves nothing of the kind — so the nonce stays reserved and nothing is resent.
    PossiblyInFlight,
    /// §4.3's preserved `Unknown`: the dispatch line is there, and either no answer followed
    /// it, or the answer proved nothing, or the node acknowledged the hash and no receipt has
    /// been read. Closed only by a receipt or a definite refusal, both of which are *reads*,
    /// and §25 keeps the lane until one arrives.
    AwaitingReceipt,
}

impl RecoveredState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::PossiblyInFlight => "possibly_in_flight",
            Self::AwaitingReceipt => "awaiting_receipt",
        }
    }

    /// Whether this state keeps a nonce out of circulation.
    pub fn holds_lane(self) -> bool {
        !matches!(self, Self::Resolved)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredExecution {
    pub execution_id: String,
    pub idempotency_key: String,
    pub opportunity_id: String,
    /// Which step of a route this record is, and `None` for a lone transaction. A route folds
    /// as one record *per step* under a shared execution id precisely because each step carries
    /// its own nonce: the question §4.3 asks is "which nonce is still held", and a record that
    /// held two of them could not answer it.
    pub position: Option<usize>,
    pub chain_id: u64,
    pub sender: Address,
    pub target: Address,
    pub nonce: Option<u64>,
    /// The hash this file named for this step. Under one record per step a route's records each
    /// name their own payload, which is what lets §8's F7 tell two steps of one execution apart
    /// from the same fact written twice.
    pub transaction_hash: Option<B256>,
    pub step_hashes: Vec<(Option<usize>, B256)>,
    pub pinned_block: Option<u64>,
    pub pinned_block_hash: Option<B256>,
    /// Every fact in file order, so a reader can tell an intent that was never dispatched from
    /// a dispatch that was never answered without trusting any wording here.
    pub facts: Vec<JournalFact>,
    pub last_fact: JournalFact,
    pub receipt_status: Option<String>,
    pub state: RecoveredState,
    /// §4.3's minimum bar: unresolved means queryable, auditable, and lane-blocking.
    pub needs_attention: bool,
    pub first_seen_at_ms: u64,
    pub last_seen_at_ms: u64,
    pub detail: String,
}

impl RecoveredExecution {
    pub fn dispatched(&self) -> bool {
        self.facts.iter().any(|fact| fact.proves_dispatch())
    }

    /// One line naming this record for a log or an evidence row.
    pub fn line(&self) -> String {
        format!(
            "{} step={} state={} last_fact={} basis={} nonce={} tx={} attention={} facts={}",
            self.execution_id,
            self.position
                .map(|position| (position + 1).to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.state.name(),
            self.last_fact.name(),
            self.last_fact.basis().name(),
            opt_num(self.nonce),
            opt_hash(self.transaction_hash),
            self.needs_attention,
            self.facts
                .iter()
                .map(|fact| fact.name())
                .collect::<Vec<_>>()
                .join(">"),
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "execution_id": self.execution_id,
            "idempotency_key": self.idempotency_key,
            "opportunity_id": self.opportunity_id,
            "position": self.position.map(|position| position + 1),
            "chain_id": self.chain_id,
            "sender": format!("{:?}", self.sender),
            "target": format!("{:?}", self.target),
            "nonce": self.nonce,
            "transaction_hash": self.transaction_hash.map(|hash| format!("{hash:#x}")),
            "step_hashes": self
                .step_hashes
                .iter()
                .map(|(position, hash)| json!({
                    "position": position.map(|position| position + 1),
                    "transaction_hash": format!("{hash:#x}"),
                }))
                .collect::<Vec<_>>(),
            "pinned_block": self.pinned_block,
            "pinned_block_hash": self
                .pinned_block_hash
                .map(|hash| format!("{hash:#x}")),
            "facts": self.facts.iter().map(|fact| fact.name()).collect::<Vec<_>>(),
            "last_fact": self.last_fact.name(),
            "last_basis": self.last_fact.basis().name(),
            "dispatched": self.dispatched(),
            "receipt_status": self.receipt_status,
            "state": self.state.name(),
            "holds_lane": self.state.holds_lane(),
            "needs_attention": self.needs_attention,
            "first_seen_at_ms": self.first_seen_at_ms,
            "last_seen_at_ms": self.last_seen_at_ms,
            "detail": self.detail,
        })
    }
}

/// What one pass over the file answers: §7's steps 1–7. Step 8 — a normal flow may start — is
/// the caller acting on this, and `lane_occupancy` is what step 4 rebuilds.
#[derive(Clone, Debug, Default)]
pub struct JournalRecovery {
    pub path: Option<PathBuf>,
    /// Lines known to this handle, the opening one included.
    pub entries: u64,
    /// Lines that were already in the file when this handle opened it.
    pub loaded_entries: u64,
    pub records: Vec<RecoveredExecution>,
    /// The nonce the file says is still reserved, and the record that reserves it. Empty when
    /// every execution reached a terminal fact.
    pub lane_occupancy: Vec<(Address, u64, String)>,
    /// Whether the file had no lines before this handle wrote its opening one. A fresh journal
    /// is not a journal that recovered successfully, and §6's wording depends on the gap. An
    /// existing file that had been emptied is *not* fresh: `open` refuses it as
    /// [`JournalFault::EmptiedExternally`], because no operation of this build leaves an empty
    /// ledger behind.
    pub fresh: bool,
}

impl JournalRecovery {
    pub fn unresolved(&self) -> impl Iterator<Item = &RecoveredExecution> {
        self.records
            .iter()
            .filter(|record| record.state.holds_lane())
    }

    pub fn needs_attention(&self) -> impl Iterator<Item = &RecoveredExecution> {
        self.records.iter().filter(|record| record.needs_attention)
    }

    /// §7's step 3 lookup, and the reason recovery is worth doing: the triple a restarted run
    /// is about to claim may already own a durable record.
    pub fn by_idempotency_key(&self, idempotency_key: &str) -> Option<&RecoveredExecution> {
        self.records
            .iter()
            .find(|record| record.idempotency_key == idempotency_key)
    }

    pub fn by_execution_id(&self, execution_id: &str) -> Option<&RecoveredExecution> {
        self.records
            .iter()
            .find(|record| record.execution_id == execution_id)
    }

    pub fn summary(&self) -> String {
        let lane = if self.lane_occupancy.is_empty() {
            "idle".to_string()
        } else {
            self.lane_occupancy
                .iter()
                .map(|(address, nonce, _)| format!("held:{nonce}@{address:?}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "ledger={} entries={} loaded={} records={} unresolved={} attention={} lane={} \
             fresh={}",
            self.path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "volatile".to_string()),
            self.entries,
            self.loaded_entries,
            self.records.len(),
            self.unresolved().count(),
            self.needs_attention().count(),
            lane,
            self.fresh,
        )
    }
}

/// What one `append` did. `Duplicate` is a success: §9 asks a repeated write of the same
/// result to create no second record, and silence about a suppressed write would be worse than
/// saying so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    Written { seq: u64 },
    Duplicate,
}

impl AppendOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Written { .. } => "written",
            Self::Duplicate => "duplicate",
        }
    }
}

/// The handle: one open journal, one chain, one writer.
///
/// §9's sync boundary is this type. It is a required argument of every stage constructor that
/// can reach a signer, so a run either names a file or says `volatile` out loud in its argument
/// list, and no default quietly leaves the ledger in memory.
#[derive(Debug)]
pub struct ExecutionJournal {
    durable: bool,
    path: Option<PathBuf>,
    file: Option<std::fs::File>,
    /// The watermark §6's truncation question needs: how many bytes this handle has written,
    /// checked against the file's length before every write.
    bytes_written: u64,
    /// The inode this handle opened, which is what tells a replacement apart from a rewrite in
    /// place. The length watermark cannot: a file that was unlinked and written again with the
    /// same bytes is the same length and a different file, and every append after that goes to
    /// an inode the path no longer names.
    inode: Option<u64>,
    next_seq: u64,
    recovery: JournalRecovery,
    /// The §30 keys and the lane occupancy the file handed this handle when it opened, frozen at
    /// that moment. `recovery` grows with every line this run writes, and the two questions they
    /// answer are different: "has this process already claimed this attempt" belongs to the
    /// in-memory ledger, while "does a record from a dead process own this nonce" belongs here.
    /// Keeping them apart is what lets §7's cross-process dedup refuse a restarted duplicate
    /// without changing what a single-process run sees.
    restored_keys: BTreeSet<String>,
    restored_lane: Vec<(Address, u64, String)>,
    /// Facts this handle has already written, so a repeated write of one result is a
    /// `Duplicate` rather than a second line. A route's steps differ by `position`, and two
    /// different hashes for one position are refused as a conflict.
    written: BTreeSet<DedupKey>,
    /// The lines a volatile handle kept, so a test can compare the same model's output against
    /// a durable file byte for byte.
    lines: Vec<String>,
    appended: u64,
}

type DedupKey = (String, Option<usize>, String, Option<String>);

impl ExecutionJournal {
    /// Open — and, for a file with no lines yet, start — the journal for one chain.
    ///
    /// Every failure here is a refusal to run: §7 requires that a critical record which cannot
    /// be recovered stop the process rather than trade around it, and a directory that cannot be
    /// created is the same answer one step earlier.
    pub fn open(dir: &Path, chain_id: u64, at_ms: u64) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|error| {
            persistence_error(JournalFault::Io {
                path: dir.to_path_buf(),
                detail: format!(
                    "the ledger directory could not be created ({error}); §4.1 gives a run that \
                     cannot persist no way into a send"
                ),
            })
        })?;
        let path = dir.join(journal_file_name(chain_id));
        let existed = path.exists();
        let mut entries: Vec<(usize, JournalEntry)> = Vec::new();
        if existed {
            let text = std::fs::read(&path).map_err(|error| {
                recovery_error(JournalFault::Io {
                    path: path.clone(),
                    detail: error.to_string(),
                })
            })?;
            entries = validate(&text, chain_id, &path).map_err(recovery_error)?;
            if entries.is_empty() {
                return Err(recovery_error(JournalFault::EmptiedExternally { path }));
            }
        }
        let loaded_entries = entries.len() as u64;
        let mut recovery = derive(&entries, &path).map_err(recovery_error)?;
        recovery.path = Some(path.clone());
        recovery.entries = loaded_entries;
        recovery.loaded_entries = loaded_entries;
        recovery.fresh = entries.is_empty();
        let next_seq = entries
            .last()
            .map(|(_, entry)| entry.seq + 1)
            .unwrap_or_default();
        let bytes_written = if existed {
            std::fs::metadata(&path)
                .map_err(|error| {
                    recovery_error(JournalFault::Io {
                        path: path.clone(),
                        detail: error.to_string(),
                    })
                })?
                .len()
        } else {
            0
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                persistence_error(JournalFault::Io {
                    path: path.clone(),
                    detail: format!(
                        "the journal could not be opened for appending ({error}); §11 forbids a \
                         memory-only production fallback"
                    ),
                })
            })?;
        let restored_lane = lane_occupancy(&recovery.records);
        let restored_keys = recovery
            .records
            .iter()
            .map(|record| record.idempotency_key.clone())
            .collect::<BTreeSet<_>>();
        // Read off the handle's own file rather than off the path, so the number below is the
        // inode these bytes go to and not whatever the name happens to point at later.
        let inode = file.metadata().ok().and_then(|m| inode_of(&m));
        let mut handle = Self {
            durable: true,
            path: Some(path.clone()),
            file: Some(file),
            bytes_written,
            inode,
            next_seq,
            written: entries.iter().map(|(_, entry)| dedup_key(entry)).collect(),
            recovery,
            restored_keys,
            restored_lane,
            lines: Vec::new(),
            appended: 0,
        };
        if entries.is_empty() {
            // The opening line is the file's own answer to "which schema am I", so a build that
            // meets a newer file is refused at it instead of half-parsing it. A journal that
            // already has lines said this once and is not told twice.
            handle.append(at_ms, JournalRecord::genesis(chain_id))?;
        }
        Ok(handle)
    }

    /// A handle that keeps its lines in memory and writes nothing — for the unit tests that
    /// judge the model, and for no production entry. §11 says a memory-only mode is not an
    /// acceptable fallback, which is why it is a call a caller makes out loud rather than a
    /// default a constructor reaches for.
    pub fn volatile() -> Self {
        Self {
            durable: false,
            path: None,
            file: None,
            bytes_written: 0,
            inode: None,
            next_seq: 0,
            written: BTreeSet::new(),
            recovery: JournalRecovery::default(),
            restored_keys: BTreeSet::new(),
            restored_lane: Vec::new(),
            lines: Vec::new(),
            appended: 0,
        }
    }

    pub fn is_durable(&self) -> bool {
        self.durable
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The lines a volatile handle kept. Empty for a durable one, whose lines are in the file.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn recovery(&self) -> &JournalRecovery {
        &self.recovery
    }

    pub fn entries(&self) -> u64 {
        self.recovery.entries
    }

    pub fn appended(&self) -> u64 {
        self.appended
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The record a restarted run must not treat as its own: the same §30 triple, already
    /// durable, and what the file says about it.
    pub fn recovered_for(&self, idempotency_key: &str) -> Option<&RecoveredExecution> {
        self.recovery.by_idempotency_key(idempotency_key)
    }

    /// §7's step 3, and the reason a restarted process cannot simply re-run an attempt: the key
    /// was already durable *before this handle opened*, so the record behind it belongs to a run
    /// this process never drove. Lines this run wrote itself are not in scope — the in-memory
    /// [`crate::lifecycle::Ledger`] already answers that question, and §9 keeps the two stores
    /// from disagreeing about which one a duplicate came from.
    pub fn restored_for(&self, idempotency_key: &str) -> Option<&RecoveredExecution> {
        if self.restored_keys.contains(idempotency_key) {
            self.recovery.by_idempotency_key(idempotency_key)
        } else {
            None
        }
    }

    /// §7's step 3 answered for a whole route: the first restored record under this execution id
    /// that proves the run got as far as the wire, or that the wire answered.
    ///
    /// A route folds as several records — one per step — so looking one up by key would return
    /// only its first step and miss the step that was actually dispatched. The predicate is the
    /// same one §30 needs: a record whose last fact is `ready_not_sent` proves the mode or the
    /// endpoint refused, which is not a fact about a transaction, and refusing a fresh attempt on
    /// it would turn a rehearsal into a permanent block on the opportunity.
    pub fn restored_executed(&self, execution_id: &str) -> Option<&RecoveredExecution> {
        self.recovery.records.iter().find(|record| {
            record.execution_id == execution_id
                && self.restored_keys.contains(&record.idempotency_key)
                && (record.state.holds_lane()
                    || matches!(
                        record.last_fact,
                        JournalFact::DefiniteRefusal | JournalFact::ReceiptObserved
                    ))
        })
    }

    /// §7's step 4 input: every `(address, nonce, execution_id)` the file says a dead process
    /// left unresolved, in file order. A stage installs these on its lane in its constructor, so
    /// the first allocation attempt after a restart meets the same one-lane refusal the crashed
    /// process would have met.
    pub fn restored_lane(&self) -> &[(Address, u64, String)] {
        &self.restored_lane
    }

    /// Write one fact. The order of operations is the whole of §4.1: this call returns `Ok`
    /// before the caller may touch a socket, and any `Err` is the caller's stop signal.
    pub fn append(&mut self, at_ms: u64, record: JournalRecord) -> Result<AppendOutcome> {
        let key = dedup_tuple(&record);
        if self.written.contains(&key) {
            return Ok(AppendOutcome::Duplicate);
        }
        let seq = self.next_seq;
        let entry = JournalEntry::new(seq, at_ms, record);
        // The fold is computed and checked *before* the bytes go out, so a record the journal's
        // own invariants refuse leaves both the file and this handle's state untouched (§9's
        // "a failed update must not fork memory against disk").
        let merged = merge(&self.recovery.records, &entry).map_err(conflict_error)?;
        let lane = lane_occupancy(&merged);
        if lane.len() > 1 {
            return Err(conflict_error(JournalFault::TwoUnresolvedLanes {
                first: lane[0].2.clone(),
                second: lane[1].2.clone(),
            }));
        }
        let line = entry.to_line();
        self.write_line(&line)?;
        self.next_seq += 1;
        self.appended += 1;
        self.recovery.entries += 1;
        self.written.insert(key);
        self.recovery.records = merged;
        self.recovery.lane_occupancy = lane;
        Ok(AppendOutcome::Written { seq })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        let Some(path) = self.path.clone() else {
            self.lines.push(line.to_string());
            return Ok(());
        };
        let actual = std::fs::metadata(&path).map_err(|error| {
            persistence_error(JournalFault::Io {
                path: path.clone(),
                detail: error.to_string(),
            })
        })?;
        if let (Some(opened), Some(current)) = (self.inode, inode_of(&actual)) {
            if opened != current {
                return Err(persistence_error(JournalFault::ReplacedExternally {
                    path: path.clone(),
                    opened_inode: opened,
                    current_inode: current,
                }));
            }
        }
        if actual.len() < self.bytes_written {
            return Err(persistence_error(JournalFault::TruncatedExternally {
                expected_bytes: self.bytes_written,
                actual_bytes: actual.len(),
            }));
        }
        let written = line.len() as u64;
        let file = self.file.as_mut().ok_or_else(|| {
            ExecutionError::LedgerPersistence(format!(
                "io — {}: the handle was opened and then lost its file",
                path.display()
            ))
        })?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.flush())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                persistence_error(JournalFault::Io {
                    path: path.clone(),
                    detail: format!(
                        "a journal line did not reach the file ({error}). §4.1's rule is that a \
                         run which cannot persist does not send, is not treated as submitted, \
                         and does not release another execution's nonce; this error is a stop, \
                         not a note."
                    ),
                })
            })?;
        self.bytes_written += written;
        Ok(())
    }

    /// Read the file from disk and answer for its records as a restarted process would see
    /// them. §8's crash tests use this to prove an assertion was made from bytes an *earlier*
    /// handle wrote, not from state the writer still had.
    pub fn reload(dir: &Path, chain_id: u64) -> Result<JournalRecovery> {
        let path = dir.join(journal_file_name(chain_id));
        let text = std::fs::read(&path).map_err(|error| {
            recovery_error(JournalFault::Io {
                path: path.clone(),
                detail: error.to_string(),
            })
        })?;
        let entries = validate(&text, chain_id, &path).map_err(recovery_error)?;
        if entries.is_empty() {
            return Err(recovery_error(JournalFault::EmptiedExternally { path }));
        }
        let mut recovery = derive(&entries, &path).map_err(recovery_error)?;
        recovery.path = Some(path);
        recovery.entries = entries.len() as u64;
        recovery.loaded_entries = entries.len() as u64;
        Ok(recovery)
    }
}

/// The inode behind a metadata result, on the platforms this ledger runs on. Where no such
/// number exists the guard is inert rather than wrong: the length watermark below still runs, and
/// a replacement that also shortened the file is still refused.
fn inode_of(metadata: &std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn recovery_error(fault: JournalFault) -> ExecutionError {
    ExecutionError::LedgerRecovery(format!("{} — {}", fault.name(), fault.describe()))
}

/// The write side carries the same token vocabulary as the read side, so §8's F8 evidence can
/// group by refusal (`io`, `truncated_externally`) instead of by the sentence that surrounds it.
fn persistence_error(fault: JournalFault) -> ExecutionError {
    ExecutionError::LedgerPersistence(format!("{} — {}", fault.name(), fault.describe()))
}

/// The same faults `derive` can meet at a restart, reported by the handle that was about to
/// write. The variant says which side was asking: "this file cannot be recovered" and "this run
/// was stopped before it could send" are different answers to give an operator, even when the
/// token underneath is `conflicting_identity` for both.
fn conflict_error(fault: JournalFault) -> ExecutionError {
    ExecutionError::LedgerConflict(format!("{} — {}", fault.name(), fault.describe()))
}

fn dedup_key(entry: &JournalEntry) -> DedupKey {
    dedup_tuple(&entry.record)
}

fn dedup_tuple(record: &JournalRecord) -> DedupKey {
    (
        record.execution_id.clone(),
        record.position,
        record.fact.name().to_string(),
        record
            .transaction_hash
            .map(|hash| format!("{hash:#x}"))
            .or_else(|| record.status.clone()),
    )
}

/// Parse, and refuse. Every fault class §6 names has an arm here, and the walk stops at the
/// first one: a journal this build cannot vouch for is not a journal whose remaining lines can
/// be salvaged.
fn validate(
    text: &[u8],
    chain_id: u64,
    path: &Path,
) -> std::result::Result<Vec<(usize, JournalEntry)>, JournalFault> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let ends_with_newline = text.last() == Some(&b'\n');
    let body = String::from_utf8_lossy(text);
    // A file that does not end in a newline has a last line the writer never finished. That
    // line is still parsed, with the knowledge that it is last, so the fault names the torn tail
    // rather than the JSON error that follows from it.
    let mut pieces: Vec<&str> = body.split('\n').collect();
    if pieces.last().copied() == Some("") {
        pieces.pop();
    }
    let total = pieces.len();
    let mut entries: Vec<(usize, JournalEntry)> = Vec::new();
    let mut previous_seq: Option<u64> = None;
    for (index, raw) in pieces.into_iter().enumerate() {
        let line_number = index + 1;
        if raw.trim().is_empty() {
            return Err(JournalFault::EmptyLine { line_number });
        }
        let entry = parse_line(raw, line_number, index + 1 == total && !ends_with_newline)?;
        if entry.record.fact == JournalFact::JournalOpened && line_number != 1 {
            return Err(JournalFault::UnknownFact {
                line_number,
                word: "journal_opened appears after the first line".to_string(),
            });
        }
        if let Some(previous) = previous_seq {
            if entry.seq == previous {
                return Err(JournalFault::DuplicateSequence {
                    line_number,
                    seq: entry.seq,
                });
            }
            if entry.seq < previous {
                return Err(JournalFault::SequenceOutOfOrder {
                    line_number,
                    seq: entry.seq,
                    previous,
                });
            }
        }
        if entry.record.chain_id != chain_id {
            return Err(JournalFault::ForeignChain {
                line_number,
                found: entry.record.chain_id,
                expected: chain_id,
            });
        }
        previous_seq = Some(entry.seq);
        entries.push((line_number, entry));
    }
    match entries.first() {
        Some((_, first)) if first.record.fact == JournalFact::JournalOpened => Ok(entries),
        _ => Err(JournalFault::MissingGenesis {
            path: path.to_path_buf(),
        }),
    }
}

fn parse_line(
    raw: &str,
    line_number: usize,
    torn_tail: bool,
) -> std::result::Result<JournalEntry, JournalFault> {
    let value: Value = serde_json::from_str(raw).map_err(|error| JournalFault::NotJson {
        line_number,
        torn_tail,
        detail: error.to_string(),
    })?;
    let object = value.as_object().ok_or(JournalFault::NotJson {
        line_number,
        torn_tail,
        detail: "the line is JSON but not an object".to_string(),
    })?;
    let schema_version = number(object, "schema_version", line_number)?;
    if schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(JournalFault::UnsupportedSchema {
            line_number,
            found: schema_version,
            supported: JOURNAL_SCHEMA_VERSION,
        });
    }
    let seq = number(object, "seq", line_number)?;
    let written_at_ms = number(object, "written_at_ms", line_number)?;
    let fact_word = string(object, "fact", line_number)?;
    let fact = JournalFact::parse(&fact_word).ok_or(JournalFault::UnknownFact {
        line_number,
        word: fact_word.clone(),
    })?;
    let basis_word = string(object, "basis", line_number)?;
    let basis = FactBasis::parse(&basis_word).ok_or(JournalFault::MismatchedBasis {
        line_number,
        fact: fact_word.clone(),
        basis: basis_word.clone(),
    })?;
    if basis != fact.basis() {
        return Err(JournalFault::MismatchedBasis {
            line_number,
            fact: fact_word,
            basis: basis_word,
        });
    }
    let record = JournalRecord {
        fact,
        chain_id: number(object, "chain_id", line_number)?,
        execution_id: string(object, "execution_id", line_number)?,
        idempotency_key: string(object, "idempotency_key", line_number)?,
        opportunity_id: string(object, "opportunity_id", line_number)?,
        sender: address(object, "sender", line_number)?,
        target: address(object, "target", line_number)?,
        nonce: optional_number(object, "nonce", line_number)?,
        transaction_hash: optional_hash(object, "transaction_hash", line_number)?,
        pinned_block: optional_number(object, "pinned_block", line_number)?,
        pinned_block_hash: optional_hash(object, "pinned_block_hash", line_number)?,
        endpoint_class: optional_string(object, "endpoint_class", line_number)?,
        position: optional_number(object, "position", line_number)?
            .map(|position| position as usize),
        status: optional_string(object, "status", line_number)?,
        detail: string(object, "detail", line_number)?,
    };
    let checksum = string(object, "checksum", line_number)?;
    let entry = JournalEntry {
        schema_version,
        seq,
        written_at_ms,
        record,
        checksum,
    };
    if checksum_of(&entry.preimage()) != entry.checksum {
        return Err(JournalFault::ChecksumMismatch { line_number, seq });
    }
    Ok(entry)
}

fn field_of<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<&'a Value, JournalFault> {
    object
        .get(field)
        .ok_or(JournalFault::MissingField { line_number, field })
}

fn number(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<u64, JournalFault> {
    field_of(object, field, line_number)?
        .as_u64()
        .ok_or(JournalFault::BadField {
            line_number,
            field,
            detail: "not a non-negative number".to_string(),
        })
}

fn string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<String, JournalFault> {
    field_of(object, field, line_number)?
        .as_str()
        .map(|text| text.to_string())
        .ok_or(JournalFault::BadField {
            line_number,
            field,
            detail: "not a string".to_string(),
        })
}

fn address(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<Address, JournalFault> {
    let text = string(object, field, line_number)?;
    text.parse::<Address>()
        .map_err(|error| JournalFault::BadField {
            line_number,
            field,
            detail: format!("{text} is not an address: {error}"),
        })
}

fn optional_number(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<Option<u64>, JournalFault> {
    match field_of(object, field, line_number)? {
        Value::Null => Ok(None),
        value => value.as_u64().map(Some).ok_or(JournalFault::BadField {
            line_number,
            field,
            detail: "neither null nor a non-negative number".to_string(),
        }),
    }
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<Option<String>, JournalFault> {
    match field_of(object, field, line_number)? {
        Value::Null => Ok(None),
        _ => string(object, field, line_number).map(Some),
    }
}

fn optional_hash(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    line_number: usize,
) -> std::result::Result<Option<B256>, JournalFault> {
    let Some(text) = optional_string(object, field, line_number)? else {
        return Ok(None);
    };
    text.parse::<B256>()
        .map(Some)
        .map_err(|error| JournalFault::BadField {
            line_number,
            field,
            detail: format!("{text} is not a 32-byte hash: {error}"),
        })
}

/// §7's steps 3–7: one state per execution, rebuilt from the facts in file order. Every fault
/// `merge` can name is checked here as it would be by a live handle, so a corrupted file and a
/// contradictory running process get the same refusal.
fn derive(
    entries: &[(usize, JournalEntry)],
    path: &Path,
) -> std::result::Result<JournalRecovery, JournalFault> {
    let mut folded: Vec<RecoveredExecution> = Vec::new();
    for (line_number, entry) in entries {
        folded = merge(&folded, entry).map_err(|fault| at_line(*line_number, fault))?;
    }
    folded.sort_by(|left, right| {
        left.first_seen_at_ms
            .cmp(&right.first_seen_at_ms)
            .then_with(|| left.execution_id.cmp(&right.execution_id))
    });
    let lane_occupancy = lane_occupancy(&folded);
    if lane_occupancy.len() > 1 {
        return Err(JournalFault::TwoUnresolvedLanes {
            first: lane_occupancy[0].2.clone(),
            second: lane_occupancy[1].2.clone(),
        });
    }
    Ok(JournalRecovery {
        path: Some(path.to_path_buf()),
        entries: entries.len() as u64,
        loaded_entries: entries.len() as u64,
        records: folded,
        lane_occupancy,
        fresh: false,
    })
}

/// Some faults name a line the fold does not know; this puts it back.
fn at_line(line_number: usize, fault: JournalFault) -> JournalFault {
    match fault {
        JournalFault::ConflictingIdentity {
            execution_id,
            field,
            first,
            second,
            ..
        } => JournalFault::ConflictingIdentity {
            line_number,
            execution_id,
            field,
            first,
            second,
        },
        JournalFault::ConflictingTransactionHash {
            execution_id,
            position,
            first,
            second,
            ..
        } => JournalFault::ConflictingTransactionHash {
            line_number,
            execution_id,
            position,
            first,
            second,
        },
        other => other,
    }
}

/// Fold one line into the record list: append the fact, re-derive every state, and keep the
/// first answer to each identity question the file has already settled. This is the only place
/// a state is computed, so a running handle and a restarted reader cannot disagree about what
/// the same lines mean.
fn merge(
    records: &[RecoveredExecution],
    entry: &JournalEntry,
) -> std::result::Result<Vec<RecoveredExecution>, JournalFault> {
    let record = &entry.record;
    if record.fact == JournalFact::JournalOpened {
        // The opening line describes the file, not an execution. Folding it would give every
        // journal a record that never dispatched and never resolved — which is to say, a
        // phantom lane occupant.
        return Ok(records.to_vec());
    }
    let mut merged = records.to_vec();
    let existing = merged.iter().position(|existing| {
        // A route's steps fold as separate records under one execution id. The pairing is what
        // makes the fold answer §4.3's question — a step record has exactly one nonce, so an
        // unresolved record *is* a held nonce — and it keeps the identity checks below from
        // reading a legitimate second step as a second claim on the same one.
        existing.execution_id == record.execution_id && existing.position == record.position
    });
    match existing {
        Some(index) => {
            let before = &merged[index];
            if before.idempotency_key != record.idempotency_key {
                return Err(JournalFault::ConflictingIdentity {
                    line_number: 0,
                    execution_id: record.execution_id.clone(),
                    field: "idempotency_key",
                    first: before.idempotency_key.clone(),
                    second: record.idempotency_key.clone(),
                });
            }
            if before.chain_id != record.chain_id {
                return Err(JournalFault::ConflictingIdentity {
                    line_number: 0,
                    execution_id: record.execution_id.clone(),
                    field: "chain_id",
                    first: before.chain_id.to_string(),
                    second: record.chain_id.to_string(),
                });
            }
            if before.sender != record.sender {
                return Err(JournalFault::ConflictingIdentity {
                    line_number: 0,
                    execution_id: record.execution_id.clone(),
                    field: "sender",
                    first: format!("{:?}", before.sender),
                    second: format!("{:?}", record.sender),
                });
            }
            match (before.nonce, record.nonce) {
                (Some(first), Some(second)) if first != second => {
                    return Err(JournalFault::ConflictingIdentity {
                        line_number: 0,
                        execution_id: record.execution_id.clone(),
                        field: "nonce",
                        first: first.to_string(),
                        second: second.to_string(),
                    })
                }
                _ => {}
            }
            if let Some(hash) = record.transaction_hash {
                if let Some((_, first)) = before
                    .step_hashes
                    .iter()
                    .find(|(position, _)| *position == record.position)
                {
                    if *first != hash {
                        return Err(JournalFault::ConflictingTransactionHash {
                            line_number: 0,
                            execution_id: record.execution_id.clone(),
                            position: record.position,
                            first: format!("{first:#x}"),
                            second: format!("{hash:#x}"),
                        });
                    }
                }
            }
            let after = &mut merged[index];
            after.facts.push(record.fact);
            after.last_fact = record.fact;
            after.last_seen_at_ms = entry.written_at_ms;
            if after.nonce.is_none() {
                after.nonce = record.nonce;
            }
            if after.transaction_hash.is_none() {
                after.transaction_hash = record.transaction_hash;
            }
            if let Some(hash) = record.transaction_hash {
                if !after
                    .step_hashes
                    .iter()
                    .any(|(position, _)| *position == record.position)
                {
                    after.step_hashes.push((record.position, hash));
                }
            }
            if after.pinned_block.is_none() {
                after.pinned_block = record.pinned_block;
                after.pinned_block_hash = record.pinned_block_hash;
            }
            if record.fact == JournalFact::ReceiptObserved {
                after.receipt_status = record.status.clone();
            }
            if record.fact == JournalFact::AttentionRequired {
                after.needs_attention = true;
            }
            if !record.detail.is_empty() {
                after.detail = record.detail.clone();
            }
        }
        None => {
            merged.push(RecoveredExecution {
                execution_id: record.execution_id.clone(),
                idempotency_key: record.idempotency_key.clone(),
                opportunity_id: record.opportunity_id.clone(),
                position: record.position,
                chain_id: record.chain_id,
                sender: record.sender,
                target: record.target,
                nonce: record.nonce,
                transaction_hash: record.transaction_hash,
                step_hashes: record
                    .transaction_hash
                    .map(|hash| vec![(record.position, hash)])
                    .unwrap_or_default(),
                pinned_block: record.pinned_block,
                pinned_block_hash: record.pinned_block_hash,
                facts: vec![record.fact],
                last_fact: record.fact,
                receipt_status: if record.fact == JournalFact::ReceiptObserved {
                    record.status.clone()
                } else {
                    None
                },
                state: RecoveredState::Resolved,
                needs_attention: record.fact == JournalFact::AttentionRequired,
                first_seen_at_ms: entry.written_at_ms,
                last_seen_at_ms: entry.written_at_ms,
                detail: record.detail.clone(),
            });
        }
    }
    for record in &mut merged {
        record.state = state_of(&record.facts);
        if record.state.holds_lane() {
            record.needs_attention = true;
        }
    }
    Ok(merged)
}

fn lane_occupancy(records: &[RecoveredExecution]) -> Vec<(Address, u64, String)> {
    records
        .iter()
        .filter(|record| record.state.holds_lane())
        .filter_map(|record| {
            record
                .nonce
                .map(|nonce| (record.sender, nonce, record.execution_id.clone()))
        })
        .collect()
}

/// The one derivation §4.2 asks for: a fact list in, a state out, with no room for a
/// conclusion that is not in the facts.
fn state_of(facts: &[JournalFact]) -> RecoveredState {
    let Some(last) = facts.last().copied() else {
        return RecoveredState::Resolved;
    };
    if last.is_terminal() {
        return RecoveredState::Resolved;
    }
    if facts.iter().any(|fact| fact.proves_dispatch()) {
        return RecoveredState::AwaitingReceipt;
    }
    RecoveredState::PossiblyInFlight
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAIN: u64 = 91_342;

    fn record(fact: JournalFact) -> JournalRecord {
        JournalRecord {
            fact,
            chain_id: CHAIN,
            execution_id: "exec-test".to_string(),
            idempotency_key: "key-test".to_string(),
            opportunity_id: "opp-test".to_string(),
            sender: Address::with_last_byte(1),
            target: Address::with_last_byte(2),
            nonce: Some(7),
            transaction_hash: Some(B256::with_last_byte(3)),
            pinned_block: Some(100),
            pinned_block_hash: Some(B256::with_last_byte(4)),
            endpoint_class: None,
            position: None,
            status: None,
            detail: String::new(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "m12f-journal-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp journal dir");
        dir
    }

    fn journal_path(dir: &Path) -> PathBuf {
        dir.join(journal_file_name(CHAIN))
    }

    fn read_lines(dir: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(journal_path(dir)).expect("file");
        text.lines().map(str::to_string).collect()
    }

    fn write_journal(dir: &Path, text: &str) {
        std::fs::write(journal_path(dir), text).expect("write journal");
    }

    /// The derived state of every record, one line each — what a recovery concludes, as
    /// opposed to `JournalRecovery::summary`, which is only what it counts.
    fn record_lines(recovery: &JournalRecovery) -> Vec<String> {
        recovery
            .records
            .iter()
            .map(|record| record.line())
            .collect()
    }

    /// The longest run of hex digits that follows a `0x`, in characters: 64 is a hash, 40 an
    /// address, anything longer is a transaction payload.
    fn longest_hex_run(line: &str) -> usize {
        let bytes = line.as_bytes();
        let mut longest = 0;
        let mut cursor = 0;
        while let Some(offset) = line[cursor..].find("0x") {
            let start = cursor + offset + 2;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            longest = longest.max(end - start);
            cursor = start;
        }
        longest
    }

    #[test]
    fn a_fresh_open_writes_one_line_that_names_the_schema_and_the_chain() {
        let dir = temp_dir("fresh");
        let journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
        assert!(journal.is_durable());
        assert!(journal.recovery().fresh);
        assert_eq!(journal.entries(), 1);
        assert!(
            journal.recovery().records.is_empty(),
            "genesis is not an execution"
        );
        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("\"fact\":\"journal_opened\""),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("\"schema_version\":1"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_append_is_readable_by_a_later_handle_and_never_rewrites_an_earlier_line() {
        let dir = temp_dir("append");
        let first_bytes = {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::SendIntentPersisted))
                .expect("append");
            std::fs::read(journal_path(&dir)).expect("bytes")
        };
        let reopened = ExecutionJournal::open(&dir, CHAIN, 2_000).expect("reopen");
        assert!(!reopened.recovery().fresh);
        assert_eq!(reopened.entries(), 2);
        assert_eq!(reopened.recovery().loaded_entries, 2);
        let after = std::fs::read(journal_path(&dir)).expect("bytes");
        assert!(
            after.starts_with(&first_bytes),
            "the append log was rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_recovered_unknown_holds_its_nonce_and_keeps_its_local_hash_and_lane_identity() {
        let dir = temp_dir("unknown");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            for fact in [
                JournalFact::SendIntentPersisted,
                JournalFact::SendDispatched,
                JournalFact::OutcomeUnknown,
            ] {
                journal.append(1_001, record(fact)).expect("append");
            }
        }
        let recovery = ExecutionJournal::reload(&dir, CHAIN).expect("reload");
        let recovered = recovery.records.first().expect("one record");
        assert_eq!(recovered.state, RecoveredState::AwaitingReceipt);
        assert!(recovered.state.holds_lane());
        assert!(recovered.needs_attention);
        assert_eq!(recovered.last_fact, JournalFact::OutcomeUnknown);
        assert_eq!(recovered.last_fact.basis(), FactBasis::UnconfirmedInference);
        assert_eq!(recovered.transaction_hash, Some(B256::with_last_byte(3)));
        assert_eq!(recovered.nonce, Some(7));
        assert_eq!(
            recovery.lane_occupancy,
            vec![(Address::with_last_byte(1), 7, "exec-test".to_string())]
        );
        assert_eq!(
            recovery
                .by_idempotency_key("key-test")
                .map(|r| r.execution_id.as_str()),
            Some("exec-test")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_intent_without_a_dispatch_line_is_possibly_in_flight_not_definitely_not_sent() {
        let intent = merge(
            &[],
            &JournalEntry::new(1, 1_000, record(JournalFact::SendIntentPersisted)),
        )
        .expect("merge");
        assert_eq!(intent[0].state, RecoveredState::PossiblyInFlight);
        assert!(intent[0].state.holds_lane());
        assert!(!intent[0].dispatched());
        let dispatched = merge(
            &intent,
            &JournalEntry::new(2, 1_001, record(JournalFact::SendDispatched)),
        )
        .expect("merge");
        assert_eq!(dispatched[0].state, RecoveredState::AwaitingReceipt);
        assert!(dispatched[0].dispatched());
        let blocked = merge(
            &intent,
            &JournalEntry::new(3, 1_002, record(JournalFact::ReadyNotSent)),
        )
        .expect("merge");
        assert_eq!(blocked[0].state, RecoveredState::Resolved);
        assert!(!blocked[0].state.holds_lane());
    }

    #[test]
    fn a_receipt_closes_a_record_and_an_acknowledgement_does_not() {
        let mut records = Vec::new();
        for fact in [JournalFact::SendDispatched, JournalFact::EndpointAccepted] {
            records = merge(&records, &JournalEntry::new(1, 1_000, record(fact))).expect("merge");
        }
        assert_eq!(records[0].state, RecoveredState::AwaitingReceipt);
        assert_eq!(lane_occupancy(&records).len(), 1);
        let mut receipt = record(JournalFact::ReceiptObserved);
        receipt.status = Some("included".to_string());
        records = merge(&records, &JournalEntry::new(2, 1_001, receipt)).expect("merge");
        assert_eq!(records[0].state, RecoveredState::Resolved);
        assert_eq!(records[0].receipt_status.as_deref(), Some("included"));
        assert!(lane_occupancy(&records).is_empty());
    }

    #[test]
    fn a_definite_refusal_releases_the_lane_and_an_unknown_never_does() {
        for (fact, holds) in [
            (JournalFact::DefiniteRefusal, false),
            (JournalFact::OutcomeUnknown, true),
        ] {
            let records = merge(
                &merge(
                    &[],
                    &JournalEntry::new(1, 1_000, record(JournalFact::SendDispatched)),
                )
                .expect("merge"),
                &JournalEntry::new(2, 1_001, record(fact)),
            )
            .expect("merge");
            assert_eq!(records[0].state.holds_lane(), holds, "{fact:?}");
            assert_eq!(
                records[0].last_fact, fact,
                "the line written is the line recovered"
            );
            if fact == JournalFact::OutcomeUnknown {
                // §4.3: an unknown answer is never stored, and never recovered, as a refusal.
                assert_ne!(
                    records[0].last_fact,
                    JournalFact::DefiniteRefusal,
                    "an unknown answer must not be rewritten as a definite refusal"
                );
            }
        }
    }

    #[test]
    fn a_torn_last_line_is_a_refusal_that_names_the_tail_and_leaves_the_bytes_alone() {
        let dir = temp_dir("torn");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::SendDispatched))
                .expect("append");
        }
        let text = std::fs::read_to_string(journal_path(&dir)).expect("text");
        let mut lines = text.lines();
        let genesis = lines.next().expect("the opening line");
        let last = lines.next().expect("a record line to cut");
        assert!(lines.next().is_none(), "the fixture is two lines");
        // Cut with the checksum and the newline missing, which is what a process killed
        // mid-write leaves behind. The file then has no trailing newline at all.
        let cut = last.len() - 74;
        assert!(last.is_char_boundary(cut));
        write_journal(&dir, &format!("{genesis}\n{}", &last[..cut]));
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("torn tail refuses");
        let message = error.to_string();
        assert!(message.contains("torn_tail"), "{message}");
        assert!(message.contains("NOT dropped"), "{message}");
        assert_eq!(
            std::fs::read(journal_path(&dir)).expect("bytes").len(),
            genesis.len() + 1 + cut,
            "a refusal must not repair the file by truncating it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_edited_line_fails_its_checksum_instead_of_reading_as_the_edit_says() {
        let dir = temp_dir("checksum");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::OutcomeUnknown))
                .expect("append");
        }
        let text = std::fs::read_to_string(journal_path(&dir)).expect("text");
        let forged = text.replace("not a JSON object", "the lane was released");
        assert_eq!(forged, text, "this edit must not match the genesis line");
        let with_detail = text.replace("\"detail\":\"\"", "\"detail\":\"settled\"");
        assert_ne!(with_detail, text, "the edit has to change a byte");
        write_journal(&dir, &with_detail);
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("checksum refuses");
        assert!(error.to_string().contains("checksum_mismatch"), "{}", error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_newer_schema_is_refused_and_not_reinterpreted_as_an_older_one() {
        let dir = temp_dir("schema");
        write_journal(&dir, "{\"schema_version\":2,\"seq\":0}\n");
        let error = ExecutionJournal::open(&dir, CHAIN, 1_000).expect_err("version refuses");
        assert!(
            error.to_string().contains("unsupported_schema"),
            "{}",
            error
        );
        assert_eq!(
            std::fs::read_to_string(journal_path(&dir)).expect("untouched"),
            "{\"schema_version\":2,\"seq\":0}\n",
            "a refused file is not rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fact_this_build_does_not_define_is_never_skipped() {
        let dir = temp_dir("unknown_fact");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::SendDispatched))
                .expect("append");
        }
        let text = std::fs::read_to_string(journal_path(&dir)).expect("text");
        let edited = text.replace(
            "\"fact\":\"send_dispatched\"",
            "\"fact\":\"nonce_released_by_timeout\"",
        );
        assert_ne!(edited, text, "the edit has to change a byte");
        write_journal(&dir, &edited);
        // The fact word is read before the checksum is recomputed, so the refusal names the
        // undefined fact rather than the stale checksum that the edit also left behind.
        let error =
            ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("an undefined fact refuses");
        assert!(error.to_string().contains("unknown_fact"), "{}", error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_with_lines_but_no_opening_line_is_not_read_further() {
        let dir = temp_dir("no_genesis");
        let line = JournalEntry::new(1, 1_000, record(JournalFact::SendDispatched)).to_line();
        write_journal(&dir, &line);
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("genesis is required");
        assert!(error.to_string().contains("missing_genesis"), "{}", error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_unresolved_executions_in_one_file_are_a_fault_and_not_a_choice() {
        let dir = temp_dir("two_lanes");
        // The file is built directly, because a live handle would refuse the second write
        // before it reached disk (that refusal is the next test's job). What this one covers
        // is the state a *restart* can meet: a journal left by two processes, or by a build
        // that grew a second lane, where recovery would otherwise have to pick a favourite.
        let mut lines = String::new();
        lines.push_str(&JournalEntry::new(0, 1_000, JournalRecord::genesis(CHAIN)).to_line());
        for (index, execution_id) in ["exec-a", "exec-b"].iter().enumerate() {
            let mut intent = record(JournalFact::SendIntentPersisted);
            intent.execution_id = execution_id.to_string();
            intent.idempotency_key = format!("key-{execution_id}");
            intent.position = Some(index);
            intent.transaction_hash = Some(B256::with_last_byte(index as u8 + 10));
            lines.push_str(
                &JournalEntry::new(index as u64 + 1, 1_001 + index as u64, intent).to_line(),
            );
        }
        write_journal(&dir, &lines);
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("one lane, not two");
        assert!(
            error.to_string().contains("two_unresolved_lanes"),
            "{}",
            error
        );
        assert_eq!(
            std::fs::read_to_string(journal_path(&dir)).expect("bytes"),
            lines,
            "a refusal leaves the file exactly as it found it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_live_handle_refuses_a_second_unresolved_execution_before_writing_it() {
        let mut journal = ExecutionJournal::volatile();
        let mut first = record(JournalFact::SendIntentPersisted);
        first.execution_id = "exec-a".to_string();
        journal.append(1_000, first).expect("first intent");
        let mut second = record(JournalFact::SendIntentPersisted);
        second.execution_id = "exec-b".to_string();
        second.idempotency_key = "key-b".to_string();
        let error = journal
            .append(1_001, second)
            .expect_err("second lane refuses");
        assert!(
            error.to_string().contains("two_unresolved_lanes"),
            "{}",
            error
        );
        assert_eq!(journal.lines().len(), 1, "a refused append writes no bytes");
        assert_eq!(journal.next_seq(), 1, "and takes no sequence number");
    }

    #[test]
    fn a_repeated_write_of_one_result_adds_no_line_and_no_record() {
        let mut journal = ExecutionJournal::volatile();
        let written = journal
            .append(1_000, record(JournalFact::SendDispatched))
            .expect("append");
        let again = journal
            .append(1_001, record(JournalFact::SendDispatched))
            .expect("append");
        assert_eq!(written, AppendOutcome::Written { seq: 0 });
        assert_eq!(again, AppendOutcome::Duplicate);
        assert_eq!(journal.lines().len(), 1);
        assert_eq!(journal.appended(), 1);
        assert_eq!(journal.recovery().records.len(), 1);
    }

    #[test]
    fn a_file_that_lost_bytes_under_an_open_handle_refuses_the_next_append() {
        let dir = temp_dir("truncated");
        let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
        journal
            .append(1_001, record(JournalFact::SendDispatched))
            .expect("append");
        write_journal(&dir, "");
        let error = journal
            .append(1_002, record(JournalFact::EndpointAccepted))
            .expect_err("a truncated file is not appended to");
        assert!(
            error.to_string().contains("truncated_externally"),
            "{}",
            error
        );
        assert_eq!(
            std::fs::read_to_string(journal_path(&dir)).expect("empty"),
            ""
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The same fault at the same length: the file was unlinked and written again with the bytes
    /// it had, which is what a copy-and-truncate rotation does. The length watermark cannot see
    /// it — nothing went missing — while every line the handle writes next goes to an inode the
    /// path no longer names, and §9 forbids the run going on with a fact only its memory holds.
    #[test]
    fn a_file_replaced_under_an_open_handle_refuses_the_next_append() {
        let dir = temp_dir("replaced");
        let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
        journal
            .append(1_001, record(JournalFact::SendDispatched))
            .expect("append");
        let path = journal_path(&dir);
        let bytes = std::fs::read(&path).expect("the two lines this handle wrote");
        std::fs::remove_file(&path).expect("unlink");
        std::fs::write(&path, &bytes).expect("replace");
        assert_eq!(
            std::fs::metadata(&path).expect("replaced").len(),
            bytes.len() as u64,
            "the replacement is the same length, so the truncation watermark does not fire"
        );
        let error = journal
            .append(1_002, record(JournalFact::EndpointAccepted))
            .expect_err("a replaced file is not appended to");
        assert!(
            error.to_string().contains("replaced_externally"),
            "{}",
            error
        );
        assert_eq!(
            std::fs::read(&path).expect("the replacement"),
            bytes,
            "and the refusal wrote nothing into the file the path now names, so the run's next \
             line did not silently land on an orphan inode"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_with_no_file_starts_a_journal_and_a_file_that_holds_nothing_refuses_one() {
        // §6's 「不得在检测到损坏后自动创建空台账并继续交易」 as two cases a reader must not
        // confuse: a path that never existed is a first run and gets its opening line; a path that
        // exists and holds nothing is a ledger whose lines were removed, and this build trades on
        // neither reading of it.
        let fresh = temp_dir("no-file");
        assert!(
            !journal_path(&fresh).exists(),
            "a first run has no file that could have been emptied"
        );
        let journal = ExecutionJournal::open(&fresh, CHAIN, 1_000).expect("a first run");
        assert!(journal.recovery().fresh);
        assert!(journal.recovery().records.is_empty());
        assert!(journal.recovery().lane_occupancy.is_empty());
        assert_eq!(journal.next_seq(), 1, "the opening line took sequence 0");
        std::fs::remove_dir_all(&fresh).ok();

        let dir = temp_dir("empty");
        write_journal(&dir, "");
        let error = ExecutionJournal::open(&dir, CHAIN, 1_000)
            .expect_err("a ledger emptied from under a run is not a fresh one");
        assert!(error.to_string().contains("emptied_externally"), "{error}");
        assert_eq!(
            std::fs::read_to_string(journal_path(&dir)).expect("still empty"),
            "",
            "the refusal wrote no opening line, so the file is exactly as the damage left it"
        );
        let error = ExecutionJournal::reload(&dir, CHAIN)
            .expect_err("reading the same file back answers the same way");
        assert!(error.to_string().contains("emptied_externally"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_blank_line_inside_the_file_is_a_fault_rather_than_noise() {
        let dir = temp_dir("blank");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::SendDispatched))
                .expect("append");
        }
        let text = std::fs::read_to_string(journal_path(&dir)).expect("text");
        let mut lines = text.lines();
        let genesis = lines.next().expect("the opening line");
        let record_line = lines.next().expect("the record line");
        assert!(lines.next().is_none(), "the fixture is two lines");
        write_journal(&dir, &format!("{genesis}\n\n{record_line}\n"));
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("blank line refuses");
        assert!(error.to_string().contains("empty_line"), "{}", error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_entry_carries_no_endpoint_url_and_no_signed_bytes() {
        let mut record = record(JournalFact::OutcomeUnknown);
        record.detail = "no answer from eth_sendRawTransaction: the socket closed".to_string();
        record.endpoint_class = Some("unknown".to_string());
        let line = JournalEntry::new(1, 1_000, record).to_line();
        let value: Value = serde_json::from_str(line.trim()).expect("line is json");
        let mut found: Vec<String> = value.as_object().expect("object").keys().cloned().collect();
        let mut expected = vec![
            "basis",
            "chain_id",
            "checksum",
            "detail",
            "endpoint_class",
            "execution_id",
            "fact",
            "idempotency_key",
            "nonce",
            "opportunity_id",
            "pinned_block",
            "pinned_block_hash",
            "position",
            "schema_version",
            "seq",
            "sender",
            "status",
            "target",
            "transaction_hash",
            "written_at_ms",
        ];
        found.sort();
        expected.sort();
        assert_eq!(found, expected, "the field allowlist is the contract");
        let lower = line.to_lowercase();
        for forbidden in ["://", "private_key", "mnemonic", "secret", "authorization"] {
            assert!(
                !lower.contains(forbidden),
                "{forbidden} reached a journal line"
            );
        }
        // Shape rather than word-matching, because the word list cannot do this job: a detail
        // that quotes the RPC method is `eth_sendRawTransaction`, which this build has to be
        // able to write down, while the thing §5 forbids is the signed payload itself. Every
        // value the journal carries is at most a 32-byte hash, so a longer hex run is the
        // payload — and the allowlist above is the field-level half of the same rule.
        assert!(
            longest_hex_run(&line) <= 64,
            "a {}-hex run is a signed payload, not a hash",
            longest_hex_run(&line)
        );
        // The method name the word list would have refused is still allowed, since it is what
        // M12-E's `Unknown` sentences actually say.
        let with_method = line.replace("the socket closed", "eth_sendRawTransaction timed out");
        assert!(
            longest_hex_run(&with_method) <= 64,
            "naming the endpoint's method is not storing its traffic"
        );
        // The shape rule needs its own positive control: a payload has to trip it, or the
        // assertion above proves nothing.
        assert!(
            longest_hex_run(&format!("\"0x{}\"", "ab".repeat(100))) > 64,
            "200 hex characters must read as a payload"
        );
        std::fs::remove_dir_all(temp_dir("unused")).ok();
    }

    #[test]
    fn two_different_hashes_for_one_step_of_one_execution_are_a_conflict() {
        let dir = temp_dir("conflict");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            let mut intent = record(JournalFact::SendIntentPersisted);
            intent.position = Some(0);
            journal.append(1_001, intent).expect("intent");
            let mut dispatched = record(JournalFact::SendDispatched);
            dispatched.position = Some(0);
            dispatched.transaction_hash = Some(B256::with_last_byte(99));
            journal
                .append(1_002, dispatched)
                .expect_err("one step, one payload");
        }
        // The refusal happened live, so the file still holds only the intent it was allowed to
        // write, and a reload says the same thing the handle did.
        let recovery = ExecutionJournal::reload(&dir, CHAIN).expect("reload");
        assert_eq!(
            recovery.records[0].facts,
            vec![JournalFact::SendIntentPersisted]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_steps_of_one_execution_fold_as_two_records_and_only_the_live_one_holds_the_lane() {
        // A route is one execution and several nonces. Folding it as one record would leave
        // §4.3's question — which nonce is still held — unanswerable, so the fold keys on the
        // step as well as the execution, and a settled step stops occupying the lane while the
        // step that never landed keeps occupying it.
        let mut records = Vec::new();
        let mut seq = 0u64;
        for (position, hash, nonce) in [
            (0usize, B256::with_last_byte(20), 7u64),
            (1, B256::with_last_byte(21), 8),
        ] {
            let mut base = record(JournalFact::SendIntentPersisted)
                .with_hash(Some(hash))
                .with_position(Some(position));
            base.nonce = Some(nonce);
            for fact in [
                JournalFact::SendIntentPersisted,
                JournalFact::SendDispatched,
                JournalFact::EndpointAccepted,
            ] {
                records = merge(
                    &records,
                    &JournalEntry::new(seq, 1_000 + seq, base.clone().with_fact(fact)),
                )
                .expect("fold a step");
                seq += 1;
            }
        }
        let settled = record(JournalFact::ReceiptObserved)
            .with_hash(Some(B256::with_last_byte(20)))
            .with_position(Some(0))
            .with_status("included");
        records = merge(&records, &JournalEntry::new(seq, 2_000, settled)).expect("receipt");

        assert_eq!(records.len(), 2, "one record per step");
        assert_eq!(records[0].nonce, Some(7));
        assert_eq!(records[1].nonce, Some(8));
        assert_eq!(records[0].state, RecoveredState::Resolved);
        assert_eq!(records[1].state, RecoveredState::AwaitingReceipt);
        let lane = lane_occupancy(&records);
        assert_eq!(
            lane.len(),
            1,
            "only the step that never landed holds a lane"
        );
        assert_eq!(lane[0].1, 8);

        // The split is per step, not a licence: one step still cannot answer twice about its
        // nonce.
        let mut conflicting = record(JournalFact::OutcomeUnknown).with_position(Some(1));
        conflicting.nonce = Some(99);
        let error = merge(&records, &JournalEntry::new(seq + 1, 3_000, conflicting))
            .expect_err("one step, one nonce");
        assert_eq!(error.name(), "conflicting_identity");
        assert!(error.describe().contains("nonce"), "{}", error.describe());
    }

    #[test]
    fn a_foreign_chain_line_is_refused_rather_than_kept_as_a_second_set_of_rows() {
        let dir = temp_dir("foreign");
        let line = {
            let mut record = record(JournalFact::SendIntentPersisted);
            record.chain_id = 1;
            JournalEntry::new(1, 1_000, record).to_line()
        };
        let genesis = JournalEntry::new(0, 999, JournalRecord::genesis(CHAIN)).to_line();
        write_journal(&dir, &format!("{genesis}{line}"));
        let error = ExecutionJournal::open(&dir, CHAIN, 3_000).expect_err("one file, one chain");
        assert!(error.to_string().contains("foreign_chain"), "{}", error);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_journal_is_read_only_to_open_so_recovering_twice_answers_twice_the_same() {
        let dir = temp_dir("idempotent");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            for fact in [
                JournalFact::SendIntentPersisted,
                JournalFact::SendDispatched,
            ] {
                journal.append(1_001, record(fact)).expect("append");
            }
        }
        let before = std::fs::read(journal_path(&dir)).expect("bytes");
        let first = ExecutionJournal::reload(&dir, CHAIN).expect("reload");
        let _second = ExecutionJournal::open(&dir, CHAIN, 2_000).expect("open again");
        let third = ExecutionJournal::reload(&dir, CHAIN).expect("reload again");
        assert_eq!(
            (first.summary(), record_lines(&first)),
            (third.summary(), record_lines(&third)),
            "the same file answers the same questions"
        );
        assert_eq!(
            std::fs::read(journal_path(&dir)).expect("bytes"),
            before,
            "no open, and no recovery, writes a line to a journal that already has one"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_two_send_facts_that_precede_an_answer_are_distinguishable_in_the_file() {
        let dir = temp_dir("two_facts");
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
            journal
                .append(1_001, record(JournalFact::SendIntentPersisted))
                .expect("F1: persisted, never dispatched");
        }
        let intent_only = ExecutionJournal::reload(&dir, CHAIN).expect("reload");
        assert_eq!(
            intent_only.records[0].state,
            RecoveredState::PossiblyInFlight
        );
        {
            let mut journal = ExecutionJournal::open(&dir, CHAIN, 2_000).expect("reopen");
            journal
                .append(2_001, record(JournalFact::SendDispatched))
                .expect("F3: dispatched, no answer yet");
        }
        let dispatched = ExecutionJournal::reload(&dir, CHAIN).expect("reload");
        assert_eq!(dispatched.records[0].state, RecoveredState::AwaitingReceipt);
        assert_eq!(dispatched.records[0].facts.len(), 2);
        assert_ne!(
            intent_only.records[0].state.name(),
            dispatched.records[0].state.name(),
            "§4.2's two pre-answer facts must not collapse"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_recovery_summary_says_fresh_when_nothing_was_recovered() {
        let dir = temp_dir("summary");
        let journal = ExecutionJournal::open(&dir, CHAIN, 1_000).expect("open");
        let summary = journal.recovery().summary();
        assert!(summary.contains("fresh=true"), "{summary}");
        assert!(summary.contains("records=0"), "{summary}");
        assert!(summary.contains("lane=idle"), "{summary}");
        assert!(!summary.contains("recovered 0 records successfully"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
