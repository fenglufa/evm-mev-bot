//! §28/§29/§30/§11/§38: one record per execution attempt, and the rules for moving it.
//!
//! The lifecycle is a ladder of nine named states. The task book's reason for insisting
//! on it (§28: "不要把所有状态都塞进 bool success") is that each rung is a different
//! *claim*, and a report that says "executed" has to be able to point at the rung it
//! means. `Included` and `Failed` are not two values of one flag; they are two different
//! things the chain told us, with two different latencies attached and two different
//! counters bumped.
//!
//! Three more things live here because they are properties of the ladder rather than of
//! any individual step:
//!
//! * **Idempotency** (§30): the ledger's key is `(opportunity_id, simulation_id,
//!   state_fingerprint)`, so an event loop that fires twice on one state update claims
//!   the same record instead of building a second transaction.
//! * **The single lane** (§11): one nonce allocator, and a rule about when it may be
//!   released — which is where §25's "Unknown is not a failure" becomes operational.
//! * **Metrics** (§38): the four latencies and five counters are derived from record
//!   timestamps, so a metric line and an evidence line can never disagree.
//!
//! Note on the name: [`evm_simulation::ExecutionStatus`] is a *different* type — it
//! describes what happened inside one simulated step. This one describes what happened
//! to a real transaction, and the two are never interchangeable.

use std::collections::BTreeMap;

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use evm_metrics::Metrics;

use crate::error::{ExecutionError, Result};
use crate::intent::TransactionIntent;
use crate::nonce::{NonceAllocator, NonceReading};
use crate::receipt::{Receipt, ReceiptStatus};
use crate::submitter::SubmissionOutcome;
use crate::tx::TransactionType;

/// §11's phase-one answer, stated as a number so a second lane is a code change rather
/// than a config value someone can flip.
pub const LANES: usize = 1;

/// §28's status list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// The opportunity was detected (M5's trigger).
    Detected,
    /// A simulation run exists for it (M4).
    Simulated,
    /// The risk layer accepted that run (§2's first boundary: this is still not a send).
    RiskApproved,
    /// Unsigned bytes exist ([`crate::builder`]).
    Built,
    /// A signature exists and recovers to the sender ([`crate::signer`]).
    Signed,
    /// The node acknowledged the raw bytes (§2.3: not inclusion).
    Submitted,
    /// A bound receipt with `status == success` exists (§27).
    Included,
    /// A bound receipt with `status == failure` exists. Not `Failed`: the transaction
    /// ran and reverted, which is a fact about the chain (§P).
    Reverted,
    /// This attempt stopped. The reason is in [`ExecutionRecord::failure`], and it is
    /// always one of §39's taxonomy entries rather than a free-form string.
    Failed,
}

impl ExecutionStatus {
    /// The ladder, in order, with the two stop states last.
    const LADDER: [Self; 7] = [
        Self::Detected,
        Self::Simulated,
        Self::RiskApproved,
        Self::Built,
        Self::Signed,
        Self::Submitted,
        Self::Included,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Simulated => "simulated",
            Self::RiskApproved => "risk_approved",
            Self::Built => "built",
            Self::Signed => "signed",
            Self::Submitted => "submitted",
            Self::Included => "included",
            Self::Reverted => "reverted",
            Self::Failed => "failed",
        }
    }

    /// Position on the ladder; the stop states sit above every rung they can follow.
    fn rank(self) -> u8 {
        match Self::LADDER.iter().position(|s| *s == self) {
            Some(index) => index as u8,
            None => Self::LADDER.len() as u8,
        }
    }

    /// Whether nothing further can be learned about this attempt.
    pub fn terminal(self) -> bool {
        matches!(self, Self::Included | Self::Reverted | Self::Failed)
    }

    /// Whether the attempt may still touch the network. `Signed` is the last rung that
    /// must not be read as "sent" (§2.2).
    pub fn before_submission(self) -> bool {
        matches!(
            self,
            Self::Detected | Self::Simulated | Self::RiskApproved | Self::Built | Self::Signed
        )
    }

    /// The ladder rule: forward only, never a repeat, never backwards.
    ///
    /// Skipping rungs is allowed at the top of the ladder and forbidden at the bottom.
    /// §35's controlled validation transaction has no simulation and no risk decision,
    /// so its record goes `Detected → Built → Signed → …` — those two rungs are
    /// statements about a *decision*, and a run can legitimately have made one without
    /// the other. What is not allowed is reaching `Signed` without `Built`, `Submitted`
    /// without `Signed`, or `Included` without `Submitted`: those three are statements
    /// about artifacts this crate produced, and a record that claims a send it never
    /// signed is exactly the fabrication §2.2 and §29 exist to make impossible. Walking
    /// back from `Submitted` to `Signed` — which is how a blind retry of a live
    /// transaction gets written — is refused by the same forward-only rule.
    pub fn can_follow(from: Self, to: Self) -> bool {
        if to == Self::Failed {
            return !from.terminal();
        }
        if to == Self::Reverted {
            // A revert needs a receipt, so only an attempt that reached the chain can
            // report one.
            return matches!(from, Self::Submitted | Self::Included);
        }
        let required_predecessor = match to {
            Self::Signed => Self::Built,
            Self::Submitted => Self::Signed,
            Self::Included => Self::Submitted,
            _ => {
                return to.rank() > from.rank();
            }
        };
        from == required_predecessor
    }
}

