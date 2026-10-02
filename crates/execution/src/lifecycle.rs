//! §28/§29/§30/§11/§38: one record per execution attempt, and the rules for moving it.
//!
//! The lifecycle is a ladder of twelve named states. The task book's reason for insisting
//! on it (§28: "不要把所有状态都塞进 bool success") is that each rung is a different
//! *claim*, and a report that says "executed" has to be able to point at the rung it
//! means. `Included` and `Failed` are not two values of one flag; they are two different
//! things the chain told us, with two different latencies attached and two different
//! counters bumped. M7 (§54) added three rungs at the ends that matter: `Preflighted`
//! before anything is built, and `Settled` + `ProfitVerified` after the chain is done —
//! the last one being the rung the milestone's success condition is actually stated on.
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

use alloy_primitives::{Address, B256, I256, U256};
use serde::{Deserialize, Serialize};

use evm_metrics::Metrics;

use crate::error::{ExecutionError, Result};
use crate::intent::TransactionIntent;
use crate::nonce::{NonceAllocator, NonceReading};
use crate::profit::ProfitVerificationStatus;
use crate::receipt::{Receipt, ReceiptStatus};
use crate::submitter::SubmissionOutcome;
use crate::tx::TransactionType;

/// §11's phase-one answer, stated as a number so a second lane is a code change rather
/// than a config value someone can flip.
pub const LANES: usize = 1;

/// §28's status list, extended by §54's three M7 rungs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// The opportunity was detected (M5's trigger).
    Detected,
    /// A simulation run exists for it (M4).
    Simulated,
    /// The risk layer accepted that run (§2's first boundary: this is still not a send).
    RiskApproved,
    /// §54's M7 rung: §26's thirteen checks ran over *this* attempt and passed. It sits
    /// before `Built` on purpose — a transaction nobody re-priced against the live head is
    /// bytes nobody should sign.
    Preflighted,
    /// Unsigned bytes exist ([`crate::builder`]).
    Built,
    /// A signature exists and recovers to the sender ([`crate::signer`]).
    Signed,
    /// The node acknowledged the raw bytes (§2.3: not inclusion).
    Submitted,
    /// A bound receipt with `status == success` exists (§27). Not terminal: §54 puts two
    /// more rungs above it, because an inclusion says the EVM did not revert and says
    /// nothing about what the wallet ended up holding.
    Included,
    /// §54's M7 rung: the before/after snapshots, the flow audit, the route audit, the bill
    /// and §39's equation have all been computed for this attempt.
    Settled,
    /// §54's last rung and "the most important new state in M7": the settlement produced a
    /// *final* profit verdict — [`crate::profit::ProfitVerificationStatus::is_final`],
    /// positive or negative. Which way it went is
    /// [`ExecutionRecord::realized_profit`], not this rung.
    ProfitVerified,
    /// A bound receipt with `status == failure` exists. Not `Failed`: the transaction
    /// ran and reverted, which is a fact about the chain (§P).
    Reverted,
    /// This attempt stopped. The reason is in [`ExecutionRecord::failure`], and it is
    /// always one of §39's taxonomy entries rather than a free-form string.
    Failed,
}

