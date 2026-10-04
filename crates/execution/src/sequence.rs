//! §16/§18–§24/§54: a route is a sequence of transactions, and its evidence is too.
//!
//! M6 could only build one transaction per intent, because §4 forbids the executor contract
//! that would make a two-pool cycle atomic. M7 keeps that ban and pays for it differently:
//! the cycle is carried by N real transactions from one EOA, sent in order on one nonce lane
//! ([`crate::lifecycle::ExecutionLane`], capacity 1, §33). This module owns that shape — the
//! plan, the serial send, and the four audits a single-transaction run never needed:
//!
//! * **§18/§47 — delta audit.** The simulation said what it would produce; the receipts say
//!   what it did produce. Every pair is a [`DeltaLine`], and a pair outside the tolerance the
//!   caller named is [`ExecutionError::ExecutionMismatch`] — not a rounding, not a retry.
//! * **§19–§21 — assets, not status.** `status = 1` proves the EVM did not revert. The
//!   wallet's native and `balanceOf` reads before and after do, and they are pinned to blocks
//!   by hash because §20 forbids accounting against "latest".
//! * **§21/§22 — logs cross-check balances.** A `balanceOf` delta is final state and a
//!   `Transfer` log is what moved; when the two disagree about the same token, [`FlowCheck`]
//!   says so instead of picking the convenient one. The wrap and the unwrap of §16's shape are
//!   part of the log side too, because the token contract mints and burns without a `Transfer`.
//! * **§23 — the route, from the pools' own logs.** Each venue named by the plan must have
//!   emitted a `Swap` itself, in leg order. "It paid me" is not "both legs ran".
//!
//! The record boundary is worth stating plainly: [`crate::lifecycle::Ledger`] holds **one**
//! record per attempt (that is §30's key: opportunity, simulation, state), and a record has
//! one `transaction_hash` and a forward-only ladder — so the record is stamped from the first
//! transaction that reaches each rung, and every step's own hash, receipt and bill lives in
//! [`SequenceReport::transactions`], which is what the cost and profit evidence is computed
//! from.
//!
//! Nothing here decides whether to send. A step refuses for the same reasons
//! [`crate::stage::ExecutionStage`] refuses — mode, gate, codec, lane — and a failed step
//! stops the sequence. The half-route left on chain is then accounted for at its real value
//! (§34), which is why the before-snapshot is read before step 0 rather than after the last.

use std::collections::BTreeMap;
use std::sync::Arc;

use alloy_primitives::{Address, B256, I256, U256};
use async_trait::async_trait;

use evm_chain::{ChainLog, RpcTraceSink};
use evm_core::{BlockNumber, ChainId};
use evm_metrics::{Clock, Metrics, Stage};
use evm_protocol::signatures::{topic_address, word, V2Topics, Weth9Topics};
use evm_simulation::{Binding, ExecutedStep, SimulationResult};

use crate::block_context::{consumer_check, BlockIdentity, ContextOutcome, VerifiedBlockContext};
use crate::builder::{GasPolicy, TransactionBuilder};
use crate::chain_read::read_binding;
use crate::cost::{ExecutionCostEvidence, SequenceCost};
use crate::error::{ExecutionError, Result};
use crate::evidence::{SignedTransactionEvidence, SubmissionEvidence};
use crate::fee::FeeSource;
use crate::gate::{
    BalanceEvidence, BlockBinding, GateAttempt, GateFacts, NonceEvidence, PreSubmitGate,
};
use crate::intent::{SenderFunding, SequencePosition, TransactionIntent};
use crate::lifecycle::{
    meter, Claim, ExecutionLane, ExecutionOutcome, ExecutionStatus, LaneRelease, Ledger,
};
use crate::market::MarketKind;
use crate::mode::ExecutionMode;
use crate::preflight::PreflightReport;
use crate::profit::{AssetSnapshot, BalanceDelta, ProfitEvidence, ProfitVerificationStatus};
use crate::receipt::{ExpectedTransaction, ReceiptStatus, ReceiptTracker, TrackedReceipt};
use crate::signer::Signer;
use crate::stage::{Abilities, ExecutionSetup};
use crate::submitter::SubmissionOutcome;

/// The steps of a simulated plan that are transactions.
///
/// A plan also measures: `MeasureNative` is answered by reading state (no call at all) and
/// `MeasureErc20` by a `balanceOf` call the engine runs *inside the simulation*. Neither has
/// to be broadcast — the runner reads the same facts from the endpoint instead of paying gas
/// for them — and deriving the count this way is what makes "six transactions" a fact about
/// the plan rather than a number this file is told.
pub fn broadcastable(run: &SimulationResult) -> Vec<&ExecutedStep> {
    run.steps
        .iter()
        .filter(|step| step.measured.is_none())
        .collect()
}

/// One transaction of the sequence, with the measurement its gas limit traces to (§13).
#[derive(Clone, Debug)]
pub struct PlannedStep {
    /// Position within the sequence, matching [`SequencePosition::index`].
    pub position: usize,
    /// The step's own index in the simulated plan, which runs ahead of `position` as soon as
    /// a measurement step precedes it.
    pub simulated_index: usize,
    /// The call the simulation recorded, e.g. `deposit()` or `transfer(address,uint256)`.
    pub signature: String,
    /// The intent to send. Its nonce and fee fields are unfilled — both are read per step,
    /// because they are facts about the moment of sending.
    pub intent: TransactionIntent,
    /// Gas the simulation measured for *this* step; the builder resolves the live limit from
    /// it plus the policy's margin.
    pub simulated_gas_used: u64,
    /// Logs this step emitted in simulation, which is the floor for what its receipt shows.
    pub simulated_logs: usize,
}

impl PlannedStep {
    pub fn describe(&self) -> String {
        format!(
            "transaction {} ({}), simulated step {}, target {}, gas {}",
            self.position + 1,
            self.signature,
            self.simulated_index,
            self.intent.target,
            self.simulated_gas_used
        )
    }
}

/// §18's tolerance, stated as a ratio the report can name.
///
/// Deliberately no `Default`: a tolerance nobody chose is how a mismatch becomes a rounding
/// error. The caller passes the number it is willing to defend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tolerance {
    pub numerator: u64,
    pub denominator: u64,
}

impl Tolerance {
    pub const fn new(numerator: u64, denominator: u64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Whether `actual` is within `expected × numerator / denominator` of `expected`.
    ///
    /// Symmetric on purpose: a route that used *less* gas than simulated differs too, and a
    /// one-sided rule would make the audit apply only to outcomes the run would rather not
    /// report. A zero `expected` demands an exact match, and a zero ratio does the same —
    /// there is no reading under which "0% of nothing" is a licence for anything.
    pub fn within(&self, expected: U256, actual: U256) -> bool {
        let difference = if actual >= expected {
            actual - expected
        } else {
            expected - actual
        };
        if self.numerator == 0 || self.denominator == 0 {
            return difference.is_zero();
        }
        difference
            <= expected.saturating_mul(U256::from(self.numerator)) / U256::from(self.denominator)
    }

    pub fn describe(&self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "numerator": self.numerator,
            "denominator": self.denominator,
        })
    }
}

/// The route as the simulation decided it, in the order it must be sent.
#[derive(Clone, Debug)]
pub struct SequencePlan {
    pub opportunity_id: String,
    pub sender: Address,
    /// The block the simulation pinned. Every step's intent carries it and §32's binding leg
    /// is re-read per send rather than assumed from here.
    pub block_number: u64,
    pub block_hash: B256,
    pub state_fingerprint: String,
    pub steps: Vec<PlannedStep>,
    /// The two venues, in leg order, from the plan's own route (§23's question).
    pub pools: Vec<Address>,
    /// The asset the route starts and ends in — §16's WETH.
    pub input_token: Address,
    pub input_amount: U256,
    /// Every token the snapshots read: each one whose `Transfer` logs the simulated run
    /// emitted, plus §16's input token whether or not it transferred. §21's cross-check needs
    /// both sides of each token, and a wrapped asset changes balance through logs that are not
    /// `Transfer`.
    pub tokens: Vec<Address>,
    /// The measurements the simulation recorded, by binding name — §18's expected side.
    pub measurements: BTreeMap<&'static str, U256>,
    pub expected_gas_used: u64,
    pub funding: SenderFunding,
    /// §51's label, supplied by the caller and required by the constructor: whether the
    /// market produced this route or the run arranged it. It lives on the plan because the
    /// plan is the thing that gets sent, and a profit verdict that never had to look at this
    /// field could count a fixture as a real arbitrage.
    pub market: MarketKind,
    pub provenance: String,
}

