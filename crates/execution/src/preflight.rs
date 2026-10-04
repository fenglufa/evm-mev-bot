//! §26/§27: the last gate before a signature, and the reason it cannot look around.
//!
//! M6's [`crate::gate`] answers seven legs. §26 asks for thirteen, and the six it adds are
//! all about *this moment*: the head the chain is on now, the reserves the pools hold now,
//! the price for gas now, the L1 charge that will be applied now, the aggregate the wallet
//! must cover across a whole sequence rather than one transaction, and the asset the route
//! actually spends. A run that signed on M6's seven legs alone would be signing on a
//! decision that had not yet asked whether the market it was priced against still exists.
//!
//! §27 fixes what the gate may do with a changed market: **Reject**. Not re-search, not
//! re-optimize, not quietly try the next candidate. The type makes that the only available
//! move — [`ExecutionPreflight::run`] takes facts about exactly one attempt and returns a
//! verdict about that attempt; there is no collection of candidates anywhere in this module,
//! and the report can only ever name the one it was handed.
//!
//! The re-pricing is the substantive line. It uses M3's own
//! ([`evm_opportunity::math`]) formula and the fee fractions measured in
//! `data/evidence/m7/candidate-fee-measurement.json` — never an assumed 0.30% — over the
//! reserves read at the head, and it subtracts the *ceiling of the whole sequence including
//! the L1 estimate* (§10) before comparing with the risk layer's floor. A route that is
//! still profitable on the old reserves and not on the new ones is a stale opportunity, and
//! §31's taxonomy entry is what this reports.
//!
//! Like the gate, this module reads nothing. Every field of [`PreflightFacts`] is a value
//! the caller obtained from the endpoint, and a value that was not obtained arrives as an
//! explicit `Unread`/`Unverified` — which fails its line, because §26's "全部通过才允许
//! Sign" has no third state to award a missing measurement.

use alloy_primitives::{Address, B256, I256, U256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId, Fee, PoolId};

use crate::block_context::{
    BlockContextScope, BlockIdentity, ContextRefusal, ProducerOutcome, VerifiedBlockContext,
};
use crate::cost::EstimatedCost;
use crate::error::{ExecutionError, Result};
use crate::fee::FeeReading;
use crate::gate::{
    BalanceEvidence, BlockBinding, GateCheck, GateFacts, GateOutcome, NonceEvidence, PreSubmitGate,
};
use crate::intent::{SenderFunding, TransactionIntent};

/// §26's thirteen lines, in the order the task book lists them, plus the one §34 makes
/// mandatory before a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightCheck {
    /// §6's three-way claim, answered by [`crate::gate`].
    ChainId,
    /// The head this run is acting against, read now. Without it the reserves below are
    /// quoted from an unknown moment.
    CurrentHead,
    /// The intent's block is still canonical at that height — [`crate::gate`]'s binding.
    OpportunityBlock,
    /// The hash the intent pins is the hash the endpoint holds there: the same read as the
    /// line above, answering the half §26 names separately.
    OpportunityBlockHash,
    /// §26's reserve line, answered by re-pricing (§27).
    PoolReserves,
    /// `simulation.success`, answered by [`crate::gate`].
    SimulationResult,
    /// `risk.decision == Accept`, answered by [`crate::gate`].
    RiskDecision,
    /// §11's pending view, answered by [`crate::gate`].
    Nonce,
    /// §33's balance leg, and §26's sequence-wide half of it: the wallet must cover every
    /// step's ceiling, not only the step the gate priced.
    NativeBalance,
    /// The asset the route actually spends (§26's "input token balance"), which for a
    /// native-funded route is the same asset as gas and for a token-funded route is not.
    InputAssetBalance,
    /// §13's traceability: a limit for every step that had a measurement.
    GasEstimate,
    /// The gas price the transaction will carry, read at the head it will be sent into.
    FeeEstimate,
    /// §10/§35's line: the L1 half of the bill is priced, not assumed to be zero.
    L1FeeEstimate,
    /// §34: an intent made payable by a state override may be built and never signed.
    RealStateFunding,
}

impl PreflightCheck {
    pub fn name(self) -> &'static str {
        match self {
            Self::ChainId => "chain_id",
            Self::CurrentHead => "current_head",
            Self::OpportunityBlock => "opportunity_block",
            Self::OpportunityBlockHash => "opportunity_block_hash",
            Self::PoolReserves => "pool_reserves",
            Self::SimulationResult => "simulation_result",
            Self::RiskDecision => "risk_decision",
            Self::Nonce => "nonce",
            Self::NativeBalance => "native_balance",
            Self::InputAssetBalance => "input_token_balance",
            Self::GasEstimate => "gas_estimate",
            Self::FeeEstimate => "fee_estimate",
            Self::L1FeeEstimate => "l1_fee_estimate",
            Self::RealStateFunding => "real_state_funding",
        }
    }

    /// Whether the answer comes from M6's gate rather than from a read this module makes.
    /// Recorded so the report can say which legs were newly measured and which were
    /// inherited — a reviewer checking §57 D needs that distinction, not a flat list.
    pub fn answered_by_m6_gate(self) -> bool {
        matches!(
            self,
            Self::ChainId
                | Self::OpportunityBlock
                | Self::SimulationResult
                | Self::RiskDecision
                | Self::Nonce
        )
    }
}

/// The chain's present head, or the reason it is unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadReading {
    Read {
        number: u64,
        hash: B256,
        source: String,
    },
    Unread(String),
}

impl HeadReading {
    pub fn number(&self) -> Option<u64> {
        match self {
            Self::Read { number, .. } => Some(*number),
            Self::Unread(_) => None,
        }
    }
}

/// One pool's reserves, read twice: as the opportunity was priced on, and as they are now.
///
/// Both halves are kept because §27's question is a *difference*. Quoting only the current
/// reserves would let a report say "the market moved" without saying from where, and
/// quoting only the priced ones would not be a check at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveReading {
    pub pool: PoolId,
    pub leg_index: usize,
    /// The reserves the route was priced against, copied from the intent's own provenance
    /// rather than remembered by the caller.
    pub priced_reserve_in: U256,
    pub priced_reserve_out: U256,
    /// What `eth_call getReserves()` answers at [`PreflightFacts::head`].
    pub current_reserve_in: Option<U256>,
    pub current_reserve_out: Option<U256>,
    pub source: String,
}

impl ReserveReading {
    fn current(&self) -> Option<(U256, U256)> {
        match (self.current_reserve_in, self.current_reserve_out) {
            (Some(in_), Some(out)) => Some((in_, out)),
            _ => None,
        }
    }
}

/// The re-pricing of exactly this route at the head's reserves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repricing {
    pub input_amount: U256,
    /// §5's "fee proven": the retained fraction measured per leg, in M3's ratio form.
    /// An assumed 997/1000 here would be a fabrication, and the evidence string is what
    /// lets a reader check it.
    pub first_fee: Fee,
    pub second_fee: Fee,
    pub fee_evidence: String,
    /// What the simulation measured back at the opportunity block — §18's expected side of
    /// the eventual comparison, and §27's reference point for "the opportunity changed".
    pub priced_output_wei: U256,
    /// The risk layer's floor, carried in from the intent rather than re-read from
    /// configuration (§26: verify this opportunity, do not re-decide it).
    pub minimum_required_profit_wei: U256,
}

impl Repricing {
    /// What the same two legs would pay on `first` and `second` reserves.
    ///
    /// `swap_exact_in` twice — the formula M3 used to find the opportunity — so the only
    /// thing that can make this answer differ from the recorded one is the reserves, which
    /// is exactly the variable §27 allows the gate to look at.
    pub fn expected_output(&self, first: &ReserveReading, second: &ReserveReading) -> Result<U256> {
        let (first_in, first_out) = first.current().ok_or_else(|| {
            ExecutionError::ChainRead(format!(
                "pool {} was not re-read at the head: §27's verdict cannot be formed from \
                 one side of the comparison",
                first.pool.address
            ))
        })?;
        let (second_in, second_out) = second.current().ok_or_else(|| {
            ExecutionError::ChainRead(format!(
                "pool {} was not re-read at the head",
                second.pool.address
            ))
        })?;
        let mid =
            evm_opportunity::swap_exact_in(first_in, first_out, self.first_fee, self.input_amount)
                .map_err(|error| {
                    ExecutionError::ChainRead(format!(
                        "the first leg at {} cannot be re-priced: {error}",
                        first.pool.address
                    ))
                })?;
        evm_opportunity::swap_exact_in(second_in, second_out, self.second_fee, mid).map_err(
            |error| {
                ExecutionError::ChainRead(format!(
                    "the second leg at {} cannot be re-priced: {error}",
                    second.pool.address
                ))
            },
        )
    }
}