impl ExecutionStatus {
    /// The ladder, in order, with the two stop states last.
    const LADDER: [Self; 10] = [
        Self::Detected,
        Self::Simulated,
        Self::RiskApproved,
        Self::Preflighted,
        Self::Built,
        Self::Signed,
        Self::Submitted,
        Self::Included,
        Self::Settled,
        Self::ProfitVerified,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::Simulated => "simulated",
            Self::RiskApproved => "risk_approved",
            Self::Preflighted => "preflighted",
            Self::Built => "built",
            Self::Signed => "signed",
            Self::Submitted => "submitted",
            Self::Included => "included",
            Self::Settled => "settled",
            Self::ProfitVerified => "profit_verified",
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
    ///
    /// `Included` dropped out of this list in M7 (§54). It used to read as "the chain took
    /// it, end of story"; §19 says a receipt proves only that the EVM did not revert, so
    /// the story continues through settlement to a profit verdict, and a ladder that called
    /// inclusion final had nowhere for those later claims to go.
    pub fn terminal(self) -> bool {
        matches!(self, Self::ProfitVerified | Self::Reverted | Self::Failed)
    }

    /// Whether the attempt may still touch the network. `Signed` is the last rung that
    /// must not be read as "sent" (§2.2).
    pub fn before_submission(self) -> bool {
        matches!(
            self,
            Self::Detected
                | Self::Simulated
                | Self::RiskApproved
                | Self::Preflighted
                | Self::Built
                | Self::Signed
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
    ///
    /// M7 adds two more required predecessors, and they are the point of §54: `Settled`
    /// needs `Included` (nothing to account for until the route is on chain) and
    /// `ProfitVerified` needs `Settled` (no profit claim without the audits behind it).
    /// `Preflighted` deliberately does *not* gate `Built` here — §35's validation
    /// transaction has no opportunity to re-price — so the sequence stage enforces it for
    /// arbitrages, where §26 says it belongs.
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
            Self::Settled => Self::Included,
            Self::ProfitVerified => Self::Settled,
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
    /// How many transactions the totals on this record cover. A single-transaction attempt
    /// says `1`; a route says `N`, and the per-transaction hashes are in
    /// [`crate::sequence::SequenceReport::transactions`]. Without this line, a record whose
    /// `gas_used` is the sum of six receipts reads like the bill of the one hash named above
    /// it, which is exactly the mismatch §55's field list exists to prevent.
    pub route_transactions: Option<usize>,
    pub status: ExecutionStatus,

    pub created_at_ms: u64,
    pub built_at_ms: Option<u64>,
    pub signed_at_ms: Option<u64>,
    pub submitted_at_ms: Option<u64>,
    pub included_at_ms: Option<u64>,
    /// §54's two M7 stamps, so a report can state how long settlement took without
    /// re-reading a log file for it.
    pub settled_at_ms: Option<u64>,
    pub profit_verified_at_ms: Option<u64>,

    pub gas_used: Option<u64>,
    pub effective_gas_price: Option<U256>,
    /// The L1 component of the bill, when a receipt carried it. Stored separately from
    /// `gas_used * effective_gas_price` because they are different bills (see
    /// [`crate::receipt`]).
    pub l1_fee: Option<U256>,

    /// §55's execution-side binding: the block the record's own receipt named, by number
    /// *and* by hash. For a route this is the first transaction's block — the one the
    /// `transaction_hash` above belongs to.
    pub execution_block: Option<u64>,
    pub execution_block_hash: Option<B256>,
    /// §55's asset side, in the route's input asset: what it put in, what it got back, and
    /// the difference before any cost line.
    pub input_asset: Option<Address>,
    pub input_amount: Option<U256>,
    pub gross_output: Option<U256>,
    pub gross_profit: Option<I256>,
    /// §55's cost lines: `l2_fee` over the route's receipts, and `total_fee` = `l2_fee` +
    /// `l1_fee`. §12's subtraction uses the total, so the record has to carry it rather than
    /// let a reader add two columns and hope they match the run.
    pub l2_fee: Option<U256>,
    pub total_fee: Option<U256>,
    /// §12's net after every cost line, or `None` when §14 says the two halves cannot be
    /// added in one denomination. A null here is the honest rendering of "unproven" — a
    /// zero would be a claim.
    pub realized_profit: Option<I256>,
    /// §56's verdict as a field, so "counted as a successful real arbitrage" is a read
    /// rather than an inference from how far up a ladder the status word sits.
    pub profit_status: Option<ProfitVerificationStatus>,

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
            route_transactions: None,
            status,
            created_at_ms,
            built_at_ms: None,
            signed_at_ms: None,
            submitted_at_ms: None,
            included_at_ms: None,
            settled_at_ms: None,
            profit_verified_at_ms: None,
            gas_used: None,
            effective_gas_price: None,
            l1_fee: None,
            execution_block: None,
            execution_block_hash: None,
            input_asset: None,
            input_amount: None,
            gross_output: None,
            gross_profit: None,
            l2_fee: None,
            total_fee: None,
            realized_profit: None,
            profit_status: None,
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
            ExecutionStatus::Settled => self.settled_at_ms = Some(at_ms),
            ExecutionStatus::ProfitVerified => self.profit_verified_at_ms = Some(at_ms),
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
        // §55's execution-side binding comes from the receipt the record just took, and
        // `route_transactions` starts at the one transaction this record is addressed by.
        // A sequence that settles later replaces both through [`Self::attach_outcome`].
        self.execution_block = Some(receipt.block_number);
        self.execution_block_hash = Some(receipt.block_hash);
        self.route_transactions = Some(1);
        self.l2_fee = receipt.l2_cost_wei();
        // No total when the L1 half was never read: `l2 + 0` is a lower bound, and §37's
        // rule that an unmeasured fee is stated as one lives here too.
        self.total_fee = match (self.l2_fee, receipt.l1_fee) {
            (Some(l2), Some(l1)) => l2.checked_add(l1),
            _ => None,
        };
        Ok(from)
    }

    /// §55's outcome lines, folded in once the route has been accounted for.
    ///
    /// The cost fields *replace* what [`ExecutionRecord::attach_receipt`] wrote, and that is
    /// the whole reason this is a separate step rather than a mutation of the receipt
    /// binding: a receipt-bound record carries the one transaction its hash names, while
    /// §55 asks the record for the run's totals. For a single-transaction attempt the two
    /// readings are the same numbers; for a sequence they are not, and a record that kept
    /// the first transaction's bill next to the route's profit would make §12's subtraction
    /// unreadable. The hash and the execution block stay on the first transaction's values
    /// because they *are* what the record is addressed by — [`Self::route_transactions`] is
    /// the line that says so.
    ///
    /// This is a data fold, not a ladder claim: it never moves [`Self::status`]. A route that
    /// landed, lost money and stopped is `Failed` (§34's half-executed arbitrage) *and*
    /// carries a `VerifiedNegative` net (§56's proven loss), because those answer two
    /// different questions — "did the attempt go as planned" and "what did the wallet end
    /// up with" — and collapsing them is how a loss disappears from the record.
    pub fn attach_outcome(&mut self, outcome: &ExecutionOutcome) {
        self.route_transactions = Some(outcome.route_transactions);
        self.input_asset = Some(outcome.input_asset);
        self.input_amount = Some(outcome.input_amount);
        self.gross_output = Some(outcome.gross_output);
        self.gross_profit = Some(outcome.gross_profit);
        self.gas_used = Some(outcome.gas_used);
        self.l2_fee = Some(outcome.l2_fee);
        self.l1_fee = Some(outcome.l1_fee);
        self.total_fee = Some(outcome.total_fee);
        self.realized_profit = outcome.realized_profit;
        self.profit_status = Some(outcome.profit_status);
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
            | ExecutionStatus::Preflighted
            | ExecutionStatus::Built
            | ExecutionStatus::Signed => false,
            ExecutionStatus::Submitted
            | ExecutionStatus::Included
            | ExecutionStatus::Settled
            | ExecutionStatus::ProfitVerified
            | ExecutionStatus::Reverted => true,
            ExecutionStatus::Failed => self.submitted_at_ms.is_some(),
        }
    }

    pub fn has_transaction(&self) -> bool {
        self.transaction_hash.is_some()
    }
}

/// §55's outcome block, as one bundle the settlement hands over.
///
/// It is a struct rather than ten arguments so the record cannot be given a total that was
/// not added from the two halves it also carries: [`ExecutionOutcome::total_fee`] and
/// [`ExecutionOutcome::realized_profit`] arrive together with the `l2_fee` and `l1_fee` they
/// were computed from, and [`ExecutionRecord::attach_outcome`] writes all of them or none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionOutcome {
    /// How many transactions these totals cover. `1` for a lone transaction.
    pub route_transactions: usize,
    pub input_asset: Address,
    pub input_amount: U256,
    /// What the route handed back in its own input asset, measured from the receipts.
    pub gross_output: U256,
    /// `gross_output − input_amount`, before any cost line (§12's first subtraction).
    pub gross_profit: I256,
    /// `Σ gas_used` over the route's receipts.
    pub gas_used: u64,
    pub l2_fee: U256,
    pub l1_fee: U256,
    pub total_fee: U256,
    /// §12's net, or `None` when §14 forbids one denomination.
    pub realized_profit: Option<I256>,
    pub profit_status: ProfitVerificationStatus,
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