/// §29's record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub execution_id: String,
    /// §30's key, kept beside the record so the ledger's dedup is auditable.
    pub idempotency_key: String,

    pub opportunity_id: String,
    pub simulation_id: B256,
    pub risk_decision_id: B256,

    pub chain_id: u64,
    pub opportunity_block: u64,
    pub opportunity_block_hash: B256,
    pub state_fingerprint: String,
    /// §34's statement, copied from the intent: what made the sender able to pay. An
    /// evidence file that says `state override: …` is the honest record of a run that
    /// could be built and simulated but never sent, which is what acceptance item T asks
    /// to see rather than infer.
    pub state_funding: String,

    pub sender: Address,
    pub target: Address,
    pub nonce: u64,

    pub transaction_type: TransactionType,
    pub gas_limit: u64,
    pub max_fee_per_gas: Option<U256>,
    pub max_priority_fee_per_gas: Option<U256>,
    pub value_wei: U256,

    pub transaction_hash: Option<B256>,
    pub status: ExecutionStatus,

    pub created_at_ms: u64,
    pub built_at_ms: Option<u64>,
    pub signed_at_ms: Option<u64>,
    pub submitted_at_ms: Option<u64>,
    pub included_at_ms: Option<u64>,

    pub gas_used: Option<u64>,
    pub effective_gas_price: Option<U256>,
    /// The L1 component of the bill, when a receipt carried it. Stored separately from
    /// `gas_used * effective_gas_price` because they are different bills (see
    /// [`crate::receipt`]).
    pub l1_fee: Option<U256>,

    pub simulation_profit_wei: Option<U256>,
    /// `gas_limit * max_fee_per_gas + value`, the bound §33's balance check uses. An
    /// estimate of cost, never a measurement of it — the measurement is `gas_used`, and
    /// realized profit is M7's (§29 explicitly defers it).
    pub estimated_execution_cost_wei: Option<U256>,

    pub failure: Option<String>,
    /// How an attempt stopped without anything failing: §24's endpoint that accepts no
    /// submission, §20's mode that may not broadcast, §26's receipt budget that ran out.
    /// Set by [`ExecutionRecord::block`], and never by [`ExecutionRecord::fail`] — the two
    /// answers mean different things to whoever reads the evidence.
    pub blocked_reason: Option<String>,
}

impl ExecutionRecord {
    /// Open a record for an intent. The record starts wherever the caller says the
    /// attempt has got to — `Detected` for a full lifecycle, or `RiskApproved` when the
    /// intent itself is the evidence that simulation and judgement happened.
    pub fn open(intent: &TransactionIntent, status: ExecutionStatus, created_at_ms: u64) -> Self {
        let idempotency_key = intent.ids.idempotency_key(&intent.state_binding());
        Self {
            execution_id: derive_execution_id(&idempotency_key),
            idempotency_key,
            opportunity_id: intent.ids.opportunity_id.clone(),
            simulation_id: intent.ids.simulation_id,
            risk_decision_id: intent.ids.risk_decision_id,
            chain_id: intent.chain_id,
            opportunity_block: intent.block_number.0,
            opportunity_block_hash: intent.block_hash,
            state_fingerprint: intent.state_fingerprint.clone(),
            state_funding: intent.funding.describe(),
            sender: intent.sender,
            target: intent.target,
            nonce: intent.nonce,
            transaction_type: intent.tx_type,
            gas_limit: intent.gas_limit,
            max_fee_per_gas: intent.max_fee_per_gas,
            max_priority_fee_per_gas: intent.max_priority_fee_per_gas,
            value_wei: intent.value,
            transaction_hash: None,
            status,
            created_at_ms,
            built_at_ms: None,
            signed_at_ms: None,
            submitted_at_ms: None,
            included_at_ms: None,
            gas_used: None,
            effective_gas_price: None,
            l1_fee: None,
            simulation_profit_wei: intent.simulation_profit_wei,
            estimated_execution_cost_wei: intent.unsigned().maximum_cost_wei().ok(),
            failure: None,
            blocked_reason: None,
        }
    }

    /// Move to `to`, stamping the timestamp for the rung reached. Refused (with the §39
    /// taxonomy) when the ladder forbids the move, which is what makes "we signed it"
    /// unreachable without "we built it" first.
    pub fn advance(&mut self, to: ExecutionStatus, at_ms: u64) -> Result<ExecutionStatus> {
        let from = self.status;
        if !ExecutionStatus::can_follow(from, to) {
            return Err(ExecutionError::InvalidIntent(format!(
                "{} → {} is not a legal lifecycle step: a record cannot walk back from a \
                 submission or reach a later rung without its predecessor",
                from.name(),
                to.name()
            )));
        }
        self.status = to;
        match to {
            ExecutionStatus::Built => self.built_at_ms = Some(at_ms),
            ExecutionStatus::Signed => self.signed_at_ms = Some(at_ms),
            ExecutionStatus::Submitted => self.submitted_at_ms = Some(at_ms),
            ExecutionStatus::Included => self.included_at_ms = Some(at_ms),
            ExecutionStatus::Reverted => {
                // A revert is an inclusion with a failed status, so the block stamp is
                // kept — dropping it would lose the fact that the chain did execute it.
                self.included_at_ms = Some(at_ms);
            }
            ExecutionStatus::Failed => {}
            _ => {}
        }
        Ok(from)
    }