/// The asset the route spends first, and whether the wallet holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputAssetEvidence {
    /// The route begins by wrapping native ETH, so this line is answered by the same read
    /// as §26's native balance — with the amount the first step actually moves named, so
    /// the two lines are not the same sentence twice.
    Native {
        required_wei: U256,
        available_wei: U256,
        source: String,
    },
    /// The route spends an ERC-20 the wallet must already hold. A route funded this way and
    /// a route funded by `deposit()` are different risks, and §26 asks them separately.
    Token {
        token: Address,
        required_amount: U256,
        available_amount: U256,
        source: String,
    },
    Unread(String),
}

/// One step of the sequence: what it will cost at its ceiling, and what the simulation
/// measured it consuming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepPricing {
    pub index: usize,
    pub to: Address,
    pub cost: EstimatedCost,
    /// `ExecutedStep::gas_used` from the simulation. `None` means the step was never
    /// simulated, and §13 forbids signing a limit that traces to nothing.
    pub simulated_gas_used: Option<u64>,
}

/// The whole sequence's ceilings, added once here so the balance line and the report agree
/// on what "the wallet must cover" means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequencePricing {
    pub steps: Vec<StepPricing>,
    pub total_ceiling_wei: U256,
    pub total_l1_estimate_wei: U256,
}

impl SequencePricing {
    pub fn new(steps: Vec<StepPricing>) -> Result<Self> {
        if steps.is_empty() {
            return Err(ExecutionError::Evidence(
                "a preflight over zero steps is not a preflight; §26's gate exists to stop a \
                 signature, and there is nothing to stop here"
                    .to_string(),
            ));
        }
        let mut total_ceiling_wei = U256::ZERO;
        let mut total_l1_estimate_wei = U256::ZERO;
        for step in &steps {
            total_ceiling_wei = total_ceiling_wei
                .checked_add(step.cost.total_ceiling_wei)
                .ok_or_else(|| {
                    ExecutionError::Evidence(format!(
                        "step {}'s ceiling does not fit beside the earlier ones",
                        step.index
                    ))
                })?;
            total_l1_estimate_wei = total_l1_estimate_wei
                .checked_add(step.cost.l1_fee_estimate)
                .ok_or_else(|| {
                    ExecutionError::Evidence(format!(
                        "step {}'s L1 estimate does not fit beside the earlier ones",
                        step.index
                    ))
                })?;
        }
        Ok(Self {
            steps,
            total_ceiling_wei,
            total_l1_estimate_wei,
        })
    }

    /// §13: every step's limit beside the gas the simulation measured for it.
    fn gas_line(&self) -> (bool, String) {
        let mut problems: Vec<String> = Vec::new();
        for step in &self.steps {
            if step.cost.gas_limit == 0 {
                problems.push(format!("step {} has a zero gas limit", step.index));
            }
            match step.simulated_gas_used {
                None => problems.push(format!(
                    "step {} at {} carries no simulated measurement, so its limit is an \
                     assumption and §13 refuses it",
                    step.index, step.to
                )),
                Some(measured) if measured > step.cost.gas_limit => problems.push(format!(
                    "step {}'s limit {} is below the {} gas the simulation measured for it",
                    step.index, step.cost.gas_limit, measured
                )),
                Some(_) => {}
            }
        }
        if problems.is_empty() {
            (
                true,
                format!(
                    "{} step(s), each limit at or above the gas its simulation step measured \
                     (total limit {} gas)",
                    self.steps.len(),
                    self.steps.iter().map(|s| s.cost.gas_limit).sum::<u64>()
                ),
            )
        } else {
            (false, problems.join("; "))
        }
    }

    fn l1_line(&self) -> (bool, String) {
        let unpriced: Vec<usize> = self
            .steps
            .iter()
            .filter(|step| !step.cost.includes_l1())
            .map(|step| step.index)
            .collect();
        if unpriced.is_empty() {
            (
                true,
                format!(
                    "every step's ceiling includes an L1 estimate; sequence total {} wei \
                     (source: {})",
                    self.total_l1_estimate_wei,
                    self.steps[0].cost.l1_fee_source.describe()
                ),
            )
        } else {
            (
                false,
                format!(
                    "step(s) {:?} carry no L1 estimate, so their ceilings are the L2 half \
                     only — §35 forbids reading that as the whole bill",
                    unpriced
                ),
            )
        }
    }
}

/// Everything the thirteen lines are answered from.
#[derive(Clone, Debug)]
pub struct PreflightFacts<'a> {
    /// §27: the one attempt under examination. An identifier, not a candidate set.
    pub attempt_id: String,
    pub intent: &'a TransactionIntent,
    /// M6's seven legs, evaluated by [`crate::gate`] and folded into five of §26's lines.
    pub gate: GateFacts,
    pub head: HeadReading,
    /// The two legs' reserves, in route order.
    pub reserves: [ReserveReading; 2],
    pub repricing: Repricing,
    pub pricing: SequencePricing,
    pub fee: &'a FeeReading,
    pub input_asset: InputAssetEvidence,
    /// What the transaction will actually carry, so the fee line can compare the signed
    /// number against the priced one instead of trusting that the stage copied it.
    pub signed_max_fee_per_gas: Option<U256>,
    /// M8.4.4 §27: which read answered for the block this preflight's pin names. The gather
    /// that made the read is the only place that knows its provenance, so it fills this in;
    /// [`ExecutionPreflight::run`] uses it as a label on the context it verifies, never as
    /// evidence of the identity itself.
    pub block_context_source: String,
    /// §28's diagnostic stamp: when this gather finished the read behind the pin's identity.
    /// It cannot be an identity, cannot stand in for a hash and cannot prove freshness, and it
    /// is deliberately absent from every evidence row.
    pub block_context_verified_at_ms: u64,
}

/// One line's answer, with the read that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreflightFinding {
    pub check: PreflightCheck,
    pub passed: bool,
    pub detail: String,
}

/// §26's verdict, in the report shape §57 D and §61's preflight section need.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreflightReport {
    pub attempt_id: String,
    pub findings: Vec<PreflightFinding>,
    pub passed: bool,
    /// §27's re-priced output and the net it implies, kept next to the verdict so a reader
    /// sees the numbers the gate used rather than only its conclusion.
    pub repriced_output_wei: Option<U256>,
    pub expected_net_after_costs_wei: Option<I256>,
    /// The first failing line's reason, in §39's taxonomy words.
    pub rejected_because: Option<String>,
    pub sequence_ceiling_wei: U256,
    /// M8.4.4 §4/§5: the block identity this stage verified from its own read — the one value
    /// in this report that is allowed to cross into the next stage — or the named reason it
    /// refused to issue one.
    ///
    /// This is the carrier M8.4.3 recorded as missing: before it, the only thing that travelled
    /// from `Preflight` to `Build` was
    /// [`crate::sequence::SnapshotPin`], which names the *head* and proves nothing about it.
    /// The field cannot be a bare number (§25) and cannot be a timestamp (§28); it is chain,
    /// height, hash, and the read that proved them together.
    pub block_context: ProducerOutcome,
}

impl PreflightReport {
    pub fn failed_checks(&self) -> Vec<PreflightCheck> {
        self.findings
            .iter()
            .filter(|finding| !finding.passed)
            .map(|finding| finding.check)
            .collect()
    }

    /// §39's taxonomy entry for this refusal, from the first failing line in declaration
    /// order. The gate's own mapping is reused for the legs it answers, so a stale
    /// opportunity is a stale opportunity here and there.
    pub fn error(&self) -> Option<ExecutionError> {
        let failing = self.findings.iter().find(|f| !f.passed)?;
        Some(match failing.check {
            PreflightCheck::ChainId => ExecutionError::ChainMismatch(failing.detail.clone()),
            PreflightCheck::PoolReserves | PreflightCheck::CurrentHead => {
                ExecutionError::StaleOpportunity(failing.detail.clone())
            }
            PreflightCheck::NativeBalance | PreflightCheck::InputAssetBalance => {
                ExecutionError::InsufficientBalance(failing.detail.clone())
            }
            PreflightCheck::Nonce => ExecutionError::NonceUnavailable(failing.detail.clone()),
            PreflightCheck::OpportunityBlock | PreflightCheck::OpportunityBlockHash => {
                // The same mapping the gate uses for `block_binding_valid`: a height that no
                // longer holds the pinned block is the opportunity going stale under the run,
                // not a receipt problem.
                ExecutionError::StaleOpportunity(failing.detail.clone())
            }
            PreflightCheck::RealStateFunding => {
                ExecutionError::OverrideDependent(failing.detail.clone())
            }
            _ => ExecutionError::InvalidIntent(failing.detail.clone()),
        })
    }