impl SequencePlan {
    /// Take the executable shape out of a run the risk layer accepted.
    ///
    /// This is the only way a plan comes into existence, and it is a constructor rather than
    /// a builder with setters: a plan whose steps were not the run's steps would be §1's
    /// "制造套利" rewritten in the execution layer, so every field below is read out of `run`,
    /// and the only things a caller supplies are the identities and the §34 funding statement
    /// the intent layer already requires.
    /// `market` is §51's label, and it has no default for the same reason [`Tolerance`] has
    /// none: the difference between a route the chain offered and a route the run arranged is
    /// a fact the caller knows and the executor cannot read off a [`SimulationResult`] — a
    /// successful simulation looks identical either way.
    pub fn from_run(
        run: &SimulationResult,
        decision: &evm_risk::RiskDecision,
        opportunity_id: &str,
        state_fingerprint: &str,
        funding: SenderFunding,
        market: MarketKind,
    ) -> Result<Self> {
        if !decision.accepted() {
            return Err(ExecutionError::InvalidIntent(format!(
                "a sequence needs a RiskDecision::Accept and this is {}",
                match decision {
                    evm_risk::RiskDecision::Accept { .. } => "an accept",
                    evm_risk::RiskDecision::Reject { .. } => "a rejection",
                    evm_risk::RiskDecision::Unknown { .. } => "an unknown",
                }
            )));
        }
        if !run.success() {
            return Err(ExecutionError::InvalidIntent(format!(
                "the run this sequence would inherit did not complete: {}",
                run.summary()
            )));
        }
        if run.plan_summary.pools.len() != 2 {
            return Err(ExecutionError::InvalidIntent(format!(
                "this route names {} venues and §16's shape is two — buy, then sell. A plan of \
                 any other length is not what this executor sends",
                run.plan_summary.pools.len()
            )));
        }
        let executable = broadcastable(run);
        if executable.is_empty() {
            return Err(ExecutionError::InvalidIntent(
                "the plan has no step that is a transaction: every step measured state, so \
                 there is nothing to send"
                    .to_string(),
            ));
        }
        let count = executable.len();
        let mut steps = Vec::with_capacity(count);
        for (position, step) in executable.into_iter().enumerate() {
            if !step.succeeded() {
                return Err(ExecutionError::InvalidIntent(format!(
                    "simulated step {} ({}) did not succeed, so the sequence would send a \
                     transaction the simulation already saw fail",
                    step.index, step.signature
                )));
            }
            if step.gas_used == 0 {
                return Err(ExecutionError::InvalidIntent(format!(
                    "simulated step {} ({}) measured no gas, and §13 makes a measured figure the \
                     only source for a live gas limit",
                    step.index, step.signature
                )));
            }
            let intent = TransactionIntent::from_simulated_step(
                run,
                decision,
                opportunity_id,
                state_fingerprint,
                funding.clone(),
                SequencePosition {
                    index: position,
                    count,
                },
                step,
            )?;
            steps.push(PlannedStep {
                position,
                simulated_index: step.index,
                signature: step.signature.clone(),
                intent,
                simulated_gas_used: step.gas_used,
                simulated_logs: step.logs.len(),
            });
        }
        // The tokens the route touched, taken from what actually emitted `Transfer` in the
        // simulation rather than inferred from which call is which. The input token is always
        // read: a wrap or an unwrap moves no `Transfer`, so a route that deposits, swaps both
        // legs and withdraws would otherwise leave the asset §16's shape starts and ends in out
        // of the snapshots.
        let topics = V2Topics::default();
        let mut tokens: Vec<Address> = Vec::new();
        for (_, log) in run.logs() {
            if log.topics.first() == Some(&topics.transfer) && !tokens.contains(&log.address) {
                tokens.push(log.address);
            }
        }
        if !tokens.contains(&run.plan_summary.input_token) {
            tokens.push(run.plan_summary.input_token);
        }
        tokens.sort();
        // §18's gas line has to compare against what the chain is billed for. A measurement
        // step costs no gas on chain — `MeasureNative` is answered by a state read and
        // `MeasureErc20` by a call the simulation ran for itself — so the expected figure is
        // the sum over the transactions being sent, not the run's whole bill.
        let expected_gas_used = steps.iter().map(|step| step.simulated_gas_used).sum();
        Ok(Self {
            opportunity_id: opportunity_id.to_string(),
            sender: run.sender,
            block_number: run.block.number.0,
            block_hash: run.block.hash,
            state_fingerprint: state_fingerprint.to_string(),
            steps,
            pools: run.plan_summary.pools.clone(),
            input_token: run.plan_summary.input_token,
            input_amount: run.plan_summary.input_amount,
            tokens,
            measurements: run
                .measurements
                .iter()
                .map(|value| (value.binding, value.value))
                .collect(),
            expected_gas_used,
            funding,
            market,
            provenance: format!(
                "{} broadcastable transaction(s) from a {}-step simulated plan pinned to block \
                 {}; venues {} then {} over input token {}",
                count,
                run.steps.len(),
                run.block.number.0,
                run.plan_summary.pools[0],
                run.plan_summary.pools[1],
                run.plan_summary.input_token,
            ),
        })
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The native value the sequence puts into the route — for §16's shape, the `deposit()`
    /// step and nothing else.
    pub fn input_native_wei(&self) -> U256 {
        self.steps
            .iter()
            .fold(U256::ZERO, |total, step| total + step.intent.value)
    }

    /// The measurement §18 compares an actual against.
    pub fn measurement(&self, binding: Binding) -> Option<U256> {
        self.measurements.get(binding.name()).copied()
    }

    pub fn describe(&self) -> Vec<String> {
        self.steps.iter().map(PlannedStep::describe).collect()
    }
}

/// What was read for one asset at one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetReading {
    pub amount: U256,
    /// The method and the block tag, in words — §20's "block before / block after" only means
    /// something if the read names which block it answered for.
    pub source: String,
}

/// The ERC-20 half of §20's snapshot.
///
/// The native half is already answered by [`crate::fee::FeeSource::balance`] (it exists for
/// §32's balance leg), so this trait carries only what that one cannot: `balanceOf` at a named
/// block. The caller passes the height and the provenance names it, which is what makes a
/// snapshot taken against "latest" visible in the evidence rather than a violation someone
/// could commit by accident.
#[async_trait]
pub trait AssetReader: Send + Sync {
    async fn token_balance(
        &self,
        token: Address,
        account: Address,
        block_number: u64,
    ) -> Result<AssetReading>;
}

/// One transaction's trail: what was built, what was sent, what it cost, and what the chain
/// said happened.
#[derive(Clone, Debug)]
pub struct TransactionStep {
    pub position: usize,
    pub signature: String,
    pub nonce: u64,
    pub gas_limit: u64,
    pub transaction_hash: B256,
    pub block_number: u64,
    /// The receipt's own block hash — §20's "block after" pin, already verified canonical by
    /// the tracker's block check (§27).
    pub block_hash: B256,
    pub receipt_status: ReceiptStatus,
    pub signed: SignedTransactionEvidence,
    pub submission: SubmissionEvidence,
    pub cost: ExecutionCostEvidence,
    /// The receipt's logs, kept in send order; the audits read the union across steps.
    pub logs: Vec<ChainLog>,
}

impl TransactionStep {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "position": self.position,
            "signature": self.signature,
            "nonce": self.nonce,
            "gas_limit": self.gas_limit,
            "transaction_hash": format!("{:#x}", self.transaction_hash),
            "block_number": self.block_number,
            "block_hash": format!("{:#x}", self.block_hash),
            "receipt_status": self.receipt_status.name(),
            "logs": self.logs.len(),
            "gas_used": self.cost.gas_used,
            "l2_fee": self.cost.l2_fee.to_string(),
            "l1_fee": self.cost.l1_fee.to_string(),
            "total_execution_cost": self.cost.total_execution_cost.to_string(),
            "l1_fee_source": self.cost.l1_fee_source.describe(),
        })
    }
}

/// §22's `Transfer`, decoded from a receipt log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenFlow {
    /// The contract that emitted it — §22's `token`, by the definition that a token's own
    /// log names it rather than by a registry lookup.
    pub token: Address,
    pub from: Address,
    pub to: Address,
    pub amount: U256,
    pub log_index: u64,
    pub block_number: u64,
    pub transaction_hash: B256,
}

/// A token contract's own statement that it minted to or burned from an account, which is
/// §22's audit of the move that no `Transfer` reports.
///
/// Only the route's wrapped asset is read this way, and only from its own contract: the
/// `Deposit(address,uint256)`/`Withdrawal(address,uint256)` declaration pair is what WETH9
/// emits for `deposit()`/`withdraw()` — a token locked *into* a vault for a withdrawable
/// receipt is a different thing, and some other token using these topics for it would be
/// misread here. §16's shape has exactly one such bridge, so the decode is bounded to it and
/// the evidence stays a fact about this route rather than a guess about the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrapMove {
    pub token: Address,
    pub account: Address,
    pub amount: U256,
    /// `true` for the mint side (`deposit()`: native ETH in, token out) and `false` for the
    /// burn side (`withdraw()`: token in, native ETH out).
    pub mints: bool,
    pub log_index: u64,
    pub block_number: u64,
    pub transaction_hash: B256,
}

/// §23's `Swap` as the pool emitted it.
///
/// `amount0_*`/`amount1_*` stay indexed by the pair's own token order instead of being
/// renamed "in" and "out": that rename needs the pair's `token0()`, and the route audit does
/// not ask the question the rename would answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapObserved {
    pub pool: Address,
    pub sender: Address,
    pub recipient: Address,
    pub amount0_in: U256,
    pub amount1_in: U256,
    pub amount0_out: U256,
    pub amount1_out: U256,
    pub log_index: u64,
    pub block_number: u64,
    pub transaction_hash: B256,
}

impl SwapObserved {
    /// Ordering key: `(block, log index)` is the chain's own sequence across the whole route.
    pub fn order(&self) -> (u64, u64) {
        (self.block_number, self.log_index)
    }
}

/// §23: one venue's own account of its leg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteLeg {
    pub pool: Address,
    pub leg: usize,
    pub swaps_found: usize,
    pub first_swap: Option<SwapObserved>,
    /// What made this leg pass or fail, in one sentence for the report.
    pub verdict: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteAudit {
    pub legs: Vec<RouteLeg>,
    pub passed: bool,
    pub detail: String,
}

impl RouteAudit {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "passed": self.passed,
            "detail": self.detail,
            "legs": self.legs.iter().map(|leg| serde_json::json!({
                "leg": leg.leg + 1,
                "pool": format!("{}", leg.pool),
                "swap_logs": leg.swaps_found,
                "first_swap_block": leg.first_swap.map(|swap| swap.block_number),
                "first_swap_log_index": leg.first_swap.map(|swap| swap.log_index),
                "verdict": leg.verdict,
            })).collect::<Vec<_>>(),
        })
    }
}

/// §21: the same token measured two ways.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowCheck {
    pub token: Address,
    /// `Σ(to == account) − Σ(from == account)` over the Transfer logs the receipts carried.
    pub log_net: I256,
    /// Wrapped by this token contract in the run (`Deposit`), which raises a balance without
    /// a Transfer; zero for every token that is not the route's wrapped asset.
    pub minted: U256,
    /// Unwrapped back to native in the run (`Withdrawal`), which lowers a balance without a
    /// Transfer.
    pub burned: U256,
    /// `balanceOf` after minus before, at the two pinned blocks.
    pub balance_delta: I256,
    /// Whether both snapshots really read this token. Without a read there is nothing to
    /// reconcile against, and `balance_delta` is then the zero an absent map entry answers with
    /// — which must not be allowed to look like a measured agreement.
    pub balance_read: bool,
    pub agrees: bool,
}

/// §18/§47: one expected-vs-actual pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaLine {
    pub name: String,
    pub expected: U256,
    pub actual: U256,
    /// Signed towards the actual, so a shortfall reads as negative.
    pub difference: I256,
    pub within: bool,
    /// Where each side came from — a mismatch nobody can trace is not evidence.
    pub source: String,
}

impl DeltaLine {
    fn describe(&self) -> String {
        format!(
            "{}: expected {} actual {} difference {}",
            self.name, self.expected, self.actual, self.difference
        )
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "expected": self.expected.to_string(),
            "actual": self.actual.to_string(),
            "difference": self.difference.to_string(),
            "within_tolerance": self.within,
            "source": self.source,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeltaAudit {
    pub tolerance: Tolerance,
    pub lines: Vec<DeltaLine>,
    /// Quantities the simulation never measured. Listing them is the difference between an
    /// audit that says "nothing was out of tolerance" and one that says what it compared.
    pub uncompared: Vec<String>,
}

impl DeltaAudit {
    pub fn new(tolerance: Tolerance) -> Self {
        Self {
            tolerance,
            lines: Vec::new(),
            uncompared: Vec::new(),
        }
    }

    /// Add a pair. A missing `expected` records an uncompared quantity and adds no line — it
    /// never silently passes a comparison that was never made.
    pub fn push(&mut self, name: &str, expected: Option<U256>, actual: U256, source: &str) {
        let Some(expected) = expected else {
            self.uncompared.push(format!("{name}: {source}"));
            return;
        };
        let difference = if actual >= expected {
            I256::from_raw(actual - expected)
        } else {
            -I256::from_raw(expected - actual)
        };
        let within = self.tolerance.within(expected, actual);
        self.lines.push(DeltaLine {
            name: name.to_string(),
            expected,
            actual,
            difference,
            within,
            source: source.to_string(),
        });
    }

    pub fn mismatched(&self) -> Vec<&DeltaLine> {
        self.lines.iter().filter(|line| !line.within).collect()
    }

    /// §18's verdict as an error: the first line outside tolerance names itself and its
    /// numbers. Every failing line is in [`DeltaAudit::mismatched`] for the report; this is
    /// the one the run stops on.
    pub fn outcome(&self) -> std::result::Result<(), ExecutionError> {
        match self.mismatched().first() {
            None => Ok(()),
            Some(line) => Err(ExecutionError::ExecutionMismatch(format!(
                "{} (tolerance {}); {}",
                line.describe(),
                self.tolerance.describe(),
                line.source,
            ))),
        }
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "tolerance": self.tolerance.to_json(),
            "lines": self.lines.iter().map(DeltaLine::to_json).collect::<Vec<_>>(),
            "uncompared": self.uncompared,
        })
    }
}