    /// Record the hash the bytes we signed have, at the moment they are signed. §27's
    /// binding is only possible if the record holds the local hash rather than a node's.
    pub fn attach_transaction_hash(&mut self, hash: B256) {
        self.transaction_hash = Some(hash);
    }

    /// Fold a receipt into the record after the tracker has bound it (§26/§27).
    pub fn attach_receipt(&mut self, receipt: &Receipt, at_ms: u64) -> Result<ExecutionStatus> {
        if Some(receipt.transaction_hash) != self.transaction_hash {
            return Err(ExecutionError::ReceiptBinding(format!(
                "the record is for {:#x} and the receipt is for {:#x}",
                self.transaction_hash.unwrap_or(B256::ZERO),
                receipt.transaction_hash
            )));
        }
        let to = match receipt.outcome() {
            ReceiptStatus::Reverted => ExecutionStatus::Reverted,
            _ => ExecutionStatus::Included,
        };
        let from = self.status;
        self.advance(to, at_ms)?;
        self.gas_used = Some(receipt.gas_used);
        self.effective_gas_price = Some(receipt.effective_gas_price);
        self.l1_fee = receipt.l1_fee;
        Ok(from)
    }

    /// Stop this attempt, with §39's reason.
    pub fn fail(&mut self, error: &ExecutionError, at_ms: u64) -> Result<ExecutionStatus> {
        let from = self.status;
        self.advance(ExecutionStatus::Failed, at_ms)?;
        self.failure = Some(error.to_string());
        Ok(from)
    }

    /// §24/§53's `BLOCKED`, on the record rather than only in a log line: the attempt
    /// stopped for a reason that is not a broken run — a mode that may not broadcast, an
    /// endpoint that accepts no submission, a receipt budget that ran out — and the record
    /// keeps the rung it honestly reached. Writing those through
    /// [`ExecutionRecord::fail`] would report a sign-only run that did everything its mode
    /// allowed as a failed one, which is the kind of sentence §55 exists to prevent.
    pub fn block(&mut self, reason: &str) {
        self.blocked_reason = Some(reason.to_string());
    }

    /// §2's second boundary, as a question a caller can ask rather than infer.
    ///
    /// `Failed` is not itself an answer: an attempt that failed at the gate was never
    /// sent, and one that failed while waiting for a receipt was. The timestamp is the
    /// evidence, so this reads it rather than the status word.
    pub fn was_sent(&self) -> bool {
        match self.status {
            ExecutionStatus::Detected
            | ExecutionStatus::Simulated
            | ExecutionStatus::RiskApproved
            | ExecutionStatus::Built
            | ExecutionStatus::Signed => false,
            ExecutionStatus::Submitted | ExecutionStatus::Included | ExecutionStatus::Reverted => {
                true
            }
            ExecutionStatus::Failed => self.submitted_at_ms.is_some(),
        }
    }

    pub fn has_transaction(&self) -> bool {
        self.transaction_hash.is_some()
    }
}

/// §30's id: derived, never counted.
///
/// A counter would make the same execution two different ids under replay and live
/// (§49), and would make a restart silently re-issue identities. keccak over the
/// idempotency key gives one id per (opportunity, simulation, state) triple in any run,
/// in any order.
fn derive_execution_id(idempotency_key: &str) -> String {
    let hash = alloy_primitives::keccak256(idempotency_key.as_bytes());
    format!("exec-{}", &hash.to_string()[2..18])
}

/// §30's dedup store, plus §37's loggable index by transaction hash.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    by_key: BTreeMap<String, ExecutionRecord>,
}

/// What a claim on the ledger produced.
///
/// The existing record is boxed because §30's answer to a duplicate claim is the whole
/// record, and an enum whose variants differ by 300 bytes would make every *new* claim
/// pay for a record it does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// This triple has never been seen; the record is new and the caller may proceed.
    New(ExecutionId),
    /// It has been seen. The caller must not build a second transaction, and gets the
    /// existing status so it can decide whether to wait for a receipt or stop.
    Existing(Box<ExecutionRecord>),
}