    /// Fold §55's outcome lines into the ledger's own copy of the record.
    pub fn attach_outcome(&mut self, execution_id: &str, outcome: &ExecutionOutcome) -> Result<()> {
        let record = self.record_mut(execution_id)?;
        record.attach_outcome(outcome);
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
    pub const SETTLEMENT_LATENCY: &str = "execution_settlement_latency";
    pub const PROFIT_LATENCY: &str = "execution_profit_latency";
    pub const BUILD_SUCCESS: &str = "execution_build_success";
    pub const SIGN_SUCCESS: &str = "execution_sign_success";
    pub const SUBMIT_SUCCESS: &str = "execution_submit_success";
    pub const RECEIPT_SUCCESS: &str = "execution_receipt_success";
    pub const REVERT: &str = "execution_revert";
    /// §54's three new rungs, one counter each: how many attempts were re-priced, accounted
    /// for, and given a final profit verdict — in that order, because the ladder is what
    /// makes the three numbers mean different things.
    pub const PREFLIGHT_SUCCESS: &str = "execution_preflight_success";
    pub const SETTLE_SUCCESS: &str = "execution_settle_success";
    pub const PROFIT_VERIFIED: &str = "execution_profit_verified";
    /// §53's `preflight_latency`. Emitted by [`crate::giwa::LivePreflightReads`], not by
    /// [`meter`]: §26's verdict is a pure decision, so the span worth timing is the set of
    /// chain reads that fed it, and those happen outside any execution record.
    pub const PREFLIGHT_LATENCY: &str = "execution_preflight_latency";
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
    if reached == ExecutionStatus::Preflighted {
        // No latency here on purpose: §26's gate is a pure decision over facts the caller
        // read, so the span worth timing belongs to those reads, which are outside this
        // record. §53's `preflight_latency` is emitted by whoever performs them.
        metrics.bump(metric_keys::PREFLIGHT_SUCCESS);
    }
    if reached == ExecutionStatus::Settled {
        metrics.bump(metric_keys::SETTLE_SUCCESS);
        if let (Some(from), Some(to)) = (record.included_at_ms, record.settled_at_ms) {
            metrics.record_from(metric_keys::SETTLEMENT_LATENCY, from, to);
        }
    }
    if reached == ExecutionStatus::ProfitVerified {
        metrics.bump(metric_keys::PROFIT_VERIFIED);
        if let (Some(from), Some(to)) = (record.settled_at_ms, record.profit_verified_at_ms) {
            metrics.record_from(metric_keys::PROFIT_LATENCY, from, to);
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
            ExecutionStatus::Preflighted,
            ExecutionStatus::Built,
            ExecutionStatus::Signed,
            ExecutionStatus::Submitted,
            ExecutionStatus::Included,
            ExecutionStatus::Settled,
            ExecutionStatus::ProfitVerified,
        ]
        .into_iter()
        .enumerate()
        {
            let at = 1_000 + (index as u64 + 1) * 100;
            record.advance(status, at).expect("the ladder is walkable");
        }
        assert_eq!(record.status, ExecutionStatus::ProfitVerified);
        assert!(record.status.terminal());
        assert!(record.was_sent());
        assert!(record.included_at_ms.is_some());
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::ProfitVerified,
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

    /// §54's point: inclusion is not the end of the claim, and the two rungs after it each
    /// need the one before. A record that reached `ProfitVerified` without passing `Settled`
    /// would be a profit number no audit produced.
    #[test]
    fn settlement_and_the_profit_verdict_have_the_rungs_below_them() {
        assert!(!ExecutionStatus::Included.terminal());
        assert!(!ExecutionStatus::Settled.terminal());
        assert!(ExecutionStatus::ProfitVerified.terminal());
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Included,
            ExecutionStatus::Settled
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Submitted,
            ExecutionStatus::Settled
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Included,
            ExecutionStatus::ProfitVerified
        ));
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Settled,
            ExecutionStatus::ProfitVerified
        ));
        // §26's gate sits before any bytes exist, and `before_submission` has to say so.
        assert!(ExecutionStatus::Preflighted.before_submission());
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::RiskApproved,
            ExecutionStatus::Settled
        ));

        // A half-executed route: the first transaction landed, a later one reverted, so the
        // attempt stopped. `Included` is no longer final, which is what lets the record say
        // `Failed` about the route rather than being frozen at the one rung that oversells it.
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Included,
            ExecutionStatus::Failed
        ));
        let mut record = ExecutionRecord::open(&intent(), ExecutionStatus::Included, 1_000);
        record
            .fail(
                &ExecutionError::TransactionReverted {
                    transaction_hash: "0xabc".to_string(),
                    block_number: 102,
                },
                2_000,
            )
            .unwrap();
        assert_eq!(record.status, ExecutionStatus::Failed);
        // … and a settled one can still fail if the accounting then finds the run
        // unprovable in a way that stops the attempt; a *verified* one cannot.
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Settled,
            ExecutionStatus::Failed
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::ProfitVerified,
            ExecutionStatus::Failed
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Reverted,
            ExecutionStatus::Settled
        ));
    }

    /// §55's outcome lines, and the rule that a route's totals replace the single receipt's
    /// numbers the record was stamped with.
    #[test]
    fn the_record_carries_the_outcome_lines_m7_added() {
        let mut ledger = Ledger::new();
        let Claim::New(handle) = ledger.claim(&intent(), ExecutionStatus::Submitted, 1_000) else {
            panic!()
        };
        let hash = B256::left_padding_from(&[77]);
        ledger
            .attach_transaction_hash(&handle.execution_id, hash)
            .unwrap();
        let mut receipt = receipt_for(hash);
        receipt.gas_used = 100_000;
        receipt.effective_gas_price = U256::from(362u64);
        ledger.attach_receipt(&receipt, 2_000).unwrap();
        ledger
            .advance(&handle.execution_id, ExecutionStatus::Settled, 2_500)
            .unwrap();

        let one_l2 = U256::from(36_200_000u64);
        let one_l1 = U256::from(7_400_000_000u64);
        {
            let record = ledger.get(&handle.execution_id).unwrap();
            assert_eq!(record.execution_block, Some(101));
            assert_eq!(record.route_transactions, Some(1));
            assert_eq!(record.l2_fee, Some(one_l2));
            assert_eq!(record.total_fee, Some(one_l2 + one_l1));
            assert!(record.was_sent());
            // Nothing has been accounted for yet, so the profit lines are absent, not zero.
            assert_eq!(record.realized_profit, None);
            assert_eq!(record.gross_profit, None);
            assert_eq!(record.profit_status, None);
        }

        let input = U256::from(1_000_000_000_000_000u64);
        let output = U256::from(1_002_000_000_000_000u64);
        let gross = output - input;
        let route_l2 = one_l2 * U256::from(6u64);
        let route_l1 = one_l1 * U256::from(6u64);
        let total = route_l2 + route_l1;
        ledger
            .attach_outcome(
                &handle.execution_id,
                &ExecutionOutcome {
                    route_transactions: 6,
                    input_asset: Address::from_slice(&[6u8; 20]),
                    input_amount: input,
                    gross_output: output,
                    gross_profit: I256::from_raw(gross),
                    gas_used: 600_000,
                    l2_fee: route_l2,
                    l1_fee: route_l1,
                    total_fee: total,
                    realized_profit: Some(I256::from_raw(gross - total)),
                    profit_status: ProfitVerificationStatus::VerifiedPositive,
                },
            )
            .unwrap();
        let record = ledger.get(&handle.execution_id).unwrap();
        assert_eq!(record.route_transactions, Some(6));
        assert_eq!(record.gas_used, Some(600_000), "the route total");
        assert_eq!(record.l2_fee, Some(route_l2));
        assert_eq!(record.l1_fee, Some(route_l1));
        assert_eq!(record.total_fee, Some(total));
        assert_eq!(record.input_amount, Some(input));
        assert_eq!(record.gross_output, Some(output));
        assert_eq!(record.gross_profit, Some(I256::from_raw(gross)));
        assert_eq!(record.realized_profit, Some(I256::from_raw(gross - total)));
        // §12's claim, readable straight off the record: the net is the gross minus the bill.
        assert!(record.realized_profit.unwrap() > I256::ZERO);
        assert_eq!(record.execution_block, Some(101));
        assert_eq!(
            record.profit_status,
            Some(ProfitVerificationStatus::VerifiedPositive)
        );
        assert!(record
            .profit_status
            .unwrap()
            .counts_as_successful_real_arbitrage());
        // The JSON a report reads has the lines too.
        let json = serde_json::to_value(record).unwrap();
        for key in [
            "execution_block",
            "execution_block_hash",
            "input_asset",
            "input_amount",
            "gross_output",
            "gross_profit",
            "l2_fee",
            "total_fee",
            "realized_profit",
            "profit_status",
        ] {
            assert!(json.get(key).is_some(), "§55 requires {key}");
        }
        assert_eq!(
            json["profit_status"],
            serde_json::json!("verified_positive")
        );
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
        // Anything unfinished can fail; a verified or reverted one cannot.
        assert!(ExecutionStatus::can_follow(
            ExecutionStatus::Built,
            ExecutionStatus::Failed
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::ProfitVerified,
            ExecutionStatus::Failed
        ));
        assert!(!ExecutionStatus::can_follow(
            ExecutionStatus::Reverted,
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
            assert!(json.get(key).is_some(), "§28 requires {key} in the record");
        }
        // §55's addition, checked the other way round from how M6 checked it: the profit
        // lines now exist on the record, and a fresh one must leave them **null** rather
        // than zero. M6's absence test is superseded — an open attempt has no realized
        // profit, but it now has a place to put one.
        let json = serde_json::to_value(&record).unwrap();
        assert!(json["realized_profit"].is_null());
        assert!(json["gross_profit"].is_null());
        assert!(json["profit_status"].is_null());
    }
}