    pub fn describe(&self) -> String {
        if self.passed {
            return format!(
                "preflight passed for {}: {} lines, ceiling {} wei, re-priced output {}",
                self.attempt_id,
                self.findings.len(),
                self.sequence_ceiling_wei,
                self.repriced_output_wei
                    .map(|out| out.to_string())
                    .unwrap_or_else(|| "?".to_string())
            );
        }
        let failed = self
            .findings
            .iter()
            .filter(|f| !f.passed)
            .map(|f| f.check.name())
            .collect::<Vec<_>>()
            .join(", ");
        format!("preflight REJECTED {failed}")
    }
}

/// §26's gate. It decides, and it decides only about the attempt it is handed (§27).
pub struct ExecutionPreflight;

impl ExecutionPreflight {
    /// Evaluate the fourteen lines in declaration order and return all of them, not just
    /// the first failure: §61's report has to say which of the thirteen were answered, and
    /// a caller that only learned the first one could not tell a stale market from a broke
    /// wallet without re-running reads.
    pub fn run(facts: &PreflightFacts) -> PreflightReport {
        let mut findings: Vec<PreflightFinding> = Vec::new();
        let mut push = |check: PreflightCheck, passed: bool, detail: String| {
            findings.push(PreflightFinding {
                check,
                passed,
                detail,
            });
        };

        // The five legs M6's gate already answers, folded in by name so a blocked leg fails
        // the §26 line it belongs to rather than being reported as a separate verdict.
        let gate_outcome = PreSubmitGate::evaluate(&facts.gate);
        let gate_failures = match &gate_outcome {
            GateOutcome::Passed => Vec::new(),
            GateOutcome::Blocked { failures } => failures.clone(),
        };
        let gate_failure = |check: GateCheck| -> Option<String> {
            gate_failures
                .iter()
                .find(|failure| failure.check == check)
                .map(|failure| failure.reason.clone())
        };

        // §26 is the gate in front of a *real opportunity*. A validation attempt has no
        // simulation, no risk decision and no opportunity behind it, so on those three legs
        // there is no answer here to verify — they are refused rather than awarded to a
        // caller that set the fields to whatever the run needed.
        let opportunity_leg = |check: GateCheck, fallback: String| -> (bool, String) {
            if !facts.gate.attempt.is_arbitrage() {
                return (
                    false,
                    format!(
                        "this attempt is {}, which has no {} to verify; §26's preflight is \
                         the arbitrage gate",
                        facts.gate.attempt.describe(),
                        check.name()
                    ),
                );
            }
            match gate_failure(check) {
                Some(reason) => (false, reason),
                None => (true, fallback),
            }
        };

        let (chain_ok, chain_detail) = match gate_failure(GateCheck::ChainMatches) {
            Some(reason) => (false, reason),
            None => (
                true,
                format!(
                    "the intent's, the configured and the endpoint's chain id all say {}: \
                     eth_chainId answered for the node",
                    facts.gate.endpoint_chain_id
                ),
            ),
        };
        push(PreflightCheck::ChainId, chain_ok, chain_detail);

        let head_detail = match &facts.head {
            HeadReading::Read {
                number,
                hash,
                source,
            } => {
                let intent_block = facts.intent.block_number.0;
                if *number < intent_block {
                    (
                        false,
                        format!(
                            "{source}: the head is {number} and the intent pins {intent_block}, \
                             so this endpoint is behind the state the opportunity was found on"
                        ),
                    )
                } else {
                    (
                        true,
                        format!(
                            "head {number} ({hash:#x}) from {source}, at or past the \
                                pinned block {intent_block}"
                        ),
                    )
                }
            }
            HeadReading::Unread(why) => (false, format!("the head was never read: {why}")),
        };
        push(PreflightCheck::CurrentHead, head_detail.0, head_detail.1);

        // §31's staleness has no line of its own in §26's list but the gate answers it, so
        // it is folded into the opportunity-block line: an opportunity the lifecycle has
        // declared stale is exactly one whose block is no longer actionable, and dropping
        // that failure would let a preflight pass over a gate refusal.
        let stale = gate_failure(GateCheck::OpportunityFresh);
        let (block_ok, block_detail) = match (&facts.gate.binding, stale) {
            (BlockBinding::Confirmed { number, hash }, None) => (
                true,
                format!("eth_getBlockByNumber still names a canonical block {number} ({hash:#x})"),
            ),
            (BlockBinding::Confirmed { number, hash }, Some(reason)) => (
                false,
                format!(
                    "block {number} ({hash:#x}) is canonical but this run may not act on it: \
                     {reason}"
                ),
            ),
            (
                BlockBinding::Reorged {
                    number,
                    pinned,
                    found,
                },
                _,
            ) => (
                false,
                format!("block {number} is now {found:#x}, not the pinned {pinned:#x}"),
            ),
            (BlockBinding::Unverified(why), _) => (false, format!("not verified: {why}")),
        };
        push(PreflightCheck::OpportunityBlock, block_ok, block_detail);

        // §26 names the hash separately from the block, and this line answers only the hash
        // question: whether the pinned bytes are still the bytes at that height. A reorg
        // fails both lines and a stale run fails only the one above, and a report that
        // conflated them could not tell those two apart.
        let (hash_ok, hash_detail) = match &facts.gate.binding {
            BlockBinding::Confirmed { hash, .. } if *hash == facts.intent.block_hash => (
                true,
                format!(
                    "the intent pins {:#x} at block {} and the endpoint's block hash there is \
                     the same",
                    facts.intent.block_hash, facts.intent.block_number.0
                ),
            ),
            BlockBinding::Confirmed { hash, .. } => (
                false,
                format!(
                    "the intent pins {:#x} at block {} and the endpoint holds {hash:#x} there",
                    facts.intent.block_hash, facts.intent.block_number.0
                ),
            ),
            BlockBinding::Reorged { found, .. } => (
                false,
                format!(
                    "the intent pins {:#x} at block {} and the height now holds {found:#x}",
                    facts.intent.block_hash, facts.intent.block_number.0
                ),
            ),
            BlockBinding::Unverified(why) => (
                false,
                format!(
                    "the intent pins {:#x} at block {} and no read confirmed that height: {why}",
                    facts.intent.block_hash, facts.intent.block_number.0
                ),
            ),
        };
        push(PreflightCheck::OpportunityBlockHash, hash_ok, hash_detail);

        let (sim_ok, sim_detail) = opportunity_leg(
            GateCheck::SimulationSucceeded,
            format!(
                "the run this intent came from (simulation {:#x}) completed successfully",
                facts.intent.ids.simulation_id
            ),
        );
        push(PreflightCheck::SimulationResult, sim_ok, sim_detail);

        let (risk_ok, risk_detail) = opportunity_leg(
            GateCheck::RiskAccepted,
            format!(
                "the risk layer's decision on this run is Accept (decision {:#x})",
                facts.intent.ids.risk_decision_id
            ),
        );
        push(PreflightCheck::RiskDecision, risk_ok, risk_detail);

        let nonce_pass = match &facts.gate.nonce {
            NonceEvidence::Matches { nonce, source } => {
                format!(
                    "{source}: the account's pending nonce is {nonce}, which is the nonce \
                         the intent carries"
                )
            }
            NonceEvidence::Differs {
                intent_nonce,
                pending_nonce,
                source,
            } => format!(
                "{source}: the intent carries {intent_nonce} and the pending view is \
                 {pending_nonce}"
            ),
            NonceEvidence::Unverified(why) => format!("the nonce was not read: {why}"),
        };
        let (nonce_ok, nonce_detail) = match gate_failure(GateCheck::NonceValid) {
            Some(reason) => (false, reason),
            None => (true, nonce_pass),
        };
        push(PreflightCheck::Nonce, nonce_ok, nonce_detail);

        // §26's native balance line is the sequence's, not one transaction's: the gate's
        // leg answers §33 for the intent it was given, and six steps of ceiling are a
        // different question.
        let balance_line = match &facts.gate.balance {
            BalanceEvidence::Sufficient {
                available_wei,
                maximum_spend_wei,
                source,
            } => {
                if *available_wei < facts.pricing.total_ceiling_wei {
                    (
                        false,
                        format!(
                            "{source}: {available_wei} wei available and the sequence ceiling \
                             is {} wei across {} step(s) (the single intent the gate checked \
                             needed only {maximum_spend_wei})",
                            facts.pricing.total_ceiling_wei,
                            facts.pricing.steps.len()
                        ),
                    )
                } else {
                    (
                        true,
                        format!(
                            "{source}: {available_wei} wei covers the sequence ceiling of {} \
                             wei (L2 ceilings + value + L1 estimates over {} steps)",
                            facts.pricing.total_ceiling_wei,
                            facts.pricing.steps.len()
                        ),
                    )
                }
            }
            BalanceEvidence::Insufficient {
                available_wei,
                maximum_spend_wei,
                source,
            } => (
                false,
                format!(
                    "{source}: {available_wei} wei available, {maximum_spend_wei} wei is \
                     already required by the single intent the gate priced, before any of the \
                     {} wei the rest of the sequence would need",
                    facts.pricing.total_ceiling_wei
                ),
            ),
            BalanceEvidence::Unverified(why) => {
                (false, format!("the real balance was not read: {why}"))
            }
        };
        push(
            PreflightCheck::NativeBalance,
            balance_line.0,
            balance_line.1,
        );

        let asset_line = match &facts.input_asset {
            InputAssetEvidence::Native {
                required_wei,
                available_wei,
                source,
            } => (
                available_wei >= required_wei,
                format!(
                    "{source}: the route opens by wrapping {required_wei} wei of native and \
                     the account holds {available_wei}"
                ),
            ),
            InputAssetEvidence::Token {
                token,
                required_amount,
                available_amount,
                source,
            } => (
                available_amount >= required_amount,
                format!(
                    "{source}: the route spends {required_amount} of {token} and the account \
                     holds {available_amount}"
                ),
            ),
            InputAssetEvidence::Unread(why) => {
                (false, format!("the input asset was not read: {why}"))
            }
        };
        push(
            PreflightCheck::InputAssetBalance,
            asset_line.0,
            asset_line.1,
        );

        let gas_line = facts.pricing.gas_line();
        push(PreflightCheck::GasEstimate, gas_line.0, gas_line.1);

        let fee_line = match (facts.head.number(), facts.signed_max_fee_per_gas) {
            (Some(head), Some(signed)) => {
                if facts.fee.block_number != head {
                    (
                        false,
                        format!(
                            "the fee was read for block {} and the head is now {head}; §31's \
                             rule is that a price is only valid for the state it names",
                            facts.fee.block_number
                        ),
                    )
                } else if facts.fee.max_fee_per_gas.is_zero() {
                    (
                        false,
                        "the fee reading is zero, which prices nothing".to_string(),
                    )
                } else if signed < facts.fee.max_fee_per_gas {
                    (
                        false,
                        format!(
                            "the transaction carries max_fee_per_gas {signed} and the fee \
                             read at block {head} says {} — the base fee has risen since this \
                             plan was priced, so the run is underpriced for the block it would \
                             land in *and* the ceiling its profit was costed against is below \
                             what it would actually pay",
                            facts.fee.max_fee_per_gas
                        ),
                    )
                } else {
                    // The plan is priced from the pinned block and the gate re-reads at the
                    // head, so on a chain whose base fee moves every block the two agree only
                    // by luck. Equality is therefore not the requirement — the requirement is
                    // the direction the money actually moves in: the transaction pays at most
                    // its own ceiling, the cost model used that same ceiling, so a head reading
                    // below it means the net figure is still a floor, never a promise.
                    let moved = if signed == facts.fee.max_fee_per_gas {
                        "the two readings agree exactly".to_string()
                    } else {
                        format!(
                            "the head's own ceiling is {}, {} wei/gas below the transaction's \
                             — the base fee fell between the block this plan was priced at and \
                             the head, so the run pays at most what it was costed at",
                            facts.fee.max_fee_per_gas,
                            signed.saturating_sub(facts.fee.max_fee_per_gas)
                        )
                    };
                    (
                        true,
                        format!(
                            "head block {head}: {}; the transaction's own ceiling is \
                             max_fee_per_gas {signed}, and {} — the sequence ceiling the \
                             balance and profit lines use is computed from {signed}, the worse \
                             of the two",
                            facts.fee.provenance, moved
                        ),
                    )
                }
            }
            (None, _) => (
                false,
                "the fee cannot be pinned to a head that was not read; the \
                                  reading is for an unknown block"
                    .to_string(),
            ),
            (Some(_), None) => (
                false,
                "a legacy-priced intent with no max_fee_per_gas: there is no ceiling to price"
                    .to_string(),
            ),
        };
        push(PreflightCheck::FeeEstimate, fee_line.0, fee_line.1);

        let l1_line = facts.pricing.l1_line();
        push(PreflightCheck::L1FeeEstimate, l1_line.0, l1_line.1);

        let funding_line = match &facts.intent.funding {
            SenderFunding::RealState { source } => (
                true,
                format!(
                    "§34: {source} — eth_getBalance and eth_call see the funding this plan \
                     spends, so nothing about this run depends on a state override"
                ),
            ),
            SenderFunding::Overridden { detail } => (
                false,
                format!(
                    "§34: this run was only payable because of a state override ({detail}); \
                     it may be built and simulated and must never be signed"
                ),
            ),
        };
        push(
            PreflightCheck::RealStateFunding,
            funding_line.0,
            funding_line.1,
        );

        // §27's substantive line, computed last so its numbers can be shown beside the
        // other twelve even when they are the ones that fail.
        let (reserves_ok, reserves_detail, repriced, expected_net) = match facts
            .repricing
            .expected_output(&facts.reserves[0], &facts.reserves[1])
        {
            Err(error) => (false, error.to_string(), None, None),
            Ok(output) => {
                let net = I256::from_raw(output)
                    - I256::from_raw(facts.repricing.input_amount)
                    - I256::from_raw(facts.pricing.total_ceiling_wei);
                // How far the market moved, in the same unit as the amounts: §27's
                // "the opportunity changed" has to be a number a reader can check, not
                // an adjective.
                let moved =
                    I256::from_raw(output) - I256::from_raw(facts.repricing.priced_output_wei);
                let clears = net > I256::ZERO
                    && unsigned(net) >= facts.repricing.minimum_required_profit_wei;
                (
                    clears,
                    format!(
                        "{} and {} re-read at the head: the same two legs now pay \
                             {output} wei on an input of {}, against the {} wei the priced \
                             simulation measured — the route moved by {moved} wei. Net of the \
                             sequence ceiling of {} wei is {net}, and the risk floor this \
                             attempt was accepted against is {} wei. Fees used: {}/{} + {}/{} \
                             — {}",
                        facts.reserves[0].source,
                        facts.reserves[1].source,
                        facts.repricing.input_amount,
                        facts.repricing.priced_output_wei,
                        facts.pricing.total_ceiling_wei,
                        facts.repricing.minimum_required_profit_wei,
                        facts.repricing.first_fee.numerator,
                        facts.repricing.first_fee.denominator,
                        facts.repricing.second_fee.numerator,
                        facts.repricing.second_fee.denominator,
                        facts.repricing.fee_evidence
                    ),
                    Some(output),
                    Some(net),
                )
            }
        };
        push(PreflightCheck::PoolReserves, reserves_ok, reserves_detail);

        let block_context = verify_block_context(facts);
        findings.sort_by_key(|finding| finding.check);
        let failed: Vec<&PreflightFinding> = findings.iter().filter(|f| !f.passed).collect();
        PreflightReport {
            attempt_id: facts.attempt_id.clone(),
            passed: failed.is_empty(),
            rejected_because: failed.first().map(|f| f.detail.clone()),
            findings,
            repriced_output_wei: repriced,
            expected_net_after_costs_wei: expected_net,
            sequence_ceiling_wei: facts.pricing.total_ceiling_wei,
            block_context,
        }
    }
}