/// §12's build row: everything one step's `Build` produced, recorded the moment the step
/// reaches Built.
///
/// [`SequenceReport::transactions`] only exists for a step that was signed, so a build-only
/// route — every route M8.4.4 runs — left no build fields in its evidence at all. M8.4.4 §19
/// asks whether two arms that differ only in a propagated block context produce *the same
/// transaction*, and that question cannot be answered from a record which holds none of the
/// transaction. This row is the record; it copies what is already in hand and asks the node
/// for nothing.
///
/// `fingerprint` is [`crate::builder::Build::signing_hash`] — keccak over `serialization_bytes`
/// of the signing payload — so byte equality of the two arms' serializations is what that one
/// field proves, and the nested `unsigned` is what names which field it would have been.
#[derive(Clone, Debug)]
pub struct StepBuild {
    /// 0-based, the same index [`TransactionStep::position`] uses; the `sources` lines call the
    /// same step by its 1-based number.
    pub position: usize,
    pub identity: BlockIdentity,
    pub unsigned: crate::tx::UnsignedTransaction,
    pub serialization_bytes: usize,
    pub fingerprint: B256,
    pub sender_expected: Address,
}

impl StepBuild {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "position": self.position,
            "chain_id": self.identity.chain_id.0,
            "block_number": self.identity.number.0,
            "block_hash": format!("{:#x}", self.identity.hash),
            "unsigned": serde_json::to_value(&self.unsigned).ok(),
            "serialization_bytes": self.serialization_bytes,
            "fingerprint": format!("{:#x}", self.fingerprint),
            "sender_expected": format!("{}", self.sender_expected),
        })
    }
}

/// M8.4.4 §31's consumer row: the step's own pin, and the verdict the propagated context got
/// against it, as fields rather than as a sentence.
///
/// The `sources` line says the same thing for a human; this says it for [`crate::block_context`]'s
/// own §17 requirement that a reader recompute *which* refusal fired from the raw record. Prose is
/// where a name like `block_hash_mismatch` goes to become a paragraph nobody can count.
///
/// `reused` is §31's `reused = true`, and it is false on every path this crate has today: §2 keeps
/// the step's binding read in place, so `consumer_read` is `true` and the value that served the step
/// was the step's own. The field is published rather than inferred because that is the sentence the
/// milestone is being asked to earn — with a number in the record saying `false`, the report cannot
/// quietly state a saving it did not make.
#[derive(Clone, Debug)]
pub struct StepContextCheck {
    /// 0-based, the index [`StepBuild`] uses for the same step.
    pub position: usize,
    /// What this step pins — the identity the propagated context was checked against.
    pub expected: BlockIdentity,
    pub outcome: ContextOutcome,
}

impl StepContextCheck {
    fn to_json(&self) -> serde_json::Value {
        let mut row = self.outcome.to_row();
        row["position"] = serde_json::json!(self.position);
        row["expected_chain_id"] = serde_json::json!(self.expected.chain_id.0);
        row["expected_block_number"] = serde_json::json!(self.expected.number.0);
        row["expected_block_hash"] = serde_json::json!(format!("{:#x}", self.expected.hash));
        row["reused"] = serde_json::json!(matches!(
            &self.outcome,
            ContextOutcome::Accepted {
                consumer_read: false
            }
        ));
        row
    }
}

/// What the sequence left behind, in the order the evidence was produced.
#[derive(Clone, Debug)]
pub struct SequenceReport {
    pub execution_id: Option<String>,
    pub opportunity_id: String,
    pub mode: ExecutionMode,
    /// §51's label, copied off the plan so the row this report writes cannot disagree with
    /// the route it describes.
    pub market: MarketKind,
    /// Every transaction was sent and included. A partial run says `false` and keeps the steps
    /// it has (§34) — the half-route is a fact about the wallet.
    pub completed: bool,
    /// How many transactions the plan called for, so `line()` can state the fraction it ran.
    pub steps_planned: usize,
    pub reached: Option<ExecutionStatus>,
    pub stopped_at: Option<usize>,
    pub transactions: Vec<TransactionStep>,
    /// Every step that reached Built, with the transaction it built, in step order.
    pub builds: Vec<StepBuild>,
    /// Every step that ran the consumer leg, with the verdict its propagated context got, in step
    /// order. A step that was given no context is in here too — §51 counts the silence.
    pub context_checks: Vec<StepContextCheck>,
    pub before: Option<AssetSnapshot>,
    pub after: Option<AssetSnapshot>,
    pub flows: Vec<TokenFlow>,
    /// The route's wrapped asset's `Deposit`/`Withdrawal` logs (§22's second half).
    pub wraps: Vec<WrapMove>,
    pub flow_checks: Vec<FlowCheck>,
    pub route: Option<RouteAudit>,
    pub deltas: DeltaAudit,
    pub cost: Option<SequenceCost>,
    pub profit: Option<ProfitEvidence>,
    /// §18/§47's finding when one arrived, and the reason the run stopped.
    pub mismatch: Option<ExecutionError>,
    pub lane: LaneRelease,
    pub detail: String,
    pub sources: Vec<String>,
}

impl SequenceReport {
    fn open(plan: &SequencePlan, mode: ExecutionMode, tolerance: Tolerance) -> Self {
        Self {
            execution_id: None,
            opportunity_id: plan.opportunity_id.clone(),
            mode,
            market: plan.market.clone(),
            completed: false,
            steps_planned: plan.len(),
            reached: None,
            stopped_at: None,
            transactions: Vec::new(),
            builds: Vec::new(),
            context_checks: Vec::new(),
            before: None,
            after: None,
            flows: Vec::new(),
            wraps: Vec::new(),
            flow_checks: Vec::new(),
            route: None,
            deltas: DeltaAudit::new(tolerance),
            cost: None,
            profit: None,
            mismatch: None,
            lane: LaneRelease::Released,
            detail: String::new(),
            sources: vec![plan.provenance.clone()],
        }
    }

    /// The receipts' logs in chain order. [`evm_chain::ChainLog::position`] is the chain's own
    /// sequence — block, transaction, log index — and it is this module's only ordering
    /// assumption, so it is stated here rather than implied.
    fn logs(&self) -> Vec<ChainLog> {
        let mut logs: Vec<ChainLog> = self
            .transactions
            .iter()
            .flat_map(|step| step.logs.iter().cloned())
            .collect();
        logs.sort_by_key(ChainLog::position);
        logs
    }

    /// §57's one-line summary of a whole route.
    pub fn line(&self) -> String {
        format!(
            "execution_id={} opportunity_id={} mode={} market={} steps={}/{} status={} completed={} — {}",
            self.execution_id.as_deref().unwrap_or("-"),
            self.opportunity_id,
            self.mode.name(),
            self.market.name(),
            self.transactions.len(),
            self.steps_planned,
            self.reached.map_or("no-record", |status| status.name()),
            self.completed,
            self.detail,
        )
    }

    /// §51 and §56 together: a run counts as one successful *real* arbitrage only when the
    /// chain settled a positive profit **and** the route came from the market. Both halves are
    /// needed and neither is a preference — a fixture that paid is still a fixture, and
    /// `RealizedProfit`'s own verdict knows nothing about how the route came to exist.
    pub fn counts_as_successful_real_arbitrage(&self) -> bool {
        self.market.counts_as_real_arbitrage()
            && self
                .profit
                .as_ref()
                .is_some_and(ProfitEvidence::counts_as_successful_real_arbitrage)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "execution_id": self.execution_id,
            "opportunity_id": self.opportunity_id,
            "mode": self.mode.name(),
            "market": self.market.to_json(),
            "completed": self.completed,
            "steps_planned": self.steps_planned,
            "status": self.reached.map_or("no-record".to_string(), |status| status.name().to_string()),
            "stopped_at": self.stopped_at,
            "transactions": self.transactions.iter().map(TransactionStep::to_json).collect::<Vec<_>>(),
            "builds": self.builds.iter().map(StepBuild::to_json).collect::<Vec<_>>(),
            "context_checks": self
                .context_checks
                .iter()
                .map(StepContextCheck::to_json)
                .collect::<Vec<_>>(),
            "before": self.before.as_ref().and_then(|s| serde_json::to_value(s).ok()),
            "after": self.after.as_ref().and_then(|s| serde_json::to_value(s).ok()),
            "flows": self.flows.iter().map(|flow| serde_json::json!({
                "token": format!("{}", flow.token),
                "from": format!("{}", flow.from),
                "to": format!("{}", flow.to),
                "amount": flow.amount.to_string(),
                "log_index": flow.log_index,
                "block_number": flow.block_number,
                "transaction_hash": format!("{:#x}", flow.transaction_hash),
            })).collect::<Vec<_>>(),
            "wraps": self.wraps.iter().map(|movement| serde_json::json!({
                "token": format!("{}", movement.token),
                "account": format!("{}", movement.account),
                "amount": movement.amount.to_string(),
                "kind": if movement.mints { "deposit" } else { "withdrawal" },
                "log_index": movement.log_index,
                "block_number": movement.block_number,
                "transaction_hash": format!("{:#x}", movement.transaction_hash),
            })).collect::<Vec<_>>(),
            "flow_checks": self.flow_checks.iter().map(|check| serde_json::json!({
                "token": format!("{}", check.token),
                "log_net": check.log_net.to_string(),
                "minted": check.minted.to_string(),
                "burned": check.burned.to_string(),
                "balance_delta": check.balance_delta.to_string(),
                "balance_read": check.balance_read,
                "agrees": check.agrees,
            })).collect::<Vec<_>>(),
            "route": self.route.as_ref().map(RouteAudit::to_json).unwrap_or(serde_json::Value::Null),
            "delta_audit": self.deltas.to_json(),
            "cost": self.cost.as_ref().and_then(|cost| serde_json::to_value(cost).ok()),
            "profit": self.profit.as_ref().and_then(|profit| serde_json::to_value(profit).ok()),
            "mismatch": self.mismatch.as_ref().map(|error| error.to_string()),
            "lane": match &self.lane {
                LaneRelease::Released => serde_json::Value::String("released".to_string()),
                LaneRelease::Held { reason } => serde_json::json!({ "held": reason }),
            },
            "detail": self.detail,
            "sources": self.sources,
        })
    }
}