/// A lightweight handle: the id plus the key it was claimed under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionId {
    pub execution_id: String,
    pub idempotency_key: String,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// §30: claim the triple, or take the record that already owns it. This is the only
    /// way a record enters the ledger, which is what makes "two transactions from one
    /// state update" a type error rather than a race.
    pub fn claim(
        &mut self,
        intent: &TransactionIntent,
        status: ExecutionStatus,
        at_ms: u64,
    ) -> Claim {
        let record = ExecutionRecord::open(intent, status, at_ms);
        match self.by_key.get(&record.idempotency_key) {
            Some(existing) => Claim::Existing(Box::new(existing.clone())),
            None => {
                let handle = ExecutionId {
                    execution_id: record.execution_id.clone(),
                    idempotency_key: record.idempotency_key.clone(),
                };
                self.by_key.insert(record.idempotency_key.clone(), record);
                Claim::New(handle)
            }
        }
    }

    pub fn get(&self, execution_id: &str) -> Option<&ExecutionRecord> {
        self.by_key
            .values()
            .find(|r| r.execution_id == execution_id)
    }

    /// §37: find the record for a transaction hash, which is the only join a log line
    /// needs once a submission has happened.
    pub fn by_transaction(&self, transaction_hash: B256) -> Option<&ExecutionRecord> {
        self.by_key
            .values()
            .find(|r| r.transaction_hash == Some(transaction_hash))
    }

    /// The ledger's own lookup: a record addressed by its execution id, or the §39 error
    /// naming the id that was not there. Every mutating step goes through this so an
    /// unknown id fails the same way in all of them.
    fn record_mut(&mut self, execution_id: &str) -> Result<&mut ExecutionRecord> {
        self.by_key
            .values_mut()
            .find(|r| r.execution_id == execution_id)
            .ok_or_else(|| {
                ExecutionError::InvalidIntent(format!("the ledger has no execution {execution_id}"))
            })
    }

    /// Apply a step to a claimed record, returning the status it moved off.
    pub fn advance(
        &mut self,
        execution_id: &str,
        to: ExecutionStatus,
        at_ms: u64,
    ) -> Result<ExecutionStatus> {
        self.record_mut(execution_id)?.advance(to, at_ms)
    }

    /// Record the local hash on the ledger's own copy of the record.
    ///
    /// This is the step that makes `by_transaction` and [`Ledger::attach_receipt`]
    /// reachable from outside the crate: both join on the hash, so a caller that could
    /// only set it on a local clone would write evidence in which every submitted record
    /// still says "no transaction".
    pub fn attach_transaction_hash(&mut self, execution_id: &str, hash: B256) -> Result<()> {
        let record = self.record_mut(execution_id)?;
        record.attach_transaction_hash(hash);
        Ok(())
    }

    /// Stop a claimed attempt in the ledger, with §39's reason.
    pub fn fail(
        &mut self,
        execution_id: &str,
        error: &ExecutionError,
        at_ms: u64,
    ) -> Result<ExecutionStatus> {
        self.record_mut(execution_id)?.fail(error, at_ms)
    }

    /// Record a stop that is not a failure on the ledger's own copy (§24, §20, §26).
    pub fn block(&mut self, execution_id: &str, reason: &str) -> Result<()> {
        self.record_mut(execution_id)?.block(reason);
        Ok(())
    }

    /// Attach a bound receipt to the record that owns the hash (§27's join).
    pub fn attach_receipt(&mut self, receipt: &Receipt, at_ms: u64) -> Result<Option<String>> {
        let key = match self
            .by_key
            .iter()
            .find(|(_, r)| r.transaction_hash == Some(receipt.transaction_hash))
            .map(|(key, _)| key.clone())
        {
            Some(key) => key,
            None => return Ok(None),
        };
        let record = self
            .by_key
            .get_mut(&key)
            .expect("the key just came from the map");
        record.attach_receipt(receipt, at_ms)?;
        Ok(Some(record.execution_id.clone()))
    }

    pub fn records(&self) -> impl Iterator<Item = &ExecutionRecord> {
        self.by_key.values()
    }

    /// §53/§59's evidence shape: every record, in idempotency-key order so two runs of
    /// the same events produce the same bytes.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.by_key
                .values()
                .map(|record| serde_json::to_value(record).expect("a record serializes"))
                .collect(),
        )
    }
}

/// §11: the one execution lane.
///
/// A lane is a nonce allocator plus the rule for when it lets go. `LANES == 1` is the
/// milestone's answer to "what happens to the second transaction when the first times
/// out": there is no second transaction, so the question does not arise, and the
/// allocator holds the nonce until a *read* resolves the attempt.
#[derive(Clone, Debug, Default)]
pub struct ExecutionLane {
    nonce: NonceAllocator,
}

/// What happened to the lane after a submission answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneRelease {
    Released,
    /// Still holding the nonce, with the reason a human needs (§25).
    Held {
        reason: String,
    },
}

impl ExecutionLane {
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of lanes this build supports. A caller that wants a second concurrent
    /// execution must fail here rather than somewhere in the mempool.
    pub fn capacity() -> usize {
        LANES
    }

    pub fn is_idle(&self) -> bool {
        self.nonce.is_free()
    }

    pub fn outstanding(&self) -> Option<(Address, u64)> {
        self.nonce.outstanding()
    }

    /// Take the lane for the nonce `reading` offers. Refused when another execution
    /// already owns it — the §11 phase-one rule, expressed as the one place a nonce can
    /// be minted. The reading has to come from the endpoint: a lane fed a number someone
    /// typed in is a lane with no policy.
    pub fn allocate(&mut self, reading: &NonceReading) -> Result<u64> {
        if let Some((held, held_nonce)) = self.nonce.outstanding() {
            return Err(ExecutionError::NonceUnavailable(format!(
                "the single execution lane is holding nonce {held_nonce} for {held}; §11 gives \
                 this build one lane"
            )));
        }
        self.nonce.allocate(reading)
    }