/// M8.4.4 §5: the producer's leg, run on the reads this gather already paid for.
///
/// Three facts have to line up before anything may be propagated: which chain the intent is
/// for, which chain the endpoint answers for, and whether the block the endpoint holds at the
/// pinned height is the pinned block. The first two are what `gate`'s `chain_matches` leg
/// already compares and the third is what `opportunity_block_hash` already answers — this
/// function does not add a read or re-decide a leg, it turns the answers that are already in
/// `facts` into the one value that may cross the stage boundary, and records the named refusal
/// when they do not line up (§30: a rejection is a fact, not an empty field).
///
/// The scope is read off the head rather than declared: a pin the endpoint still calls head is
/// §8's live-head case, and a pin the chain has moved past is a fixed historical block, and
/// the two have different freshness rules downstream.
fn verify_block_context(facts: &PreflightFacts) -> ProducerOutcome {
    let head = match &facts.head {
        HeadReading::Read { number, .. } => *number,
        HeadReading::Unread(why) => {
            return ProducerOutcome::Refused(ContextRefusal::UnverifiableBlockContext(format!(
                "the head was never read, so this pin cannot even be said to be historical or \
                 live: {why}"
            )))
        }
    };
    let (number, hash) = match &facts.gate.binding {
        BlockBinding::Confirmed { number, hash } => (*number, *hash),
        BlockBinding::Reorged { number, found, .. } => (*number, *found),
        BlockBinding::Unverified(why) => {
            return ProducerOutcome::Refused(ContextRefusal::UnverifiableBlockContext(why.clone()))
        }
    };
    let expected = BlockIdentity {
        chain_id: ChainId(facts.gate.intent_chain_id),
        number: facts.intent.block_number,
        hash: facts.intent.block_hash,
    };
    let observed = BlockIdentity {
        chain_id: ChainId(facts.gate.endpoint_chain_id),
        number: BlockNumber(number),
        hash,
    };
    VerifiedBlockContext::verify(
        &expected,
        &observed,
        if head == facts.intent.block_number.0 {
            BlockContextScope::LiveHead
        } else {
            BlockContextScope::FixedHistorical
        },
        facts.block_context_source.clone(),
        "preflight §26 `block binding at pin` and `eth_chainId` legs".to_string(),
        facts.block_context_verified_at_ms,
    )
    .map_or_else(ProducerOutcome::Refused, ProducerOutcome::Verified)
}