/// One block, named by height *and* hash — §20's binding for a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotPin {
    pub block_number: u64,
    pub block_hash: B256,
}

/// Read native plus every listed token at one pinned block, and say which reads produced it.
///
/// The hash is an argument rather than a lookup because the two halves of §20's pair are read
/// at different moments for different reasons: the before-snapshot pins a head the caller
/// named, and the after-snapshot pins the last bound receipt's own block — whose hash the
/// tracker already verified against the endpoint (§27). Re-reading a fresh head here would let
/// the after-snapshot name a block newer than the transaction it exists to account for.
pub async fn read_snapshot(
    fees: &dyn FeeSource,
    assets: &dyn AssetReader,
    account: Address,
    pin: &SnapshotPin,
    tokens: &[Address],
) -> Result<AssetSnapshot> {
    let native = fees.balance(account, pin.block_number).await?;
    let mut token_balances = BTreeMap::new();
    let mut sources = vec![format!(
        "native: eth_getBalance({account}, {})",
        pin.block_number
    )];
    for token in tokens {
        let reading = assets
            .token_balance(*token, account, pin.block_number)
            .await?;
        sources.push(reading.source);
        token_balances.insert(*token, reading.amount);
    }
    Ok(AssetSnapshot {
        block_number: pin.block_number,
        block_hash: pin.block_hash,
        account,
        native_wei: native,
        token_balances,
        provenance: format!(
            "block {} ({:#x}); {}",
            pin.block_number,
            pin.block_hash,
            sources.join("; ")
        ),
    })
}

/// §22: every `Transfer` a receipt carried.
///
/// Returns the flows and a count of `Transfer` logs that did not decode. A short `from`/`to`
/// topic or a missing amount word is reported as a count rather than dropped, because the
/// audit that follows would otherwise read fewer flows than the chain emitted and call that
/// agreement.
pub fn transfer_flows(logs: &[ChainLog]) -> (Vec<TokenFlow>, usize) {
    let topics = V2Topics::default();
    let mut flows = Vec::new();
    let mut undecodable = 0usize;
    for log in logs {
        if log.topics.first() != Some(&topics.transfer) {
            continue;
        }
        let decoded = || -> std::result::Result<TokenFlow, ()> {
            Ok(TokenFlow {
                token: log.address,
                from: topic_address(&log.topics, 1).map_err(|_| ())?,
                to: topic_address(&log.topics, 2).map_err(|_| ())?,
                amount: word(&log.data, 0).map_err(|_| ())?,
                log_index: log.log_index.0,
                block_number: log.block_number.0,
                transaction_hash: log.tx_hash.0,
            })
        };
        match decoded() {
            Ok(flow) => flows.push(flow),
            Err(()) => undecodable += 1,
        }
    }
    (flows, undecodable)
}

/// §22's other half: the token contract's own `Deposit`/`Withdrawal` logs for `token`.
///
/// A wrap or an unwrap moves no `Transfer`, so without this the flow audit would read the
/// deposited asset as having left and never come back. Bounded to one token for the reason in
/// [`WrapMove`].
pub fn wrap_moves(logs: &[ChainLog], token: Address) -> (Vec<WrapMove>, usize) {
    let topics = Weth9Topics::default();
    let mut moves = Vec::new();
    let mut undecodable = 0usize;
    for log in logs {
        let mints = match log.topics.first() {
            Some(topic0) if *topic0 == topics.deposit => true,
            Some(topic0) if *topic0 == topics.withdrawal => false,
            _ => continue,
        };
        if log.address != token {
            continue;
        }
        let decoded = || -> std::result::Result<WrapMove, ()> {
            Ok(WrapMove {
                token,
                account: topic_address(&log.topics, 1).map_err(|_| ())?,
                amount: word(&log.data, 0).map_err(|_| ())?,
                mints,
                log_index: log.log_index.0,
                block_number: log.block_number.0,
                transaction_hash: log.tx_hash.0,
            })
        };
        match decoded() {
            Ok(movement) => moves.push(movement),
            Err(()) => undecodable += 1,
        }
    }
    (moves, undecodable)
}

/// §23's inputs: every `Swap` a receipt carried, attributed to the contract that emitted it.
pub fn swap_observations(logs: &[ChainLog]) -> (Vec<SwapObserved>, usize) {
    let topics = V2Topics::default();
    let mut observed = Vec::new();
    let mut undecodable = 0usize;
    for log in logs {
        if log.topics.first() != Some(&topics.swap) {
            continue;
        }
        let decoded = || -> std::result::Result<SwapObserved, ()> {
            let amounts = [0usize, 1, 2, 3].map(|index| word(&log.data, index).ok());
            let [Some(amount0_in), Some(amount1_in), Some(amount0_out), Some(amount1_out)] =
                amounts
            else {
                return Err(());
            };
            Ok(SwapObserved {
                pool: log.address,
                sender: topic_address(&log.topics, 1).map_err(|_| ())?,
                recipient: topic_address(&log.topics, 2).map_err(|_| ())?,
                amount0_in,
                amount1_in,
                amount0_out,
                amount1_out,
                log_index: log.log_index.0,
                block_number: log.block_number.0,
                transaction_hash: log.tx_hash.0,
            })
        };
        match decoded() {
            Ok(swap) => observed.push(swap),
            Err(()) => undecodable += 1,
        }
    }
    (observed, undecodable)
}

/// §23: prove both legs from the venues' own logs.
///
/// `pools` is in leg order. A leg passes when that venue emitted at least one `Swap` **and**
/// its first one is not before the previous leg's — which is the difference between "the trade
/// happened" and "the trade happened the way the plan said", and the reason §23 refuses
/// `status = 1` as evidence of an arbitrage.
pub fn audit_route(pools: &[Address], swaps: &[SwapObserved], account: Address) -> RouteAudit {
    let mut legs = Vec::new();
    let mut previous: Option<(u64, u64)> = None;
    let mut passed = true;
    for (leg, pool) in pools.iter().enumerate() {
        let found: Vec<&SwapObserved> = swaps.iter().filter(|swap| swap.pool == *pool).collect();
        let first = found.first().copied();
        let verdict = match first {
            None => {
                passed = false;
                format!(
                    "{pool} emitted no Swap log in any receipt of this run, so leg {} is not \
                     evidenced by the venue that was supposed to execute it",
                    leg + 1
                )
            }
            Some(swap) if previous.is_some_and(|previous| swap.order() <= previous) => {
                passed = false;
                let previous = previous.expect("the arm above only runs with a previous leg");
                format!(
                    "{pool}'s first Swap log is at block {} log {}, which is not after the \
                     previous leg's at ({}, {}): the legs did not run in route order",
                    swap.block_number, swap.log_index, previous.0, previous.1
                )
            }
            Some(swap) => format!(
                "{pool} emitted {} Swap log(s), first at block {} log {}, carrying amount0 \
                 in/out {}/{} and amount1 in/out {}/{} to {}",
                found.len(),
                swap.block_number,
                swap.log_index,
                swap.amount0_in,
                swap.amount0_out,
                swap.amount1_in,
                swap.amount1_out,
                swap.recipient,
            ),
        };
        previous = first.map(SwapObserved::order);
        legs.push(RouteLeg {
            pool: *pool,
            leg,
            swaps_found: found.len(),
            first_swap: first.copied(),
            verdict,
        });
    }
    let detail = if passed {
        format!(
            "both venues emitted their own Swap, in route order, with the route's sender {} in \
             the loop",
            account
        )
    } else {
        legs.iter()
            .filter(|leg| leg.swaps_found == 0 || leg.verdict.contains("not after"))
            .map(|leg| leg.verdict.clone())
            .collect::<Vec<_>>()
            .join("; ")
    };
    RouteAudit {
        legs,
        passed,
        detail,
    }
}

/// §21: reconcile each token's log total against its `balanceOf` difference.
///
/// Tokens appear from either side — a token the receipts moved but the snapshots did not read,
/// and one the snapshots read but nothing moved, both get a line, because each asymmetry is a
/// different thing the evidence failed to cover.
///
/// A wrapped asset's balance also moves without a `Transfer`, so its `wraps` are read as part
/// of the log side: minted in, burned out. Any difference that is left over after that is
/// reported as a disagreement rather than absorbed — a transfer tax, an unexpected burn, or a
/// flow this audit never decoded all look the same from here, and each is a fact the report
/// has to state.
pub fn reconcile_flows(
    flows: &[TokenFlow],
    delta: Option<&BalanceDelta>,
    account: Address,
    wraps: &[WrapMove],
) -> Vec<FlowCheck> {
    // One line per token, whichever side produced it: a route that moved the same asset in and
    // out again still has one balance to reconcile, and repeating the line would make the count
    // of checks read like a count of transfers.
    let mut tokens: Vec<Address> = Vec::new();
    for token in flows.iter().map(|flow| flow.token) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    if let Some(delta) = delta {
        for snapshot in [&delta.before, &delta.after] {
            for token in snapshot.token_balances.keys() {
                if !tokens.contains(token) {
                    tokens.push(*token);
                }
            }
        }
    }
    tokens.sort();
    tokens
        .into_iter()
        .map(|token| {
            let mut log_net = I256::ZERO;
            for flow in flows.iter().filter(|flow| flow.token == token) {
                if flow.to == account {
                    log_net += I256::from_raw(flow.amount);
                }
                if flow.from == account {
                    log_net -= I256::from_raw(flow.amount);
                }
            }
            let sum = |mints: bool| {
                wraps
                    .iter()
                    .filter(|movement| {
                        movement.token == token
                            && movement.account == account
                            && movement.mints == mints
                    })
                    .fold(U256::ZERO, |total, movement| total + movement.amount)
            };
            let (minted, burned) = (sum(true), sum(false));
            let balance_read = delta.is_some_and(|delta| {
                delta.before.balance_read(&token) && delta.after.balance_read(&token)
            });
            let balance_delta = delta.map_or(I256::ZERO, |delta| delta.token_delta(&token));
            FlowCheck {
                token,
                log_net,
                minted,
                burned,
                balance_delta,
                balance_read,
                agrees: balance_read
                    && log_net + I256::from_raw(minted) - I256::from_raw(burned) == balance_delta,
            }
        })
        .collect()
}

/// What the selling venue paid back in the asset the route started with (§24's
/// `final_input_token_balance` side).
fn token_from_venues(
    flows: &[TokenFlow],
    account: Address,
    token: Address,
    venues: &[Address],
) -> U256 {
    flows
        .iter()
        .filter(|flow| flow.token == token && flow.to == account && venues.contains(&flow.from))
        .fold(U256::ZERO, |total, flow| total + flow.amount)
}