    /// §25's rule, applied to the lane: only a definite refusal releases it. An `Unknown`
    /// answer holds the nonce until a receipt or a re-read says otherwise, because a
    /// second transaction on the same nonce would be a duplicate of a possibly-live one.
    pub fn resolve_submission(&mut self, outcome: &SubmissionOutcome) -> LaneRelease {
        let Some((address, nonce)) = self.outstanding() else {
            return LaneRelease::Released;
        };
        if outcome.proven_not_in_flight() {
            return match self.nonce.release(address, nonce) {
                Ok(()) => LaneRelease::Released,
                Err(error) => LaneRelease::Held {
                    reason: error.to_string(),
                },
            };
        }
        LaneRelease::Held {
            reason: format!(
                "the submission answer was `{}`; §25 forbids treating that as a failure, so \
                 nonce {nonce} stays reserved for {address} until a receipt or a re-read \
                 resolves it",
                outcome.status_word()
            ),
        }
    }

    /// A terminal receipt releases the lane; a timeout does not (§26).
    pub fn resolve_receipt(&mut self, status: ReceiptStatus) -> LaneRelease {
        let Some((address, nonce)) = self.outstanding() else {
            return LaneRelease::Released;
        };
        if !status.may_be_in_flight() {
            return match self.nonce.release(address, nonce) {
                Ok(()) => LaneRelease::Released,
                Err(error) => LaneRelease::Held {
                    reason: error.to_string(),
                },
            };
        }
        LaneRelease::Held {
            reason: format!(
                "the receipt status was `{}`; the transaction may still land, so nonce {nonce} \
                 stays reserved",
                status.name()
            ),
        }
    }

    /// The lane after an attempt that stopped *before* handing anything to a node.
    ///
    /// §25's rule is about a submission answer, so it does not reach this case: a build
    /// that refused, a signature the mode forbade, and a gate that blocked all leave the
    /// nonce unspent on the chain, and holding it would make one refused attempt block
    /// every later one. The method is named for the fact it encodes rather than for the
    /// caller's convenience, because the opposite call — releasing a lane after an
    /// `Unknown` submission — is the bug §25 exists to prevent.
    pub fn release_unsent(&mut self) -> LaneRelease {
        let Some((address, nonce)) = self.outstanding() else {
            return LaneRelease::Released;
        };
        match self.nonce.release(address, nonce) {
            Ok(()) => LaneRelease::Released,
            Err(error) => LaneRelease::Held {
                reason: error.to_string(),
            },
        }
    }
}

/// §38's metric keys, spelled once.
pub mod metric_keys {
    pub const BUILD_LATENCY: &str = "execution_build_latency";
    pub const SIGN_LATENCY: &str = "execution_sign_latency";
    pub const SUBMISSION_LATENCY: &str = "execution_submission_latency";
    pub const RECEIPT_LATENCY: &str = "execution_receipt_latency";
    pub const BUILD_SUCCESS: &str = "execution_build_success";
    pub const SIGN_SUCCESS: &str = "execution_sign_success";
    pub const SUBMIT_SUCCESS: &str = "execution_submit_success";
    pub const RECEIPT_SUCCESS: &str = "execution_receipt_success";
    pub const REVERT: &str = "execution_revert";
}