/// The magnitude of a signed total. `I256::abs` is not available for the value range used
/// here without a wrap, and a floor that is compared against a magnitude must not be
/// compared against a two's-complement rendering of a negative.
fn unsigned(net: I256) -> U256 {
    if net.is_negative() {
        U256::ZERO
    } else {
        net.into_raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::L1FeeSource;
    use crate::gate::GateAttempt;
    use crate::intent::ExecutionIds;
    use alloy_primitives::Bytes;
    use evm_core::{BlockNumber, ChainId};

    const WETH: Address = Address::with_last_byte(0x06);
    const POOL_A: Address = Address::with_last_byte(0x0a);
    const POOL_B: Address = Address::with_last_byte(0x0b);
    const SENDER: Address = Address::with_last_byte(0xd4);

    /// §48's first real principal: 0.0001 ETH.
    const INPUT: u128 = 100_000_000_000_000;
    /// base 362 + 2*362 + tip 1e6, the fee M6 measured and M7 re-reads at the head.
    const MAX_FEE: u64 = 1_000_401;
    const STEP_GAS: u64 = 200_000;
    /// One step's `gas_limit × max_fee_per_gas`.
    const GAS_CEILING: u128 = 200_080_200_000;
    const L1_ESTIMATE: u64 = 7_000_000_000;
    /// The deposit step also moves `INPUT`, so the sequence ceiling is two gas ceilings
    /// plus the principal plus two L1 estimates.
    const SEQUENCE_CEILING: u128 = 100_414_160_400_000;
    /// What `INPUT` comes back as through the reserves in [`clearing_reserves`], computed
    /// from the V2 formula by hand: 90_661_089_388_014_913_158 middle tokens out of pool A,
    /// then that amount through pool B.
    const REPRICED_OUTPUT: u128 = 864_806_517_171_626;
    const NET_AFTER_COSTS: i128 = 664_392_356_771_626;
    const FLOOR: u128 = 1_000_000_000_000;

    fn leg_fee() -> Fee {
        Fee {
            numerator: 997_000,
            denominator: 1_000_000,
        }
    }

    fn oracle() -> L1FeeSource {
        L1FeeSource::OracleEstimate {
            amount: U256::from(L1_ESTIMATE),
            block_number: 37_530_600,
            read_by: "eth_call getL1Fee(bytes) at the GasPriceOracle predeploy".to_string(),
        }
    }

    fn intent() -> TransactionIntent {
        TransactionIntent {
            ids: ExecutionIds {
                opportunity_id: "attempt-under-test".to_string(),
                simulation_id: B256::repeat_byte(0x11),
                risk_decision_id: B256::repeat_byte(0x22),
            },
            chain_id: 91_342,
            block_number: BlockNumber(37_530_593),
            block_hash: B256::repeat_byte(0x33),
            state_fingerprint: "37530593/0".to_string(),
            sender: SENDER,
            target: WETH,
            value: U256::from(INPUT),
            calldata: Bytes::new(),
            nonce: 1,
            gas_limit: STEP_GAS,
            tx_type: crate::tx::TransactionType::DynamicFee,
            max_fee_per_gas: Some(U256::from(MAX_FEE)),
            max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
            access_list: Vec::new(),
            simulation_profit_wei: Some(U256::from(3_100_463_872_989_758u128)),
            minimum_required_profit_wei: U256::from(FLOOR),
            funding: SenderFunding::RealState {
                source: "eth_getBalance@pending plus the deposit() the plan executes".to_string(),
            },
            simulated_steps: 2,
            sequence: None,
        }
    }

    /// Only the step that wraps native ETH into WETH can carry `value`; a swap step pays
    /// its input token from the balance the previous step created.
    fn step(index: usize, to: Address, value: u128, simulated: Option<u64>) -> StepPricing {
        StepPricing {
            index,
            to,
            cost: EstimatedCost::new(STEP_GAS, U256::from(MAX_FEE), U256::from(value), oracle())
                .unwrap(),
            simulated_gas_used: simulated,
        }
    }

    fn priced_steps() -> SequencePricing {
        SequencePricing::new(vec![
            step(0, WETH, INPUT, Some(180_000)),
            step(1, POOL_A, 0, Some(120_000)),
        ])
        .unwrap()
    }

    fn reserve(pool: Address, leg: usize, in_: u128, out: u128) -> ReserveReading {
        ReserveReading {
            pool: PoolId {
                chain_id: ChainId(91_342),
                address: pool,
            },
            leg_index: leg,
            priced_reserve_in: U256::from(in_),
            priced_reserve_out: U256::from(out),
            current_reserve_in: Some(U256::from(in_)),
            current_reserve_out: Some(U256::from(out)),
            source: format!("eth_call getReserves() at block 37530600 for {pool:#x}"),
        }
    }

    /// The route §16's ideal shape describes: `WETH → pool A → middle token → pool B →
    /// WETH`, with pool A holding 1e15 WETH against 1e21 middle and pool B holding 2e21
    /// middle against 2e16 WETH. Both legs retain 3 000 ppm.
    fn clearing_reserves() -> [ReserveReading; 2] {
        [
            reserve(
                POOL_A,
                0,
                1_000_000_000_000_000,
                1_000_000_000_000_000_000_000,
            ),
            reserve(
                POOL_B,
                1,
                2_000_000_000_000_000_000_000,
                20_000_000_000_000_000,
            ),
        ]
    }

    fn fee_reading(block: u64) -> FeeReading {
        FeeReading {
            chain_id: 91_342,
            block_number: block,
            block_hash: B256::repeat_byte(0x44),
            base_fee_per_gas: Some(U256::from(362u64)),
            suggested_tip_wei: Some(U256::from(1_000_000u64)),
            max_fee_per_gas: U256::from(MAX_FEE),
            max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
            kinds: vec![
                crate::fee::FeeSourceKind::PinnedBlockBaseFee,
                crate::fee::FeeSourceKind::NodeSuggestedTip,
            ],
            provenance: "base 362 + 2*362 + tip 1000000 = max_fee 1000401 wei/gas at block \
                        37530600"
                .to_string(),
        }
    }

    fn build_facts<'a>(
        intent: &'a TransactionIntent,
        reserves: [ReserveReading; 2],
        pricing: SequencePricing,
        fee: &'a FeeReading,
    ) -> PreflightFacts<'a> {
        PreflightFacts {
            attempt_id: "attempt-under-test".to_string(),
            intent,
            gate: GateFacts {
                attempt: GateAttempt::Arbitrage {
                    simulation_success: true,
                    risk_accepted: true,
                    freshness: crate::gate::Freshness::Active,
                },
                intent_chain_id: 91_342,
                configured_chain_id: 91_342,
                endpoint_chain_id: 91_342,
                binding: BlockBinding::Confirmed {
                    number: 37_530_593,
                    hash: intent.block_hash,
                },
                balance: BalanceEvidence::Sufficient {
                    available_wei: U256::from(20_000_000_000_000_000u128),
                    maximum_spend_wei: U256::from(GAS_CEILING + INPUT),
                    source: "eth_getBalance(pending)".to_string(),
                },
                nonce: NonceEvidence::Matches {
                    nonce: intent.nonce,
                    source: "eth_getTransactionCount(pending)".to_string(),
                },
            },
            head: HeadReading::Read {
                number: 37_530_600,
                hash: B256::repeat_byte(0x44),
                source: "eth_getBlockByNumber(canonical head)".to_string(),
            },
            reserves,
            repricing: Repricing {
                input_amount: U256::from(INPUT),
                first_fee: leg_fee(),
                second_fee: leg_fee(),
                fee_evidence: "data/evidence/m7/candidate-fee-measurement.json — 3000 ppm \
                              retained on all four legs, bisected to a 1-wei bracket"
                    .to_string(),
                priced_output_wei: U256::from(REPRICED_OUTPUT),
                minimum_required_profit_wei: U256::from(FLOOR),
            },
            pricing,
            fee,
            input_asset: InputAssetEvidence::Native {
                required_wei: U256::from(INPUT),
                available_wei: U256::from(20_000_000_000_000_000u128),
                source: "eth_getBalance(pending)".to_string(),
            },
            signed_max_fee_per_gas: intent.max_fee_per_gas,
            block_context_source: "eth_getBlockByNumber(37530593) and eth_chainId, read as the \
                                   `block binding at pin` leg"
                .to_string(),
            block_context_verified_at_ms: 1_700_000_000_000,
        }
    }

    fn line(report: &PreflightReport, check: PreflightCheck) -> &PreflightFinding {
        report
            .findings
            .iter()
            .find(|finding| finding.check == check)
            .unwrap_or_else(|| panic!("{} was not answered", check.name()))
    }

    fn failures(report: &PreflightReport) -> String {
        report
            .findings
            .iter()
            .filter(|finding| !finding.passed)
            .map(|finding| format!("{}: {}", finding.check.name(), finding.detail))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    #[test]
    fn a_route_that_still_clears_the_floor_passes_and_every_line_names_its_read() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let report = ExecutionPreflight::run(&build_facts(
            &intent,
            clearing_reserves(),
            priced_steps(),
            &fee,
        ));
        assert!(report.passed, "{}", failures(&report));
        assert_eq!(report.findings.len(), 14);
        assert!(report.rejected_because.is_none());
        for finding in &report.findings {
            assert!(
                !finding.detail.is_empty(),
                "{} has no reason",
                finding.check.name()
            );
            assert!(
                ["0x", "eth_", "wei", "block", "gas", "step"]
                    .iter()
                    .any(|token| finding.detail.contains(token)),
                "{}: {} names no read",
                finding.check.name(),
                finding.detail
            );
        }

        // The numbers the verdict was formed on are in the report, not only in its prose.
        assert_eq!(report.sequence_ceiling_wei, U256::from(SEQUENCE_CEILING));
        assert_eq!(
            report.repriced_output_wei,
            Some(U256::from(REPRICED_OUTPUT)),
            "{}",
            line(&report, PreflightCheck::PoolReserves).detail
        );
        assert_eq!(
            report.expected_net_after_costs_wei,
            Some(I256::from_raw(U256::from(NET_AFTER_COSTS.unsigned_abs())))
        );

        // §26's thirteen lines are present in full and in its own order, and the five the
        // M6 gate already answers say so.
        assert_eq!(
            report
                .findings
                .iter()
                .map(|f| f.check.name())
                .collect::<Vec<_>>(),
            vec![
                "chain_id",
                "current_head",
                "opportunity_block",
                "opportunity_block_hash",
                "pool_reserves",
                "simulation_result",
                "risk_decision",
                "nonce",
                "native_balance",
                "input_token_balance",
                "gas_estimate",
                "fee_estimate",
                "l1_fee_estimate",
                "real_state_funding",
            ]
        );
        let inherited: Vec<&str> = report
            .findings
            .iter()
            .filter(|f| f.check.answered_by_m6_gate())
            .map(|f| f.check.name())
            .collect();
        assert_eq!(
            inherited,
            vec![
                "chain_id",
                "opportunity_block",
                "simulation_result",
                "risk_decision",
                "nonce"
            ]
        );
    }

    #[test]
    fn reserves_that_no_longer_clear_the_floor_reject_and_quote_both_numbers() {
        // Pool B's WETH side has been drained from 2e16 to 1e15 since the opportunity was
        // priced: the same two legs now pay less than the principal, let alone the costs.
        // §27's only permitted answer is Reject, and it has to name both figures.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut reserves = clearing_reserves();
        reserves[1].current_reserve_out = Some(U256::from(1_000_000_000_000_000u128));
        let report = ExecutionPreflight::run(&build_facts(&intent, reserves, priced_steps(), &fee));
        assert!(!report.passed);
        assert_eq!(report.failed_checks(), vec![PreflightCheck::PoolReserves]);
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::StaleOpportunity(_)
        ));
        let detail = &line(&report, PreflightCheck::PoolReserves).detail;
        assert!(detail.contains("864806517171626"), "{detail}");
        assert!(detail.contains("43240325858581"), "{detail}");
        assert!(detail.contains("3000 ppm"), "{detail}");
        assert_eq!(
            report.repriced_output_wei,
            Some(U256::from(43_240_325_858_581u128))
        );
        assert!(report.expected_net_after_costs_wei.unwrap().is_negative());
        // §27: the verdict is about the one attempt it was handed and offers no alternative.
        assert_eq!(report.attempt_id, "attempt-under-test");
        assert_eq!(report.rejected_because.as_deref(), Some(detail.as_str()));
    }

    #[test]
    fn an_unread_reserve_blocks_instead_of_falling_back_to_the_priced_numbers() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut reserves = clearing_reserves();
        reserves[1].current_reserve_in = None;
        reserves[1].current_reserve_out = None;
        let report = ExecutionPreflight::run(&build_facts(&intent, reserves, priced_steps(), &fee));
        assert!(!report.passed);
        let detail = &line(&report, PreflightCheck::PoolReserves).detail;
        assert!(detail.contains("was not re-read"), "{detail}");
        assert_eq!(report.repriced_output_wei, None);
    }

    #[test]
    fn an_override_funded_intent_never_reaches_a_signature() {
        let mut intent = intent();
        intent.funding = SenderFunding::Overridden {
            detail: "balance override for 0x953e7e98… at step 0".to_string(),
        };
        let fee = fee_reading(37_530_600);
        let report = ExecutionPreflight::run(&build_facts(
            &intent,
            clearing_reserves(),
            priced_steps(),
            &fee,
        ));
        assert!(report
            .failed_checks()
            .contains(&PreflightCheck::RealStateFunding));
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::OverrideDependent(_)
        ));
    }

    #[test]
    fn a_stale_opportunity_fails_the_block_line_instead_of_passing_on_a_canonical_hash() {
        // The gate answers §31's staleness and §26 has no line named for it, so folding that
        // failure away would let a preflight pass over a gate refusal.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.attempt = GateAttempt::Arbitrage {
            simulation_success: true,
            risk_accepted: true,
            freshness: crate::gate::Freshness::Stale {
                reason: "the state engine moved to block 37530601".to_string(),
            },
        };
        let report = ExecutionPreflight::run(&facts);
        assert!(!line(&report, PreflightCheck::OpportunityBlock).passed);
        let detail = &line(&report, PreflightCheck::OpportunityBlock).detail;
        assert!(detail.contains("37530601"), "{detail}");
        assert!(detail.contains("may not act on it"), "{detail}");
        // The legs the staleness says nothing about keep their answers.
        assert!(line(&report, PreflightCheck::SimulationResult).passed);
        assert!(line(&report, PreflightCheck::PoolReserves).passed);
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::StaleOpportunity(_)
        ));
    }

    #[test]
    fn a_reorg_fails_both_block_lines_and_is_a_stale_opportunity_not_a_receipt_problem() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.binding = BlockBinding::Reorged {
            number: 37_530_593,
            pinned: intent.block_hash,
            found: B256::repeat_byte(0x55),
        };
        let report = ExecutionPreflight::run(&facts);
        assert!(!line(&report, PreflightCheck::OpportunityBlock).passed);
        assert!(!line(&report, PreflightCheck::OpportunityBlockHash).passed);
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::StaleOpportunity(_)
        ));
    }

    #[test]
    fn a_validation_attempt_has_no_opportunity_legs_for_the_preflight_to_verify() {
        // §26 is the gate in front of a real opportunity. Handing it §35's controlled
        // transaction would otherwise read the caller's own booleans as answers.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.attempt = GateAttempt::Validation {
            label: "M6 §35 controlled validation".to_string(),
        };
        let report = ExecutionPreflight::run(&facts);
        assert!(!line(&report, PreflightCheck::SimulationResult).passed);
        assert!(!line(&report, PreflightCheck::RiskDecision).passed);
        assert!(line(&report, PreflightCheck::ChainId).passed);
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::InvalidIntent(_)
        ));
    }

    #[test]
    fn a_step_without_a_simulated_measurement_fails_the_gas_line() {
        // §13: a limit that traces to a measurement. A step nobody simulated has no such
        // trace, whatever the number looks like.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut steps = priced_steps();
        steps.steps[1].simulated_gas_used = None;
        let report =
            ExecutionPreflight::run(&build_facts(&intent, clearing_reserves(), steps, &fee));
        let line = line(&report, PreflightCheck::GasEstimate);
        assert!(!line.passed);
        assert!(
            line.detail.contains("no simulated measurement"),
            "{}",
            line.detail
        );
    }

    #[test]
    fn a_limit_below_the_measured_gas_is_a_refusal_not_a_margin() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut steps = priced_steps();
        steps.steps[0].simulated_gas_used = Some(210_000);
        let report =
            ExecutionPreflight::run(&build_facts(&intent, clearing_reserves(), steps, &fee));
        let line = line(&report, PreflightCheck::GasEstimate);
        assert!(!line.passed, "{}", line.detail);
        assert!(line.detail.contains("below the 210000"), "{}", line.detail);
    }

    #[test]
    fn a_price_read_at_a_block_that_is_not_the_head_is_not_a_price() {
        let intent = intent();
        // The fee was priced at the opportunity block while the head has moved on: §31's
        // rule is that a price is only valid for the state it names.
        let fee = fee_reading(37_530_593);
        let report = ExecutionPreflight::run(&build_facts(
            &intent,
            clearing_reserves(),
            priced_steps(),
            &fee,
        ));
        let line = line(&report, PreflightCheck::FeeEstimate);
        assert!(!line.passed, "{}", line.detail);
        assert!(
            line.detail.contains("the head is now 37530600"),
            "{}",
            line.detail
        );
    }

    #[test]
    fn a_transaction_cheaper_than_the_head_now_demands_is_refused() {
        // The direction that costs money: the plan was priced at a base fee that has since
        // risen, so its ceiling is now below what the head would charge. Such a run can sit
        // unmined while the window closes, and the profit was costed against a ceiling lower
        // than what it would actually pay.
        let mut intent = intent();
        intent.max_fee_per_gas = Some(U256::from(1_000_400u64));
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.signed_max_fee_per_gas = intent.max_fee_per_gas;
        let report = ExecutionPreflight::run(&facts);
        let line = line(&report, PreflightCheck::FeeEstimate);
        assert!(!line.passed, "{}", line.detail);
        assert!(
            line.detail
                .contains("the base fee has risen since this plan was priced"),
            "{}",
            line.detail
        );
    }

    #[test]
    fn a_ceiling_above_the_head_reading_passes_and_says_which_number_is_costed() {
        // The other direction is the ordinary one on a chain that recomputes its base fee
        // every block: the plan carries the ceiling from the block it was priced at, the head
        // has since gotten cheaper, and the transaction still pays at most the ceiling the
        // profit was costed against. Requiring the two numbers to be equal here would refuse
        // every real run on noise.
        let mut intent = intent();
        intent.max_fee_per_gas = Some(U256::from(1_000_700u64));
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.signed_max_fee_per_gas = intent.max_fee_per_gas;
        let report = ExecutionPreflight::run(&facts);
        let line = line(&report, PreflightCheck::FeeEstimate);
        assert!(line.passed, "{}", line.detail);
        assert!(
            line.detail
                .contains("the transaction's own ceiling is max_fee_per_gas 1000700"),
            "{}",
            line.detail
        );
        assert!(
            line.detail.contains("299 wei/gas below the transaction's"),
            "{}",
            line.detail
        );
        assert!(
            line.detail
                .contains("computed from 1000700, the worse of the two"),
            "{}",
            line.detail
        );
    }

    #[test]
    fn an_unpriced_l1_step_stops_the_run_before_anything_is_signed() {
        // §35's ban on assuming the L1 fee is zero, applied at the gate rather than in the
        // accounting afterwards, where it would be too late.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut steps = priced_steps();
        steps.steps[0].cost = EstimatedCost::new(
            STEP_GAS,
            U256::from(MAX_FEE),
            U256::from(INPUT),
            L1FeeSource::Unreadable {
                reason: "GasPriceOracle returned no data".to_string(),
            },
        )
        .unwrap();
        let report =
            ExecutionPreflight::run(&build_facts(&intent, clearing_reserves(), steps, &fee));
        let line = line(&report, PreflightCheck::L1FeeEstimate);
        assert!(!line.passed, "{}", line.detail);
        assert!(line.detail.contains("L2 half only"), "{}", line.detail);
    }

    #[test]
    fn a_wallet_that_covers_one_step_and_not_the_sequence_fails_the_balance_line() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        // Enough for the single intent the gate priced, not for two steps of ceiling.
        facts.gate.balance = BalanceEvidence::Sufficient {
            available_wei: U256::from(GAS_CEILING + INPUT + 100_000_000_000u128),
            maximum_spend_wei: U256::from(GAS_CEILING + INPUT),
            source: "eth_getBalance(pending)".to_string(),
        };
        let report = ExecutionPreflight::run(&facts);
        let line = line(&report, PreflightCheck::NativeBalance);
        assert!(!line.passed, "{}", line.detail);
        assert!(line.detail.contains("sequence ceiling"), "{}", line.detail);
        assert!(matches!(
            report.error().unwrap(),
            ExecutionError::InsufficientBalance(_)
        ));
    }

    #[test]
    fn a_token_funded_route_is_checked_against_the_token_it_spends() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.input_asset = InputAssetEvidence::Token {
            token: WETH,
            required_amount: U256::from(1_000_000u64),
            available_amount: U256::from(999_999u64),
            source: "eth_call balanceOf(WETH)@pending".to_string(),
        };
        let report = ExecutionPreflight::run(&facts);
        let line = line(&report, PreflightCheck::InputAssetBalance);
        assert!(!line.passed, "{}", line.detail);
        assert!(line.detail.contains("holds 999999"), "{}", line.detail);
    }

    #[test]
    fn the_producer_attaches_the_identity_its_own_read_verified() {
        // §5's six steps, run by the preflight itself: the header read, its number, its hash,
        // the identity check against the pin and the chain, and the context on the report. The
        // assertion is on the *values the reads answered*, not on the type's name — §5 warns
        // that a struct called Verified proves nothing.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        let report = ExecutionPreflight::run(&facts);
        let context = match &report.block_context {
            ProducerOutcome::Verified(context) => context,
            other => panic!("the pin's read confirmed, so a context was expected: {other:?}"),
        };
        assert_eq!(context.number(), BlockNumber(37_530_593));
        assert_eq!(context.hash(), intent.block_hash);
        assert_eq!(context.chain_id(), ChainId(91_342));
        assert_eq!(context.scope(), BlockContextScope::FixedHistorical);
        // §27's provenance questions, answered in the value rather than in prose beside it.
        assert!(
            context.source().contains("eth_getBlockByNumber(37530593)"),
            "{}",
            context.source()
        );
        assert!(
            context.verified_by().contains("block binding at pin"),
            "{}",
            context.verified_by()
        );
        // §28: the timestamp is a process fact and has no place in the row the evidence is
        // built from — and the row is what the report serializes.
        let row = report.block_context.to_row();
        assert_eq!(row["outcome"].as_str(), Some("verified"));
        assert_eq!(
            row["verified_block"]["block_number"].as_u64(),
            Some(37_530_593)
        );
        assert!(
            row["verified_block"].get("verified_at_ms").is_none(),
            "{row}"
        );
        assert_eq!(context.verified_at_ms(), 1_700_000_000_000);
    }

    /// §35's header test: the context is the header this stage *read*, copied out of that read,
    /// and not the pin restated. The positive half compares the context with the same
    /// `gate.binding` the §26 lines judged, field for field; the negative half edits that very
    /// binding to a hash the intent does not pin, where the producer must refuse rather than
    /// hand the consumer a context that agrees with the intent by construction. A context built
    /// from the intent would pass the first half and fail the second.
    #[test]
    fn the_context_copies_the_header_that_was_read_and_refuses_one_that_disagrees() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        let report = ExecutionPreflight::run(&facts);
        let context = match &report.block_context {
            ProducerOutcome::Verified(context) => context,
            other => panic!("the binding confirmed, so a context was expected: {other:?}"),
        };
        match &facts.gate.binding {
            BlockBinding::Confirmed { number, hash } => {
                assert_eq!(context.number(), BlockNumber(*number));
                assert_eq!(context.hash(), *hash);
            }
            other => panic!("the fixture's binding is a confirmed header: {other:?}"),
        }
        assert_eq!(
            context.chain_id(),
            ChainId(facts.gate.endpoint_chain_id),
            "the chain the endpoint answered for, not the chain the config hoped for"
        );

        let read_hash = B256::repeat_byte(0x77);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.binding = BlockBinding::Confirmed {
            number: 37_530_593,
            hash: read_hash,
        };
        let report = ExecutionPreflight::run(&facts);
        let reason = match &report.block_context {
            ProducerOutcome::Refused(reason) => reason,
            other => {
                panic!("a header that is not the pin must not produce a context: {other:?}")
            }
        };
        assert_eq!(reason.name(), "block_hash_mismatch");
        let detail = reason.describe();
        assert!(detail.contains(&format!("{:#x}", read_hash)), "{detail}");
        assert!(
            detail.contains(&format!("{:#x}", intent.block_hash)),
            "{detail}"
        );
        assert!(report.block_context.context().is_none());
        // §2: the new refusal is one more fact, not a replacement — §26's own hash line still
        // runs over the same read and still fails on it.
        assert!(
            !line(&report, PreflightCheck::OpportunityBlockHash).passed,
            "{}",
            failures(&report)
        );
    }

    #[test]
    fn a_reorg_at_the_pin_refuses_the_context_and_names_the_hash_that_replaced_it() {
        // NC1 at the producer: the height is right and the block is not. The gate already fails
        // §26's two block lines over the same read; this is the refusal that stops the identity
        // from crossing into the next stage.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.binding = BlockBinding::Reorged {
            number: 37_530_593,
            pinned: intent.block_hash,
            found: B256::repeat_byte(0x55),
        };
        let report = ExecutionPreflight::run(&facts);
        let reason = match &report.block_context {
            ProducerOutcome::Refused(reason) => reason,
            other => panic!("a reorged pin must not produce a context: {other:?}"),
        };
        assert_eq!(reason.name(), "block_hash_mismatch");
        assert!(
            reason.describe().contains("0x5555"),
            "{}",
            reason.describe()
        );
        assert!(report.block_context.context().is_none());
    }

    #[test]
    fn a_read_about_a_different_height_refuses_the_context_rather_than_renumbering_it() {
        // NC2 at the producer: same hash, another height. A bare number would have sailed
        // through; the pair cannot, because the hash the read answered belongs to a different
        // height than the pin's claim.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.binding = BlockBinding::Confirmed {
            number: 37_530_594,
            hash: intent.block_hash,
        };
        let report = ExecutionPreflight::run(&facts);
        assert!(matches!(
            &report.block_context,
            ProducerOutcome::Refused(ContextRefusal::BlockNumberMismatch {
                expected: 37_530_593,
                held: 37_530_594
            })
        ));
    }

    #[test]
    fn another_chain_at_the_same_height_refuses_the_context() {
        // NC3 at the producer, and the reason §25 refuses to share a bare number: 37530593 with
        // this hash is a real block somewhere, just not on the chain the intent is for.
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.endpoint_chain_id = 91_343;
        let report = ExecutionPreflight::run(&facts);
        assert!(matches!(
            &report.block_context,
            ProducerOutcome::Refused(ContextRefusal::ChainMismatch {
                expected: 91_342,
                held: 91_343
            })
        ));
        // The refusal is about the identity carrier only; §26's own chain line still carries
        // its own answer, and no leg of the gate was rewritten to make room for this milestone.
        assert!(!line(&report, PreflightCheck::ChainId).passed);
    }

    #[test]
    fn a_read_that_never_answered_leaves_no_context_and_says_so_by_name() {
        // NC4's shape at the producer, and §30's requirement that an absence be reported: an
        // unverified binding and an unread head are two different refusals with two different
        // reasons, and neither arrives as a `null` a reader has to guess about.
        let intent = intent();
        let fee = fee_reading(37_530_600);

        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.gate.binding = BlockBinding::Unverified("the endpoint did not answer".to_string());
        let report = ExecutionPreflight::run(&facts);
        let reason = match &report.block_context {
            ProducerOutcome::Refused(ContextRefusal::UnverifiableBlockContext(reason)) => reason,
            other => panic!("an unverified binding must refuse: {other:?}"),
        };
        assert!(reason.contains("did not answer"), "{reason}");

        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.head = HeadReading::Unread("eth_getBlockByNumber timed out".to_string());
        let report = ExecutionPreflight::run(&facts);
        let reason = match &report.block_context {
            ProducerOutcome::Refused(ContextRefusal::UnverifiableBlockContext(reason)) => reason,
            other => panic!("an unread head must refuse: {other:?}"),
        };
        assert!(reason.contains("head was never read"), "{reason}");
    }

    #[test]
    fn the_scope_is_read_off_the_head_and_never_declared() {
        // §8's two cases are not a caller's choice: the same pin is a live head while the chain
        // is on it and a fixed historical block one header later. Both answers come from the
        // head read, and the freshness rule that follows each is different downstream.
        let intent = intent();
        let fee = fee_reading(37_530_600);

        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.head = HeadReading::Read {
            number: 37_530_593,
            hash: B256::repeat_byte(0x33),
            source: "eth_getBlockByNumber(canonical head)".to_string(),
        };
        let report = ExecutionPreflight::run(&facts);
        assert_eq!(
            report.block_context.context().map(|c| c.scope()),
            Some(BlockContextScope::LiveHead),
            "§21's Case A: the pin is still the head"
        );

        facts.head = HeadReading::Read {
            number: 37_530_594,
            hash: B256::repeat_byte(0x44),
            source: "eth_getBlockByNumber(canonical head)".to_string(),
        };
        let report = ExecutionPreflight::run(&facts);
        assert_eq!(
            report.block_context.context().map(|c| c.scope()),
            Some(BlockContextScope::FixedHistorical),
            "§21's Case B: one header later, the same pin is history"
        );
    }

    #[test]
    fn an_unread_head_or_binding_fails_its_line_rather_than_passing_on_silence() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let mut facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        facts.head = HeadReading::Unread("eth_getBlockByNumber timed out".to_string());
        facts.gate.binding = BlockBinding::Unverified("the endpoint did not answer".to_string());
        let report = ExecutionPreflight::run(&facts);
        let failed = report.failed_checks();
        assert!(failed.contains(&PreflightCheck::CurrentHead), "{failed:?}");
        assert!(failed.contains(&PreflightCheck::OpportunityBlock));
        assert!(failed.contains(&PreflightCheck::OpportunityBlockHash));
        assert!(failed.contains(&PreflightCheck::FeeEstimate));
        assert!(
            line(&report, PreflightCheck::FeeEstimate)
                .detail
                .contains("a head that was not read"),
            "{}",
            line(&report, PreflightCheck::FeeEstimate).detail
        );
    }

    #[test]
    fn the_report_is_the_same_twice_over_the_same_facts() {
        let intent = intent();
        let fee = fee_reading(37_530_600);
        let facts = build_facts(&intent, clearing_reserves(), priced_steps(), &fee);
        assert_eq!(
            ExecutionPreflight::run(&facts),
            ExecutionPreflight::run(&facts),
            "a refusal must be deterministic (§16's habit)"
        );
    }

    #[test]
    fn a_sequence_of_no_steps_is_not_a_passing_preflight() {
        let error = SequencePricing::new(Vec::new()).unwrap_err();
        assert!(matches!(error, ExecutionError::Evidence(_)), "{error}");
    }
}