/// What left the wallet towards the buying venue in that same asset.
fn token_to_venues(
    flows: &[TokenFlow],
    account: Address,
    token: Address,
    venues: &[Address],
) -> U256 {
    flows
        .iter()
        .filter(|flow| flow.token == token && flow.from == account && venues.contains(&flow.to))
        .fold(U256::ZERO, |total, flow| total + flow.amount)
}

/// The native a `withdraw()` handed back: the token contract's own `Withdrawal` log is the
/// burn, and the wrapped asset redeems exactly the burned amount in ETH (§11's bridge — same
/// asset, two representations, no price involved).
fn native_settled_by_unwrap(wraps: &[WrapMove], account: Address, token: Address) -> U256 {
    wraps
        .iter()
        .filter(|movement| {
            movement.token == token && movement.account == account && !movement.mints
        })
        .fold(U256::ZERO, |total, movement| total + movement.amount)
}

/// §24/§38: choose the accounting from what the evidence shows, and refuse when it does not.
///
/// The branch is not a configuration. A route that put native ETH in and took native ETH back
/// has one provable unit (§14's `NativeWei`); a route that settled in its ERC-20 does not
/// (§13/§14's split), because joining a token gain to an ETH bill needs a price §15 forbids.
/// Which of the two happened is read off the sequence's own value and burn logs.
/// What the route came back with, and what §12's accounting says about it.
///
/// The two travel together because §55's `gross_output` and §55's `gross_profit` are the same
/// measurement seen from two sides: the amount that came back is the one the profit was
/// subtracted from, so computing them in two places would let a report state an output no
/// profit figure was derived from.
struct RouteAccounting {
    evidence: ProfitEvidence,
    /// The route's input asset, as it arrived back in the wallet, measured from the receipts.
    gross_output: U256,
}

fn profit_evidence(
    plan: &SequencePlan,
    delta: &BalanceDelta,
    cost: &SequenceCost,
    flows: &[TokenFlow],
    wraps: &[WrapMove],
) -> Result<RouteAccounting> {
    let input_native = plan.input_native_wei();
    if input_native > U256::ZERO {
        let gross_output = native_settled_by_unwrap(wraps, plan.sender, plan.input_token);
        return Ok(RouteAccounting {
            gross_output,
            evidence: ProfitEvidence::from_native_round_trip(
                &plan.opportunity_id,
                delta,
                cost,
                plan.input_token,
                input_native,
                gross_output,
            )?,
        });
    }
    let paid = token_to_venues(flows, plan.sender, plan.input_token, &plan.pools);
    let received = token_from_venues(flows, plan.sender, plan.input_token, &plan.pools);
    if paid > U256::ZERO || received > U256::ZERO {
        return Ok(RouteAccounting {
            gross_output: received,
            evidence: ProfitEvidence::from_token_round_trip(
                &plan.opportunity_id,
                delta,
                cost,
                plan.input_token,
                "the route's input token left for the buying venue and came back from the selling \
                 one, and no transaction unwrapped it to native ETH — so the gain is in the token \
                 and the bill is in ETH (§13/§14)",
                paid,
                received,
            )?,
        });
    }
    Err(ExecutionError::Evidence(format!(
        "no asset movement this module can name appears in the receipts of a {} transaction \
         sequence that put in no native ETH, so §24's formula has no `initial input token \
         balance` side to subtract from",
        cost.transaction_count
    )))
}

/// §26/§27 as a precondition of the ladder, stated once so all three refusals read the same.
///
/// The gate itself is [`crate::preflight::ExecutionPreflight`] and it is deliberately *not*
/// called here: by the time a route is being sent, the head, reserve and oracle reads it
/// performs belong to the caller, and §27 forbids this module from re-searching or
/// re-deciding anything. What the stage can and must check is the three
/// things that make the verdict about *this* attempt: that one exists, that it passed, and
/// that the identifier it was drawn over is the plan now being sent. A verdict for another
/// opportunity is the candidate-switch §27 names, wearing a PASS.
fn preflight_cleared(
    plan: &SequencePlan,
    preflight: Option<&PreflightReport>,
) -> std::result::Result<(), ExecutionError> {
    let Some(verdict) = preflight else {
        return Err(ExecutionError::InvalidIntent(format!(
            "§26 never ran for {}: this is an arbitrage route, so no step may be built, \
             signed or sent on a simulation-and-risk decision alone",
            plan.opportunity_id
        )));
    };
    if verdict.attempt_id != plan.opportunity_id {
        return Err(ExecutionError::InvalidIntent(format!(
            "§27: the preflight verdict passed for {}, and this route is {} — one attempt's \
             gate cannot clear another's, and a plan never re-priced cannot be sent",
            verdict.attempt_id, plan.opportunity_id
        )));
    }
    if !verdict.passed {
        return Err(match verdict.error() {
            Some(error) => error,
            None => ExecutionError::InvalidIntent(verdict.describe()),
        });
    }
    Ok(())
}

/// How a step ended in a way that stops the sequence.
enum Halt {
    Failed(ExecutionError),
    Blocked(String),
}

impl From<ExecutionError> for Halt {
    fn from(error: ExecutionError) -> Self {
        Self::Failed(error)
    }
}

/// §54's ladder, run N times over one lane and one record.
pub struct SequenceStage {
    abilities: Abilities,
    assets: Arc<dyn AssetReader + Send + Sync>,
    signer: Signer,
    setup: ExecutionSetup,
    lane: ExecutionLane,
    ledger: Ledger,
    clock: Clock,
    configured_chain_id: u64,
    tolerance: Tolerance,
    /// Bytes from this run reached a node, so §25 — not §11 — decides whether the lane lets
    /// go. Tracked on the stage rather than inferred from the report because a submission that
    /// was `Unknown` produced no report row at all.
    in_flight: bool,
    /// The trace this lane's reads are being recorded into, when the run has one.
    ///
    /// M8.4.1 §11's second half: connecting the lane's socket to the run's sink makes its calls
    /// *visible*, and this handle is what makes each one *attributable* — §9 asks every recorded
    /// call for the stage and the caller that issued it, and the code issuing a step's fee,
    /// nonce, balance and binding reads is this function, which no layer above it can name.
    /// `None` is the ordinary case and not a failure: no sink, no labels, same run.
    rpc_trace: Option<RpcTraceSink>,
}

impl SequenceStage {
    /// The sequence over any endpoint that answers the reads — the live adapter, or a
    /// scripted one in a test (§40): the same four surfaces
    /// [`crate::stage::ExecutionStage::new`] takes, plus the ERC-20 half of §20's snapshot.
    ///
    /// `tolerance` is required rather than defaulted (§18), and the build policy's gas rule
    /// must be simulation-traced: a configured limit is a number no measurement produced,
    /// which is what §13 refuses for an arbitrage — and every step here is an arbitrage step.
    pub fn new(
        abilities: Abilities,
        assets: Arc<dyn AssetReader + Send + Sync>,
        signer: Signer,
        mut setup: ExecutionSetup,
        configured_chain_id: u64,
        clock: Clock,
        tolerance: Tolerance,
    ) -> Result<Self> {
        if signer.mode() != setup.mode {
            return Err(ExecutionError::ModeGate(format!(
                "the sequence is in {} and was handed a signer in {}; one run has one mode (§20)",
                setup.mode.name(),
                signer.mode().name()
            )));
        }
        if !matches!(setup.build.gas, GasPolicy::SimulationGasPlus { .. }) {
            return Err(ExecutionError::BuildFailed(format!(
                "a step's gas limit has to be that step's measured gas plus a margin (§13) and \
                 this build policy is {}, so the sequence would broadcast a run of transactions \
                 whose limits no simulation produced",
                setup.build.gas.describe()
            )));
        }
        if setup.build.expected_chain_id == 0 {
            setup.build.expected_chain_id = configured_chain_id;
        } else if setup.build.expected_chain_id != configured_chain_id {
            return Err(ExecutionError::ChainMismatch(format!(
                "the build policy expects chain {} and this sequence is configured for chain {}",
                setup.build.expected_chain_id, configured_chain_id
            )));
        }
        Ok(Self {
            abilities,
            assets,
            signer,
            setup,
            lane: ExecutionLane::new(),
            ledger: Ledger::new(),
            clock,
            configured_chain_id,
            tolerance,
            in_flight: false,
            rpc_trace: None,
        })
    }

    pub fn mode(&self) -> ExecutionMode {
        self.setup.mode
    }

    /// The sequence stage over the live endpoint, assembled the way
    /// [`crate::stage::ExecutionStage::connect`] assembles a single-transaction one.
    ///
    /// This exists so that no caller outside this crate has to name a key: the pipeline and
    /// the CLI hand over a URL, a mode and a tolerance, and the one read of
    /// `GIWA_EXECUTION_PRIVATE_KEY` happens here (§17). It is also why the preflight gatherer
    /// borrows [`Self::abilities`] instead of building its own — a gate answered by a
    /// different connection than the one that sends is an opinion about the lane, not a
    /// fact from it.
    pub async fn connect(
        url: &str,
        expected_chain_id: u64,
        setup: ExecutionSetup,
        clock: Clock,
        tolerance: Tolerance,
    ) -> Result<Self> {
        Self::connect_with_trace(url, expected_chain_id, setup, clock, tolerance, None).await
    }

    /// [`connect`][Self::connect], with the run's RPC trace passed through to the lane's socket.
    ///
    /// M8.4.1 §11 asks whether the execution lane's nonce, fee, balance and block-header reads
    /// can join the trace the rest of the run already writes. They can, and this is the only
    /// change that takes: the sink goes onto the one adapter the lane builds for itself, so its
    /// calls land in the same event list as the market's, in the same order, off the same
    /// monotonic origin. No request is added (§12) — the sink observes the calls this function
    /// was already going to make.
    ///
    /// `trace` is also kept on the stage, because §9's `caller` has to be stamped by the code
    /// that issues the read and the four ability surfaces this stage holds carry no sink.
    pub async fn connect_with_trace(
        url: &str,
        expected_chain_id: u64,
        setup: ExecutionSetup,
        clock: Clock,
        tolerance: Tolerance,
        trace: Option<RpcTraceSink>,
    ) -> Result<Self> {
        let adapter = Arc::new(
            crate::giwa::GiwaSequencerDirect::connect_with_trace(
                url,
                expected_chain_id,
                setup.mode,
                crate::submitter::EndpointKind::PublicHttpRpc,
                trace.clone(),
            )
            .await?,
        );
        let abilities = Abilities {
            submitter: adapter.clone(),
            fees: adapter.clone(),
            nonces: adapter.clone(),
            chain: adapter.clone(),
        };
        let assets = Arc::new(adapter.assets());
        let signer = Signer::from_env(setup.mode)?;
        let mut stage = Self::new(
            abilities,
            assets,
            signer,
            setup,
            expected_chain_id,
            clock,
            tolerance,
        )?;
        stage.rpc_trace = trace;
        Ok(stage)
    }