/// Emit §38's metrics for one transition.
///
/// Called with the *previous* status the record had, so a counter is bumped exactly once
/// per rung reached and a latency is only recorded when both stamps exist. This function
/// reads nothing but the record, which is what lets the report and the metric line agree.
pub fn meter(metrics: &mut Metrics, previous: Option<ExecutionStatus>, record: &ExecutionRecord) {
    // Only the rung just reached earns a counter, so a repeated call on the same status
    // cannot double-count.
    let reached = match previous {
        Some(previous) if previous == record.status => return,
        _ => record.status,
    };
    if reached == ExecutionStatus::Built {
        metrics.bump(metric_keys::BUILD_SUCCESS);
        if let (Some(created), Some(built)) = (Some(record.created_at_ms), record.built_at_ms) {
            metrics.record_from(metric_keys::BUILD_LATENCY, created, built);
        }
    }
    if reached == ExecutionStatus::Signed {
        metrics.bump(metric_keys::SIGN_SUCCESS);
        if let (Some(from), Some(to)) = (record.built_at_ms, record.signed_at_ms) {
            metrics.record_from(metric_keys::SIGN_LATENCY, from, to);
        }
    }
    if reached == ExecutionStatus::Submitted {
        metrics.bump(metric_keys::SUBMIT_SUCCESS);
        if let (Some(from), Some(to)) = (record.signed_at_ms, record.submitted_at_ms) {
            metrics.record_from(metric_keys::SUBMISSION_LATENCY, from, to);
        }
    }
    match reached {
        ExecutionStatus::Included => {
            metrics.bump(metric_keys::RECEIPT_SUCCESS);
            if let (Some(from), Some(to)) = (record.submitted_at_ms, record.included_at_ms) {
                metrics.record_from(metric_keys::RECEIPT_LATENCY, from, to);
            }
        }
        ExecutionStatus::Reverted => {
            metrics.bump(metric_keys::REVERT);
            if let (Some(from), Some(to)) = (record.submitted_at_ms, record.included_at_ms) {
                metrics.record_from(metric_keys::RECEIPT_LATENCY, from, to);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm_core::BlockNumber;
    use evm_simulation::BlockPin;

    fn intent() -> TransactionIntent {
        TransactionIntent::validation(
            BlockPin::new(BlockNumber(37_191_169), B256::left_padding_from(&[7])),
            Address::from_slice(&[1u8; 20]),
            &crate::tx::UnsignedTransaction {
                tx_type: TransactionType::DynamicFee,
                chain_id: 91_342,
                nonce: 0,
                to: Some(Address::from_slice(&[2u8; 20])),
                value: U256::from(7u64),
                gas_limit: 21_000,
                input: alloy_primitives::Bytes::from(vec![0u8; 4]),
                access_list: Vec::new(),
                max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
                max_fee_per_gas: Some(U256::from(1_000_370u64)),
            },
        )
        .expect("a validation intent over a call transaction")
    }

    /// A receipt shaped like the one measured in
    /// `data/evidence/m6/probe-submission-surface.txt`, with the hash the record holds.
    fn receipt_for(hash: B256) -> Receipt {
        Receipt {
            transaction_hash: hash,
            block_number: 101,
            block_hash: B256::left_padding_from(&[101]),
            transaction_index: 3,
            success: true,
            gas_used: 21_000,
            effective_gas_price: U256::from(1_000_370u64),
            cumulative_gas_used: Some(U256::from(84_000u64)),
            from: Address::from_slice(&[1u8; 20]),
            to: Some(Address::from_slice(&[2u8; 20])),
            contract_address: None,
            tx_type: Some(2),
            logs: Vec::new(),
            l1_fee: Some(U256::from(7_400_000_000u64)),
            l1_gas_price: Some(U256::from(1_088_519_061u64)),
            l1_gas_used: Some(U256::from(1_600u64)),
            l1_base_fee_scalar: Some(U256::from(1_368u64)),
            l1_blob_base_fee: Some(U256::from(62_294_004u64)),
            l1_blob_base_fee_scalar: Some(U256::from(801_949u64)),
            provenance: "eth_getTransactionReceipt".to_string(),
        }
    }

    fn reading(address: Address, confirmed: u64, pending: u64) -> NonceReading {
        NonceReading {
            address,
            confirmed,
            pending,
            at_block: 100,
            source: "test read".to_string(),
        }
    }

    /// `LatencyTable::stats` is an array of rows, so a named lookup belongs to the test
    /// rather than to the metric API.
    fn min_ms(metrics: &Metrics, name: &str) -> u64 {
        metrics
            .latency
            .stats()
            .as_array()
            .expect("a table of rows")
            .iter()
            .find(|row| row["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("no latency series named {name}"))["stats"]["min_ms"]
            .as_u64()
            .expect("a millisecond figure")
    }

    #[test]
    fn the_ladder_forward_only_and_a_signed_record_was_never_sent() {
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Detected, 1_000);
        for (index, status) in [
            ExecutionStatus::Simulated,
            ExecutionStatus::RiskApproved,
            ExecutionStatus::Built,
            ExecutionStatus::Signed,
            ExecutionStatus::Submitted,
            ExecutionStatus::Included,
        ]
        .into_iter()
        .enumerate()
        {
            let at = 1_000 + (index as u64 + 1) * 100;
            record.advance(status, at).expect("the ladder is walkable");
        }
        assert_eq!(record.status, ExecutionStatus::Included);
        assert!(record.was_sent());
        assert!(record.included_at_ms.is_some());
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Included,
            ExecutionStatus::Signed
        ));

        // §2.2: the rung after signing is still not a send.
        let mut signed = ExecutionRecord::open(&intent(), ExecutionStatus::Detected, 1_000);
        signed.advance(ExecutionStatus::Built, 1_100).unwrap();
        signed.advance(ExecutionStatus::Signed, 1_200).unwrap();
        assert!(!signed.was_sent());
        assert!(!signed.has_transaction());
        assert!(signed.status.before_submission());
    }

    #[test]
    fn illegal_steps_are_refused_with_the_pair_that_was_tried() {
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Detected, 1_000);
        // No build without a prior rung is allowed to skip to Signed.
        let error = record
            .advance(ExecutionStatus::Submitted, 2_000)
            .unwrap_err();
        assert!(
            error.to_string().contains("detected → submitted"),
            "{error}"
        );
        // A walk back from Submitted is the retry trap; it must be impossible.
        let mut sent = ExecutionRecord::open(&intent(), ExecutionStatus::Submitted, 1_000);
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Submitted,
            ExecutionStatus::Signed
        ));
        assert!(sent
            .advance(ExecutionStatus::Signed, 900)
            .unwrap_err()
            .to_string()
            .contains("not a legal lifecycle step"));
        // A revert requires the chain to have run it.
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Signed,
            ExecutionStatus::Reverted
        ));
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Submitted,
            ExecutionStatus::Reverted
        ));
        // Anything unfinished can fail; a finished thing cannot.
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Built,
            ExecutionStatus::Failed
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Included,
            ExecutionStatus::Failed
        ));
        let _ = error;
    }

    #[test]
    fn one_triple_claims_once_and_a_second_claim_returns_the_existing_record() {
        let mut ledger = Ledger::new();
        let first = ledger.claim(&intent(), ExecutionStatus::Detected, 1_000);
        let Claim::New(handle) = &first else {
            panic!("the first claim must be new")
        };
        assert!(handle.execution_id.starts_with("exec-"));
        assert_eq!(ledger.len(), 1);

        let again = ledger.claim(&intent(), ExecutionStatus::Detected, 2_000);
        let Claim::Existing(record) = again else {
            panic!(
                "§30: the same (opportunity, simulation, state) triple must not be claimed twice"
            )
        };
        assert_eq!(record.created_at_ms, 1_000, "the original attempt wins");
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            record.execution_id, handle.execution_id,
            "an id derived from the triple must be stable across claims"
        );
    }

    #[test]
    fn a_different_state_fingerprint_is_a_different_execution() {
        let mut ledger = Ledger::new();
        let a = intent();
        let mut b = intent();
        b.state_fingerprint = String::from("a different state version");
        let _ = ledger.claim(&a, ExecutionStatus::Detected, 1);
        match ledger.claim(&b, ExecutionStatus::Detected, 2) {
            Claim::New(_) => {}
            Claim::Existing(_) => panic!("a new state version is a new execution"),
        }
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn a_receipt_joins_by_hash_and_its_status_decides_included_or_reverted() {
        let mut ledger = Ledger::new();
        let Claim::New(handle) = ledger.claim(&intent(), ExecutionStatus::Submitted, 1_000) else {
            panic!()
        };
        let hash = B256::left_padding_from(&[42]);
        ledger
            .attach_transaction_hash(&handle.execution_id, hash)
            .unwrap();

        let receipt = receipt_for(hash);
        let id = ledger.attach_receipt(&receipt, 2_000).unwrap();
        assert_eq!(id, Some(handle.execution_id.clone()));
        let record = ledger.by_transaction(hash).expect("the join works");
        assert_eq!(record.status, ExecutionStatus::Included);
        assert_eq!(record.gas_used, Some(21_000));
        assert_eq!(record.l1_fee, Some(U256::from(7_400_000_000u64)));
        assert_eq!(record.effective_gas_price, Some(U256::from(1_000_370u64)));

        // A second receipt for the same hash cannot re-decide the attempt: the record is
        // already terminal, and an already-decluded execution reverting would be a lie
        // about what the chain did.
        assert!(ledger.attach_receipt(&receipt, 5_000).is_err());

        let other_hash = B256::left_padding_from(&[43]);
        let mut reverted = receipt_for(other_hash);
        reverted.success = false;
        let Claim::New(second) = ledger.claim(
            &{
                let mut i = intent();
                i.state_fingerprint = String::from("a different state version");
                i
            },
            ExecutionStatus::Submitted,
            3_000,
        ) else {
            panic!()
        };
        ledger
            .attach_transaction_hash(&second.execution_id, other_hash)
            .unwrap();
        ledger.attach_receipt(&reverted, 4_000).unwrap();
        let record = ledger.by_transaction(other_hash).unwrap();
        assert_eq!(record.status, ExecutionStatus::Reverted);
        // A revert still has a block stamp: the chain executed it (§P).
        assert_eq!(record.included_at_ms, Some(4_000));
        // And a receipt for a transaction the ledger has never heard of is `Ok(None)`,
        // not a fabricated record.
        let stranger = receipt_for(B256::left_padding_from(&[44]));
        assert_eq!(ledger.attach_receipt(&stranger, 9_000).unwrap(), None);
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn the_lane_holds_a_nonce_through_an_unknown_answer_and_releases_on_a_refusal() {
        let mut lane = ExecutionLane::new();
        assert!(lane.is_idle());
        let address = Address::from_slice(&[1u8; 20]);
        assert_eq!(
            lane.allocate(&reading(address, 4, 4)).unwrap(),
            4,
            "with nothing in flight the pending view is the nonce"
        );
        assert!(!lane.is_idle());
        assert_eq!(lane.outstanding(), Some((address, 4)));
        assert_eq!(ExecutionLane::capacity(), 1);
        assert!(matches!(
            lane.allocate(&reading(address, 5, 5)),
            Err(ExecutionError::NonceUnavailable(_))
        ));

        let unknown = SubmissionOutcome::Unknown {
            reason: "connection reset".to_string(),
            endpoint: crate::submitter::EndpointKind::PublicHttpRpc,
        };
        assert!(matches!(
            lane.resolve_submission(&unknown),
            LaneRelease::Held { .. }
        ));
        assert!(!lane.is_idle(), "§25: an unknown answer is not a release");

        let refused = SubmissionOutcome::Rejected {
            reason: "nonce too low".to_string(),
            endpoint: crate::submitter::EndpointKind::PublicHttpRpc,
        };
        assert_eq!(lane.resolve_submission(&refused), LaneRelease::Released);
        assert!(lane.is_idle());
        // An idle lane has nothing to release, and says so without inventing an error.
        assert_eq!(lane.resolve_submission(&refused), LaneRelease::Released);
    }

    #[test]
    fn a_receipt_timeout_keeps_the_lane_and_a_terminal_receipt_frees_it() {
        let mut lane = ExecutionLane::new();
        let address = Address::from_slice(&[9u8; 20]);
        lane.allocate(&reading(address, 1, 1)).unwrap();
        for busy in [
            ReceiptStatus::Submitted,
            ReceiptStatus::Pending,
            ReceiptStatus::Timeout,
        ] {
            let LaneRelease::Held { .. } = lane.resolve_receipt(busy) else {
                panic!("{} must keep the lane", busy.name())
            };
        }
        assert_eq!(
            lane.resolve_receipt(ReceiptStatus::Included),
            LaneRelease::Released
        );
        assert!(lane.is_idle());
    }

    #[test]
    fn each_rung_bumps_its_counter_once_and_only_measurable_latencies_are_recorded() {
        let mut metrics = Metrics::default();
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Detected, 1_000);
        let steps = [
            (ExecutionStatus::Built, 1_500u64),
            (ExecutionStatus::Signed, 1_600),
            (ExecutionStatus::Submitted, 1_650),
            (ExecutionStatus::Included, 2_650),
        ];
        for (status, at) in steps {
            let previous = record.advance(status, at).unwrap();
            meter(&mut metrics, Some(previous), &record);
            // A second call with the same status must not double-count.
            meter(&mut metrics, Some(status), &record);
        }
        assert_eq!(metrics.get(metric_keys::BUILD_SUCCESS), 1);
        assert_eq!(metrics.get(metric_keys::SIGN_SUCCESS), 1);
        assert_eq!(metrics.get(metric_keys::SUBMIT_SUCCESS), 1);
        assert_eq!(metrics.get(metric_keys::RECEIPT_SUCCESS), 1);
        assert_eq!(metrics.get(metric_keys::REVERT), 0);
        assert_eq!(metrics.latency.count(metric_keys::BUILD_LATENCY), 1);
        assert_eq!(
            min_ms(&metrics, metric_keys::BUILD_LATENCY),
            500,
            "detected → built"
        );
        assert_eq!(min_ms(&metrics, metric_keys::SIGN_LATENCY), 100);
        assert_eq!(min_ms(&metrics, metric_keys::SUBMISSION_LATENCY), 50);
        assert_eq!(
            min_ms(&metrics, metric_keys::RECEIPT_LATENCY),
            1_000,
            "submission → receipt is the block-landing wait"
        );

        // A revert path: the receipt counter stays, the revert counter appears.
        let mut metrics = Metrics::default();
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Submitted, 1_000);
        record.submitted_at_ms = Some(1_000);
        let previous = record.advance(ExecutionStatus::Reverted, 3_000).unwrap();
        meter(&mut metrics, Some(previous), &record);
        assert_eq!(metrics.get(metric_keys::REVERT), 1);
        assert_eq!(metrics.get(metric_keys::RECEIPT_SUCCESS), 0);
    }

    #[test]
    fn a_failure_records_the_taxonomy_entry_rather_than_a_string_of_convenience() {
        let mut metrics = Metrics::default();
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Built, 1_000);
        let previous = record
            .fail(
                &ExecutionError::InsufficientBalance("0 wei".to_string()),
                2_000,
            )
            .unwrap();
        meter(&mut metrics, Some(previous), &record);
        assert_eq!(record.status, ExecutionStatus::Failed);
        assert!(record
            .failure
            .as_deref()
            .unwrap()
            .starts_with("insufficient balance"));
        assert!(!record.was_sent());
        assert_eq!(
            metrics.get(metric_keys::BUILD_SUCCESS),
            0,
            "a rung already passed does not get re-counted by a later failure"
        );
    }

    #[test]
    fn the_record_carries_the_identity_triple_that_makes_a_log_line_actionable() {
        let record = ExecutionRecord::open(&intent(), ExecutionStatus::Detected, 1_000);
        // §37's four ids, all present, all derived rather than typed in.
        assert_eq!(record.opportunity_id, "m6-execution-validation-transaction");
        assert_ne!(record.simulation_id, B256::ZERO);
        assert_ne!(record.risk_decision_id, B256::ZERO);
        assert_ne!(record.execution_id, record.idempotency_key);
        let json = serde_json::to_value(&record).unwrap();
        for key in [
            "execution_id",
            "opportunity_id",
            "simulation_id",
            "risk_decision_id",
            "chain_id",
            "opportunity_block",
            "opportunity_block_hash",
            "sender",
            "target",
            "nonce",
            "status",
            "created_at_ms",
            "gas_limit",
            "estimated_execution_cost_wei",
            "simulation_profit_wei",
        ] {
            assert!(json.get(key).is_some(), "§29 requires {key} in the record");
        }
        // §29's deferral, checked as an absence: M6 must not claim a realized profit.
        assert!(json.get("realized_profit").is_none());
    }
}