    /// The lane's four surfaces, for the caller that has to price the same attempt it is
    /// about to send (§26). Mutable state is not reachable through this — it is a borrow.
    pub fn abilities(&self) -> &Abilities {
        &self.abilities
    }

    /// A stage assembled rather than connected — a scripted endpoint in a test — with the
    /// run's trace attached for labelling.
    ///
    /// [`connect_with_trace`][Self::connect_with_trace] both wires the sink onto the live socket
    /// and keeps the handle for stamping; a caller that hands over four abilities has already
    /// done the first half wherever those abilities record, and this sets only the second. It
    /// exists so the labelling can be tested without a node: the reads a scripted endpoint
    /// answers are the same reads, in the same order, with the same labels on them.
    pub fn with_rpc_trace(mut self, sink: RpcTraceSink) -> Self {
        self.rpc_trace = Some(sink);
        self
    }

    pub fn setup(&self) -> &ExecutionSetup {
        &self.setup
    }

    /// Name the lane's next reads for the trace: which stage they serve, and which leg of it
    /// asked.
    ///
    /// The stage names are [`evm_metrics`]'s own and not invented for the report (§8), and each
    /// is the stage the run's own ladder gives these reads a span for. `build` covers the price,
    /// nonce, gate and before-snapshot reads: the ladder stamps no instant of its own for them,
    /// so a leg that is really §26's gate keeps the gate in its *caller* name rather than in a
    /// stage name that does not exist.
    ///
    /// With no sink this is a `None` test and nothing else — no lock, no clock read, no request
    /// (§12).
    fn stamp(&self, stage: Stage, caller: &str) {
        if let Some(sink) = &self.rpc_trace {
            sink.set_context(stage.as_str(), caller);
        }
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn lane_is_idle(&self) -> bool {
        self.lane.is_idle()
    }

    /// §54's sequence: one plan, one record, N transactions, then the audits.
    ///
    /// `before_head` is the block the caller read immediately before sending; the run refuses
    /// to form a §20 delta unless it is strictly older than the first transaction's block,
    /// because a "before" read at or after the first inclusion is a picture of the result.
    ///
    /// `preflight` is §26's verdict over this attempt, computed by the caller from live
    /// reads. For an arbitrage it is not optional and it is not re-derived here (§27): the
    /// run stops before the first read, the first nonce and the first byte of calldata
    /// unless it is present, passing, and about the plan it is handed with. A
    /// non-arbitrage attempt (a §35 validation transaction) has nothing to re-price and
    /// ignores the argument.
    pub async fn run(
        &mut self,
        plan: &SequencePlan,
        attempt: &GateAttempt,
        preflight: Option<&PreflightReport>,
        before_head: &SnapshotPin,
        metrics: &mut Metrics,
    ) -> SequenceReport {
        metrics.bump("execution_sequence_attempt");
        let mut report = SequenceReport::open(plan, self.setup.mode, self.tolerance);
        let at = self.clock.now_ms();
        let execution_id =
            match self
                .ledger
                .claim(&plan.steps[0].intent, ExecutionStatus::Detected, at)
            {
                Claim::New(handle) => handle.execution_id,
                Claim::Existing(existing) => {
                    metrics.bump("execution_duplicate");
                    report.execution_id = Some(existing.execution_id.clone());
                    report.reached = Some(existing.status);
                    report.detail = format!(
                        "§30: {} already owns this state binding (it is at {}), so this attempt \
                     stopped before sending anything",
                        existing.execution_id,
                        existing.status.name()
                    );
                    return report;
                }
            };
        report.execution_id = Some(execution_id.clone());
        if attempt.is_arbitrage() {
            self.rung(&execution_id, ExecutionStatus::Simulated, metrics);
            self.rung(&execution_id, ExecutionStatus::RiskApproved, metrics);
            // §26 before §54's `Built`: nothing is read, priced, signed or sent on an attempt
            // the gate has not cleared, and the gate's own reads happened outside this run.
            if let Err(error) = preflight_cleared(plan, preflight) {
                metrics.bump("execution_preflight_blocked");
                self.stop(&mut report, &execution_id, &error, metrics);
                report.stopped_at = Some(0);
                report.reached = self.ledger.get(&execution_id).map(|r| r.status);
                return report;
            }
            self.rung(&execution_id, ExecutionStatus::Preflighted, metrics);
            if let Some(verdict) = preflight {
                report.sources.push(verdict.describe());
            }
        }
        self.stamp(
            Stage::Build,
            "before-snapshot: native and input-token balances",
        );
        report.before = match self
            .snapshot(plan, before_head.block_number, before_head.block_hash)
            .await
        {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                self.stop(&mut report, &execution_id, &error, metrics);
                report.stopped_at = Some(0);
                report.reached = self.ledger.get(&execution_id).map(|r| r.status);
                return report;
            }
        };

        // M8.4.4 §6: what the previous stage verified about the block this route pins. A
        // non-arbitrage attempt has no preflight to take it from, and an arbitrage whose
        // producer refused to issue a context propagates `None` — both arrive at the same
        // `no_context` outcome downstream rather than at an assumed agreement (§30).
        let propagated = preflight.and_then(|verdict| verdict.block_context.context());
        let mut halted = false;
        for position in 0..plan.len() {
            match self
                .drive_step(
                    plan,
                    position,
                    attempt,
                    &execution_id,
                    propagated,
                    &mut report,
                    metrics,
                )
                .await
            {
                Ok(step) => report.transactions.push(step),
                Err(Halt::Failed(error)) => {
                    self.stop(&mut report, &execution_id, &error, metrics);
                    report.stopped_at = Some(position);
                    halted = true;
                    break;
                }
                Err(Halt::Blocked(reason)) => {
                    self.block(&mut report, &execution_id, &reason, metrics);
                    report.stopped_at = Some(position);
                    halted = true;
                    break;
                }
            }
        }
        // "Completed" is the arbitrage's own word, not the sender's: a route that sent every
        // transaction and had one of them revert did not round-trip, and its evidence has to
        // be readable as the partial it is (§34).
        report.completed = !halted
            && report.transactions.len() == plan.len()
            && report
                .transactions
                .iter()
                .all(|step| step.receipt_status == ReceiptStatus::Included);
        self.settle(plan, &mut report, metrics).await;
        if let Some(record) = self.ledger.get(&execution_id) {
            report.reached = Some(record.status);
        }
        report
    }

    /// One transaction of the route, end to end: price → nonce → build → gate → sign →
    /// round-trip → submit → receipt. This is [`crate::stage`]'s recipe, deliberately: a step
    /// of a sequence is not a weaker transaction than a lone one.
    ///
    /// `propagated` is M8.4.4's consumer input: what the previous stage verified about the
    /// block this step pins. It replaces no read — `Build` still asks the endpoint for the
    /// block at its own pin below, which is what makes the comparison an independent one
    /// rather than a copy. This stage deliberately passes no head to the freshness rule: it
    /// reads the block at its pin by number and never asks the endpoint for `latest` (the ban
    /// `crates/pipeline/tests/state_is_always_pinned.rs` enforces), so a live-head context
    /// from upstream has no honest answer here and is refused by name, not accepted on the
    /// producer's timing.
    // The eight arguments are one step's whole recipe; bundling the three run-level ones into a
    // struct would rename every use site without changing what this function reads or judges.
    #[allow(clippy::too_many_arguments)]
    async fn drive_step(
        &mut self,
        plan: &SequencePlan,
        position: usize,
        attempt: &GateAttempt,
        execution_id: &str,
        propagated: Option<&VerifiedBlockContext>,
        report: &mut SequenceReport,
        metrics: &mut Metrics,
    ) -> std::result::Result<TransactionStep, Halt> {
        let step = &plan.steps[position];
        let mut intent = step.intent.clone();
        let step_number = position + 1;

        // §17: one key, one account, and the check runs before any read is paid for.
        if self.signer.mode().may_read_key() {
            let configured = self.signer.address()?;
            if configured != intent.sender {
                return Err(ExecutionError::SigningFailed(format!(
                    "the configured signer ({}) is {configured} and step {step_number} of this \
                     route sends from {}; §17's signer never signs for another account",
                    self.signer.source(),
                    intent.sender
                ))
                .into());
            }
        }

        let fees = self.abilities.fees.clone();
        self.stamp(
            Stage::Build,
            &format!("step {step_number}: fee at pinned block"),
        );
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
        report.sources.push(format!(
            "step {step_number}: fee from {} at block {} ({}), {}",
            reading.provenance,
            intent.block_number.0,
            intent.tx_type.name(),
            intent.fee_summary(),
        ));

        let nonces = self.abilities.nonces.clone();
        self.stamp(
            Stage::Build,
            &format!("step {step_number}: pending and latest nonces"),
        );
        let nonce_reading = nonces.nonce(intent.sender).await?;
        let nonce = self.lane.allocate(&nonce_reading)?;
        intent.nonce = nonce;
        report.sources.push(format!(
            "step {step_number}: nonce {nonce} from {}",
            nonce_reading.source
        ));

        let mut policy = self.setup.build.clone();
        policy.simulated_gas_used = Some(step.simulated_gas_used);
        let build = TransactionBuilder::build(&intent, &policy)?;
        self.rung(execution_id, ExecutionStatus::Built, metrics);
        report.builds.push(StepBuild {
            position,
            identity: BlockIdentity {
                chain_id: ChainId(intent.chain_id),
                number: intent.block_number,
                hash: intent.block_hash,
            },
            serialization_bytes: build.signing_payload.len(),
            fingerprint: build.signing_hash,
            sender_expected: build.sender_expected,
            unsigned: build.unsigned.clone(),
        });

        let chain = self.abilities.chain.clone();
        self.stamp(
            Stage::Build,
            &format!("step {step_number}: gate — endpoint chain id"),
        );
        let endpoint_chain_id = chain.endpoint_chain_id().await?;
        self.stamp(
            Stage::Build,
            &format!("step {step_number}: gate — block binding at pin"),
        );
        let binding = read_binding(&*chain, intent.block_number, intent.block_hash).await;

        // M8.4.4 §6: the consumer's leg, run beside the read above and not instead of it. The
        // three names that have to agree are the propagated context, this step's own pin, and
        // what the endpoint just answered at that height — and the third is the reason this is
        // a verification rather than a receipt of the producer's word (§2 forbids deleting the
        // read that produces it). Nothing is skipped, so no RPC is saved here; what this buys
        // is the disagreement between two stages about one block becoming a named fact
        // (`rejected / block_hash_mismatch`) before a signature, which it was not before.
        let observed = match &binding {
            BlockBinding::Confirmed { number, hash } => Some(BlockIdentity {
                chain_id: ChainId(endpoint_chain_id),
                number: BlockNumber(*number),
                hash: *hash,
            }),
            BlockBinding::Reorged { number, found, .. } => Some(BlockIdentity {
                chain_id: ChainId(endpoint_chain_id),
                number: BlockNumber(*number),
                hash: *found,
            }),
            BlockBinding::Unverified(_) => None,
        };
        let expected = BlockIdentity {
            chain_id: ChainId(intent.chain_id),
            number: intent.block_number,
            hash: intent.block_hash,
        };
        let context_outcome = consumer_check(propagated, &expected, observed.as_ref(), None);
        metrics.bump(&format!(
            "execution_block_context_{}",
            context_outcome.name()
        ));
        report.context_checks.push(StepContextCheck {
            position,
            expected,
            outcome: context_outcome.clone(),
        });
        // §29's named error, on the line that records it: `describe()` is the sentence a reader
        // parses, `name()` is the token a program re-groups by. A record carrying only the
        // sentence cannot be asked *which* refusal fired, and §17 forbids answering that from
        // anything but the raw row.
        let refusal = match &context_outcome {
            ContextOutcome::Rejected { reason, .. } => format!("{}: ", reason.name()),
            _ => String::new(),
        };
        report.sources.push(format!(
            "step {step_number}: block context {} for {}: {}{}",
            context_outcome.name(),
            expected.describe(),
            refusal,
            context_outcome.describe()
        ));

        let fees = self.abilities.fees.clone();
        self.stamp(
            Stage::Build,
            &format!("step {step_number}: gate — native balance"),
        );
        let available = fees.balance(intent.sender, intent.block_number.0).await?;
        let maximum_spend_wei = build.unsigned.maximum_cost_wei()?;
        let source = format!(
            "step {step_number}: native balance of {} read at pinned block {}, against this \
             step's maximum spend of gas_limit × max_fee + value",
            intent.sender, intent.block_number.0
        );
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
        let nonce_evidence = match self.lane.outstanding() {
            Some((lane_sender, lane_nonce))
                if nonce_reading.pending == intent.nonce
                    && lane_sender == intent.sender
                    && lane_nonce == intent.nonce =>
            {
                NonceEvidence::Matches {
                    nonce: intent.nonce,
                    source: nonce_reading.source.clone(),
                }
            }
            _ => NonceEvidence::Differs {
                intent_nonce: intent.nonce,
                pending_nonce: nonce_reading.pending,
                source: nonce_reading.source.clone(),
            },
        };
        let outcome = PreSubmitGate::evaluate(&GateFacts {
            attempt: attempt.clone(),
            intent_chain_id: intent.chain_id,
            configured_chain_id: self.configured_chain_id,
            endpoint_chain_id,
            binding,
            balance,
            nonce: nonce_evidence,
        });
        if !outcome.passed() {
            metrics.bump("execution_gate_blocked");
            return Err(match outcome.error() {
                Some(error) => Halt::Failed(error),
                None => Halt::Failed(ExecutionError::InvalidIntent(outcome.describe())),
            });
        }

        // §20: a mode that may not read a key stops here, on its own terms and not as a
        // failure — the route simply goes no further than the mode allows.
        if !self.signer.mode().may_read_key() {
            return Err(Halt::Blocked(format!(
                "{} stops at Built: step {step_number} has no signature and no bytes (§19, §20)",
                self.signer.mode().name()
            )));
        }
        let (signed, recovered) = self.signer.sign_and_recover(&build.unsigned)?;
        let signed_evidence = SignedTransactionEvidence::from_build(&build, &signed, recovered)
            .map_err(|why| {
                ExecutionError::BuildFailed(format!(
                    "the signature does not cover step {step_number} of this route: {why}"
                ))
            })?;
        if !signed_evidence.sender_matches_expectation() {
            return Err(ExecutionError::SigningFailed(format!(
                "step {step_number}: the signature recovers {} and the build expected {}",
                signed_evidence.recovered_sender, signed_evidence.expected_sender
            ))
            .into());
        }
        self.rung(execution_id, ExecutionStatus::Signed, metrics);
        let local_hash = signed.hash();
        // The record carries one hash and one ladder (§30: one attempt, one record), so it is
        // stamped by the first transaction to reach each rung. Every step's own hash is in the
        // report, and the cost and profit evidence is computed from all of them.
        let record_is_unstamped = self
            .ledger
            .get(execution_id)
            .is_some_and(|record| record.transaction_hash.is_none());
        if record_is_unstamped {
            self.ledger
                .attach_transaction_hash(execution_id, local_hash)?;
        }

        // §15: the bytes decode back to what went in. Before the send, because a codec that
        // drifts is a reason not to send.
        TransactionBuilder::round_trip(&build, &signed)?;

        if !self.abilities.submitter.may_submit() {
            let reason = format!(
                "{} over a {} endpoint: step {step_number} is signed and was never handed to a \
                 node (§20/§24)",
                self.signer.mode().name(),
                self.abilities.submitter.endpoint().name()
            );
            let blocked = SubmissionEvidence::blocked(
                Some(local_hash),
                self.abilities.submitter.endpoint(),
                reason.clone(),
                self.clock.now_ms(),
            );
            report.sources.push(format!(
                "step {step_number}: {} — signed bytes exist and were never sent",
                blocked.outcome
            ));
            return Err(Halt::Blocked(reason));
        }

        let submitter = self.abilities.submitter.clone();
        self.stamp(Stage::Submit, &format!("step {step_number}: raw bytes"));
        let submission = submitter.submit(&signed).await?;
        metrics.bump(&format!(
            "execution_sequence_submission_{}",
            submission.status_word()
        ));
        self.in_flight = true;
        report.lane = self.lane.resolve_submission(&submission);
        let submission_evidence =
            SubmissionEvidence::from_outcome(local_hash, &submission, self.clock.now_ms());
        match submission {
            SubmissionOutcome::Rejected { reason, .. } => {
                return Err(ExecutionError::SubmissionRejected(format!(
                    "step {step_number}: {reason}"
                ))
                .into())
            }
            // §25: this is why the lane is not released and nothing is resent.
            SubmissionOutcome::Unknown { reason, .. } => {
                return Err(Halt::Blocked(format!(
                    "§25 at step {step_number}: the node's answer leaves nonce {nonce} possibly \
                     in flight, so nothing is resent and the lane stays held: {reason}"
                )))
            }
            SubmissionOutcome::Accepted { .. } => {}
        }
        self.rung(execution_id, ExecutionStatus::Submitted, metrics);

        let expected = ExpectedTransaction {
            transaction_hash: local_hash,
            sender: recovered,
            target: Some(intent.target),
            nonce: intent.nonce,
            chain_id: intent.chain_id,
        };
        let receipt_submitter = self.abilities.submitter.clone();
        let receipt_chain = self.abilities.chain.clone();
        self.stamp(
            Stage::Receipt,
            &format!("step {step_number}: receipt and canonical binding"),
        );
        let tracked = ReceiptTracker::new(self.setup.receipts)
            .track(
                &expected,
                {
                    let hash = local_hash;
                    move || {
                        let submitter = receipt_submitter.clone();
                        async move {
                            submitter
                                .receipt(hash)
                                .await
                                .map_err(|error| error.to_string())
                        }
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
        let (receipt, status) = match tracked {
            TrackedReceipt::Included(receipt) => (receipt, ReceiptStatus::Included),
            TrackedReceipt::Reverted(receipt) => (receipt, ReceiptStatus::Reverted),
            TrackedReceipt::Pending {
                attempts,
                last_answer,
            } => {
                return Err(Halt::Blocked(format!(
                    "§26 at step {step_number}: no receipt after {attempts} attempts \
                     ({last_answer}); the transaction may still land, so nothing is resent"
                )))
            }
            TrackedReceipt::Unbound { receipt, reason } => {
                return Err(ExecutionError::ReceiptBinding(format!(
                    "step {step_number}: {reason}; the receipt in block {} is not evidence about \
                     a canonical block",
                    receipt.block_number
                ))
                .into())
            }
        };
        report.lane = self.lane.resolve_receipt(status);
        if record_is_unstamped {
            if let Some(owner) = self.ledger.attach_receipt(&receipt, self.clock.now_ms())? {
                if let Some(record) = self.ledger.get(&owner) {
                    meter(metrics, Some(ExecutionStatus::Submitted), record);
                }
            }
        }
        let cost = ExecutionCostEvidence::from_receipt(&receipt)?;
        report.sources.push(cost.describe());
        let submission_evidence = submission_evidence.with_receipt(&receipt);
        let executed = TransactionStep {
            position,
            signature: step.signature.clone(),
            nonce: intent.nonce,
            gas_limit: build.gas_limit,
            transaction_hash: local_hash,
            block_number: receipt.block_number,
            block_hash: receipt.block_hash,
            receipt_status: status,
            signed: signed_evidence,
            submission: submission_evidence,
            cost,
            logs: receipt.logs.clone(),
        };
        if status == ReceiptStatus::Reverted {
            // §P and §34 together: the chain ran it and the call failed, so the route stops —
            // but the transaction stays in the report. Its gas was paid and its logs are what
            // the wallet actually moved; dropping the step would make the settlement pass bill
            // a route for less than it cost.
            let error = ExecutionError::TransactionReverted {
                transaction_hash: format!("{:#x}", receipt.transaction_hash),
                block_number: receipt.block_number,
            };
            report.transactions.push(executed);
            return Err(error.into());
        }
        Ok(executed)
    }

    /// §19–§24 after every transaction the run has: the second snapshot, the audits, the bill,
    /// the profit equation.
    ///
    /// A run that stopped early comes through here too — that is §34's requirement that a
    /// failed arbitrage get a complete record, and the reason the before-snapshot was taken
    /// before step 0 rather than after the last one.
    async fn settle(
        &mut self,
        plan: &SequencePlan,
        report: &mut SequenceReport,
        metrics: &mut Metrics,
    ) {
        let Some(before) = report.before.clone() else {
            return;
        };
        let Some(last) = report.transactions.last() else {
            if report.detail.is_empty() {
                report.detail = format!(
                    "no transaction of this {}-step route reached a receipt, so no asset delta \
                     is claimed",
                    plan.len()
                );
            }
            metrics.bump("execution_sequence_unsettled");
            return;
        };
        if before.block_number >= last.block_number {
            report.detail = format!(
                "§20's binding is unusable: the before-snapshot is pinned to block {} and the \
                 route's last receipt is in block {}, so the 'before' read is not a picture of \
                 the state the transaction moved from",
                before.block_number, last.block_number
            );
            metrics.bump("execution_sequence_unsettled");
            return;
        }
        let pin = SnapshotPin {
            block_number: last.block_number,
            block_hash: last.block_hash,
        };
        self.stamp(
            Stage::Settlement,
            "after-snapshot: native and input-token balances",
        );
        let after = match self.snapshot(plan, pin.block_number, pin.block_hash).await {
            Ok(after) => after,
            Err(error) => {
                report.detail = format!("the after-snapshot was not read: {error}");
                return;
            }
        };
        let delta = match BalanceDelta::new(before, after) {
            Ok(delta) => delta,
            Err(error) => {
                report.detail = error.to_string();
                return;
            }
        };
        report.after = Some(delta.after.clone());

        let logs = report.logs();
        let (flows, undecodable) = transfer_flows(&logs);
        if undecodable > 0 {
            report.sources.push(format!(
                "{undecodable} Transfer log(s) in this run's receipts did not decode, so the \
                 flow audit sees fewer flows than the chain emitted"
            ));
        }
        let (swaps, swap_undecodable) = swap_observations(&logs);
        if swap_undecodable > 0 {
            report.sources.push(format!(
                "{swap_undecodable} Swap log(s) did not decode and are not counted as legs of \
                 the route"
            ));
        }
        let (wraps, wrap_undecodable) = wrap_moves(&logs, plan.input_token);
        if wrap_undecodable > 0 {
            report.sources.push(format!(
                "{wrap_undecodable} Deposit/Withdrawal log(s) of {} did not decode, so the wrap \
                 side of the flow audit is short by that many moves",
                plan.input_token
            ));
        }
        report.flows = flows;
        report.wraps = wraps;
        report.flow_checks =
            reconcile_flows(&report.flows, Some(&delta), plan.sender, &report.wraps);
        // §21: a token whose logs and balances do not reconcile is stated in the report, not
        // left as a field only a JSON reader will notice. Each kind of failure says something
        // different, so each gets its own sentence.
        let mut gaps: Vec<String> = Vec::new();
        for check in report.flow_checks.iter().filter(|check| !check.agrees) {
            metrics.bump("execution_sequence_flow_unreconciled");
            let gap = if check.balance_read {
                format!(
                    "moved {} net by log (minted {}, burned {}) while its balance changed by {} \
                     between blocks {} and {}; the difference is not absorbed",
                    check.log_net,
                    check.minted,
                    check.burned,
                    check.balance_delta,
                    delta.before.block_number,
                    delta.after.block_number
                )
            } else {
                "appears in the receipts' Transfer logs but no snapshot read its balance, so the \
                 flow audit has one side and cannot reconcile it"
                    .to_string()
            };
            gaps.push(format!("flow audit: {} {}", check.token, gap));
        }
        report.sources.extend(gaps);
        let route = audit_route(&plan.pools, &swaps, plan.sender);
        if !route.passed {
            metrics.bump("execution_sequence_route_incomplete");
        }
        report.route = Some(route);

        let cost = match SequenceCost::new(
            report
                .transactions
                .iter()
                .map(|step| step.cost.clone())
                .collect(),
        ) {
            Ok(cost) => cost,
            Err(error) => {
                report.detail = error.to_string();
                return;
            }
        };
        report.cost = Some(cost.clone());
        self.audit_deltas(plan, report, &delta, &cost);
        // §18/§47: the delta audit is the finding, and it outranks the profit statement — a
        // run whose numbers disagree with its own simulation is not made profitable by the
        // equation closing on the balances.
        if let Err(error) = report.deltas.outcome() {
            metrics.bump("execution_sequence_mismatch");
            report.mismatch = Some(error.clone());
            report.detail = format!("{}; {error}", report.detail);
        }
        match profit_evidence(plan, &delta, &cost, &report.flows, &report.wraps) {
            Ok(accounting) => {
                let status = accounting.evidence.status;
                metrics.bump(&format!("execution_sequence_profit_{}", status.name()));
                report.sources.push(accounting.evidence.describe());
                if let Some(execution_id) = report.execution_id.clone() {
                    let outcome = ExecutionOutcome {
                        route_transactions: cost.transaction_count,
                        input_asset: plan.input_token,
                        input_amount: plan.input_amount,
                        gross_output: accounting.gross_output,
                        gross_profit: accounting.evidence.realized.gross_profit,
                        gas_used: report
                            .transactions
                            .iter()
                            .map(|step| step.cost.gas_used)
                            .sum(),
                        l2_fee: cost.l2_fee_total,
                        l1_fee: cost.l1_fee_total,
                        total_fee: cost.total_execution_cost,
                        realized_profit: accounting.evidence.realized.net_profit,
                        profit_status: status,
                    };
                    let _ = self.ledger.attach_outcome(&execution_id, &outcome);
                }
                report.profit = Some(accounting.evidence);
                self.settled_rungs(report, status, metrics);
            }
            Err(error) => {
                metrics.bump("execution_sequence_profit_unprovable");
                report.detail = format!("{}; {error}", report.detail);
                // The audits ran to the end and only the profit statement is missing, which
                // is §54's `Settled` with no verdict above it: the record stops one rung short
                // rather than claiming the number §56 refuses to count.
                self.settled_rungs(report, ProfitVerificationStatus::Pending, metrics);
            }
        }
    }

    /// §54's last two rungs, climbed at the end of a settlement that completed.
    ///
    /// `Settled` says the snapshots, the flow audit, the route audit, the bill and §39's
    /// equation were all computed for this attempt. `ProfitVerified` says that work produced a
    /// **final** verdict (§56's `Pending` and `Inconclusive` do not earn the rung), and it is
    /// deliberately silent about which way the verdict points — that is
    /// [`crate::lifecycle::ExecutionRecord::realized_profit`] and
    /// [`crate::lifecycle::ExecutionRecord::profit_status`], so a loss and a win climb the
    /// same ladder and cannot be told apart by status alone.
    fn settled_rungs(
        &mut self,
        report: &SequenceReport,
        status: ProfitVerificationStatus,
        metrics: &mut Metrics,
    ) {
        let Some(execution_id) = report.execution_id.clone() else {
            return;
        };
        self.rung(&execution_id, ExecutionStatus::Settled, metrics);
        if status.is_final() {
            self.rung(&execution_id, ExecutionStatus::ProfitVerified, metrics);
        }
    }

    /// §18's lines, filled from the plan's measurements and this run's own evidence.
    fn audit_deltas(
        &self,
        plan: &SequencePlan,
        report: &mut SequenceReport,
        delta: &BalanceDelta,
        cost: &SequenceCost,
    ) {
        let mid_expected = plan.measurement(Binding::SenderMidReceived);
        let mid_actual = report
            .flows
            .iter()
            .filter(|flow| {
                flow.from == plan.sender
                    && plan.pools.contains(&flow.to)
                    && flow.token != plan.input_token
            })
            .fold(U256::ZERO, |total, flow| total + flow.amount);
        report.deltas.push(
            "mid token handed from the wallet to the selling venue",
            mid_expected,
            mid_actual,
            &format!(
                "expected is the simulation's `{}` measurement; actual is the Transfer logs \
                 leaving {} for a venue, in a token that is not the route's input",
                Binding::SenderMidReceived.name(),
                plan.sender
            ),
        );
        let output_actual =
            token_from_venues(&report.flows, plan.sender, plan.input_token, &plan.pools);
        report.deltas.push(
            "route's input token received back from the selling venue",
            plan.measurement(Binding::SenderInputEnd),
            output_actual,
            &format!(
                "expected is the simulation's `{}` measurement against an input of {} {}; \
                 actual is the Transfer logs arriving at {} from a venue, in {}",
                Binding::SenderInputEnd.name(),
                plan.input_amount,
                plan.input_token,
                plan.sender,
                plan.input_token
            ),
        );
        report.deltas.push(
            "gas used across the sequence",
            Some(U256::from(plan.expected_gas_used)),
            U256::from(
                report
                    .transactions
                    .iter()
                    .map(|step| step.cost.gas_used)
                    .sum::<u64>(),
            ),
            &format!(
                "expected is the simulated plan's total of {} gas units; actual is gas_used over \
                 {} bound receipt(s). The bill is the L2 + L1 total of {} wei and §12's profit \
                 uses that, not this comparison",
                plan.expected_gas_used, cost.transaction_count, cost.total_execution_cost
            ),
        );
        report.deltas.push(
            "native balance of the wallet at the end of the route",
            plan.measurement(Binding::SenderNativeEnd),
            delta.after.native_wei,
            &format!(
                "expected is the simulation's `{}` measurement, read as the purse the wallet \
                 started with plus what the route settled back minus the input and the simulated \
                 gas bill; actual is eth_getBalance at block {}. The two are not meant to be \
                 equal, and the known difference is sized rather than hand-waved: the simulated \
                 bill carries no L1 data fee, and the chain charged {} wei of it across {} \
                 transaction(s), so a shortfall of about that size is this line's expected shape \
                 and anything beyond the tolerance is state that moved for a reason outside this \
                 run",
                Binding::SenderNativeEnd.name(),
                delta.after.block_number,
                cost.l1_fee_total,
                cost.transaction_count,
            ),
        );
    }

    /// Both snapshots go through the same three surfaces, so a "before" and an "after" that
    /// came from different endpoints could never be compared in the first place.
    async fn snapshot(
        &self,
        plan: &SequencePlan,
        block_number: u64,
        block_hash: B256,
    ) -> Result<AssetSnapshot> {
        read_snapshot(
            &*self.abilities.fees,
            &*self.assets,
            plan.sender,
            &SnapshotPin {
                block_number,
                block_hash,
            },
            &plan.tokens,
        )
        .await
    }

    /// Climb the shared record, ignoring a rung it is already on or past: the ladder is
    /// forward-only (§30's single record), and a later step reaching `Built` again is not a
    /// reason to stop sending the route.
    fn rung(&mut self, execution_id: &str, to: ExecutionStatus, metrics: &mut Metrics) {
        let Some(from) = self.ledger.get(execution_id).map(|record| record.status) else {
            return;
        };
        if !ExecutionStatus::can_follow(from, to) {
            return;
        }
        if let Ok(previous) = self.ledger.advance(execution_id, to, self.clock.now_ms()) {
            if let Some(record) = self.ledger.get(execution_id) {
                meter(metrics, Some(previous), record);
            }
        }
    }

    /// A refusal: the record ends `Failed` with §39's reason, and a lane this run never put
    /// bytes on goes back (§11). Bytes that did reach a node are §25's, and the lane keeps the
    /// nonce.
    fn stop(
        &mut self,
        report: &mut SequenceReport,
        execution_id: &str,
        error: &ExecutionError,
        metrics: &mut Metrics,
    ) {
        metrics.bump("execution_sequence_stopped");
        if report.detail.is_empty() {
            report.detail = error.to_string();
        } else {
            report.detail = format!("{} ({error})", report.detail);
        }
        if self
            .ledger
            .get(execution_id)
            .is_some_and(|record| !record.status.terminal())
        {
            let _ = self.ledger.fail(execution_id, error, self.clock.now_ms());
        }
        if !self.in_flight {
            report.lane = self.lane.release_unsent();
        }
    }

    /// A stop that is not a failure (§20's mode, §24's endpoint, §25's unknown answer, §26's
    /// budget): the record keeps its rung and gains the reason (§24).
    fn block(
        &mut self,
        report: &mut SequenceReport,
        execution_id: &str,
        reason: &str,
        metrics: &mut Metrics,
    ) {
        metrics.bump("execution_sequence_blocked");
        if report.detail.is_empty() {
            report.detail = reason.to_string();
        }
        let _ = self.ledger.block(execution_id, reason);
        if !self.in_flight {
            report.lane = self.lane.release_unsent();
        }
    }
}
