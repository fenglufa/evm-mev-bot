//! §26/§27: the risk answer for a multi-hop simulation, and the eleven checks that give it.
//!
//! M4's [`crate::policy::RiskThresholds`] decides over a [`evm_simulation::SimulationResult`] —
//! one priced route, run as a sequence of direct pair calls — and answers three questions: did it
//! run, was it cheap enough, is the net figure big enough. §27 asks a different set of a
//! [`evm_simulation::SimulatedOpportunity`]: the same three ideas plus the ones that only exist
//! once a route has legs, a deployment, a pinned header and a state version. So this module adds
//! a policy rather than stretching M4's, and M4's three rules stay exactly as M7 and M8's
//! evidence files quote them.
//!
//! ## How this list relates to §27's
//!
//! §27 names nine checks. [`RiskCheck`] has eleven, and the two extra are additions of this
//! module, not findings in the task:
//!
//! - §27's `executor validity` is asked as two checks here ([`RiskCheck::ChainValidity`] then
//!   [`RiskCheck::ExecutorValidity`]) because they fail for different reasons and an evidence row
//!   that counts "invalid executor" would count a wrong-chain record with it.
//! - [`RiskCheck::FinalGuard`] re-reads the delivered amount against the floor the call itself
//!   carried. No §27 item covers it; it is here because it is the only check that can notice a
//!   record that has been edited after the run, which is what §30's execution claim rests on.
//!
//! §27 says "至少" (at least), so the extra check is inside the requirement's letter; the split is
//! inside nothing, and is stated here rather than passed off as the task's own list.
//!
//! ```text
//! SimulatedOpportunity  +  MarketFacts (data the caller already has)
//!   -> MultihopRiskPolicy::evaluate   eleven checks, in one fixed order
//!   -> MultihopRiskDecision            Accept / Reject(RiskRejectReason) / Unknown(same)
//!   -> ArbitrageExecutionPlan          (crates/execution, §29/§30 — not this crate)
//! ```
//!
//! ## Why every rejection is a named check
//!
//! §26 requires a Reject to carry a `RiskRejectReason`, and the reason a reason has to be a
//! machine-readable label rather than a sentence is the one M4 already stated in
//! [`crate::decision`]: "9 rejected, 3 of them on gas" is a question about a *rule*, and
//! string-matching a detail for the answer would be a question about wording. [`RiskCheck`] is
//! therefore carried as data on every `Reject` and `Unknown`, and the detail is beside it, not
//! inside it.
//!
//! ## What this layer cannot do, and how that is enforced
//!
//! §28 forbids `Risk → Signer`, `Risk → RPC send`, `Risk → Submitter`. That is not a behaviour
//! this module can be tested out of — it is a property of the type system, and it holds because
//! [`MultihopRiskPolicy::evaluate`] is a total function over two borrowed values and returns a
//! value: there is no `async`, no trait object, no `&mut`, and nothing in
//! `crates/risk/Cargo.toml` that reaches a network or a key. The two facts a freshness check
//! needs — the chain's head and the state version the pipeline is at — arrive as
//! [`MarketFacts`], arguments the caller fills from work it has already done. A missing fact is
//! `Unknown` (§41's philosophy), never a silent pass and never a failure invented to fill its
//! place.

use alloy_primitives::{Address, U256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};
use evm_simulation::{SimulatedOpportunity, SimulationStatus};

/// §27's nine checks plus the two described at the top of this module, as data.
///
/// The order of the variants is the order [`MultihopRiskPolicy::evaluate`] asks them in, which is
/// the order a reader can act in: first whether the run happened at all, then the arithmetic of
/// what it paid, then what the route and the deployment claim, then how stale the whole thing is.
/// A check that cannot be answered because a fact is absent stops the walk and reports
/// [`MultihopRiskDecision::Unknown`] rather than continuing to a later check that would have been
/// answered differently had the fact been there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskCheck {
    /// Did the call deliver? §45's `simulation_success`, asked of §25's five endings: only a
    /// `Delivered` run has an amount to put through the rest of this list.
    SimulationSuccess,
    /// §27's `input > 0`, read off the run's own input rather than off the candidate's claim.
    InputPositive,
    /// §27's `output > input`, in the input token's units. A round trip that comes back even is
    /// not a small profit: it is the route failing to pay for itself before any bill.
    OutputAboveInput,
    /// §27's `min profit`, in the same units — §24 keeps gross and gas apart, so this threshold
    /// is a gross figure and the gas question is asked separately, never netted into this one.
    MinimumGrossProfit,
    /// §27's `max gas`, compared against what the EVM measured.
    MaximumGas,
    /// The delivery reaching the floor the call itself carried. The contract enforces this
    /// (`FinalShortfall`) before the run can end, so a `SimulatedOpportunity` that disagrees with
    /// its own guard is not a market rejection — it is a record that has been edited after the
    /// run, which is exactly what §30's execution claim cannot be allowed to rest on.
    FinalGuard,
    /// §27's `route validity`: the legs the call carries are the legs the pricing produced, one
    /// per hop, chained, closing the circle, within the contract's cap.
    RouteValidity,
    /// §27's `executor validity`, split from the chain because they fail for different reasons
    /// and a caller has to be able to count them separately: an address that is not the deployed
    /// contract is an operator mistake; a chain that is not the route's chain is a plan applied
    /// to the wrong network.
    ChainValidity,
    /// The deployment this route is legal on.
    ExecutorValidity,
    /// §27's `simulation freshness`: how many blocks have been sealed since the header the state
    /// was loaded from.
    SimulationFreshness,
    /// §27's `state freshness`: how far the state the pipeline is reading now has moved past the
    /// state this run was made on. Two blocks can be the same height and still be different
    /// states, which is why this asks about the version and not about the clock.
    StateFreshness,
}

impl RiskCheck {
    /// The word an evidence row groups by. Stable across renames of the variant, because §43's
    /// recompute gate compares strings, not Rust paths.
    pub const fn label(self) -> &'static str {
        match self {
            Self::SimulationSuccess => "simulation_success",
            Self::InputPositive => "input_positive",
            Self::OutputAboveInput => "output_above_input",
            Self::MinimumGrossProfit => "minimum_gross_profit",
            Self::MaximumGas => "maximum_gas",
            Self::FinalGuard => "final_guard",
            Self::RouteValidity => "route_validity",
            Self::ChainValidity => "chain_validity",
            Self::ExecutorValidity => "executor_validity",
            Self::SimulationFreshness => "simulation_freshness",
            Self::StateFreshness => "state_freshness",
        }
    }

    /// The eleven checks, in the order they are asked. Exposed so a test can assert the policy
    /// answers every one of §27's list rather than a subset that happened to be convenient.
    pub const ALL: [RiskCheck; 11] = [
        RiskCheck::SimulationSuccess,
        RiskCheck::InputPositive,
        RiskCheck::OutputAboveInput,
        RiskCheck::MinimumGrossProfit,
        RiskCheck::MaximumGas,
        RiskCheck::FinalGuard,
        RiskCheck::RouteValidity,
        RiskCheck::ChainValidity,
        RiskCheck::ExecutorValidity,
        RiskCheck::SimulationFreshness,
        RiskCheck::StateFreshness,
    ];
}

/// §26's rejection: which check answered, and what it saw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RiskRejectReason {
    pub check: RiskCheck,
    pub detail: String,
}

impl RiskRejectReason {
    fn new(check: RiskCheck, detail: impl Into<String>) -> Self {
        Self {
            check,
            detail: detail.into(),
        }
    }

    pub fn label(&self) -> &'static str {
        self.check.label()
    }

    /// `check|detail`, for a report that has to fit a rejection in one field and still be
    /// groupable by the first half.
    pub fn describe(&self) -> String {
        format!("{}|{}", self.label(), self.detail)
    }
}

/// The two block-height facts §27's freshness checks need, supplied by the caller.
///
/// `Option` on purpose: a pipeline that has not read a head is not a pipeline that has read
/// block zero, and a policy that defaults an absent head to `0` would make every simulation look
/// ancient (or every stale one look fresh, depending on which way the subtraction is written).
/// Absence is answered with [`MultihopRiskDecision::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketFacts {
    /// The chain's current head, as the caller last saw it.
    pub head: Option<BlockNumber>,
    /// The block the caller's live state is at — the version the next simulation would be priced
    /// against. This is the pipeline's own state engine, not a second name for the head: they
    /// differ by however far the ingestion has fallen behind, and §27 asks about both.
    pub state_version: Option<BlockNumber>,
    /// Where these two numbers came from, in words, for §43's evidence rows.
    pub provenance: String,
}

/// The thresholds, in the units §24 says they live in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MultihopRiskPolicy {
    /// Smallest gross figure, in the input token's units, this layer calls worth running.
    /// Compared strictly: a run whose gain equals the floor is not above it.
    pub minimum_gross_profit: U256,
    /// Largest `gas_used` this layer looks at further.
    pub maximum_gas: u64,
    /// Blocks a caller may allow between the chain's head and the header this run was made on.
    pub maximum_simulation_age: u64,
    /// Blocks a caller may allow between the state it is reading now and the state this run was
    /// made on.
    pub maximum_state_age: u64,
    /// §27's `executor validity`, answered as a comparison against the one address the operator
    /// has deployed and allowlisted. A policy that read the allowance list off a node would be
    /// asking the network a question §28 forbids this layer to ask.
    pub executor: Address,
    /// The chain the route must belong to.
    pub chain_id: ChainId,
    /// Where these thresholds came from.
    pub provenance: String,
}

/// What an `Accept` is a claim about: the figures that passed, the thresholds they passed
/// against, and the two block facts the ages were computed from. Kept together for the same
/// reason M4 keeps its thresholds inside its `Accept` — a decision that does not say what it was
/// measured against cannot be re-derived after the numbers move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MultihopAcceptance {
    pub input_amount: U256,
    pub delivered: U256,
    pub gross_profit: U256,
    pub minimum_gross_profit: U256,
    pub min_final_output: U256,
    pub gas_used: u64,
    pub maximum_gas: u64,
    /// The bill beside the gross (§24), and `None` when the run measured gas and no price could
    /// be attached to it. An `Accept` never converts that absence into a zero.
    pub gas_charge_wei: Option<U256>,
    pub simulation_block: BlockNumber,
    pub head: Option<BlockNumber>,
    pub state_version: Option<BlockNumber>,
    pub detail: String,
}

/// §26's answer: `Accept`, or a rejection with the check that produced it, or the same shape when
/// a fact needed for a check is missing.
///
/// `large_enum_variant` fires because the accepted arm carries every number §27 compared — 280
/// bytes for the enum against a rejection's couple of dozen. Boxing it would buy a smaller stack
/// slot and cost the one thing this type is for: the builder downstream reads an acceptance field
/// by field, so an extra deref layer would sit between the answer and the five figures §30 checks,
/// for a value that exists a handful of times per window at most.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum MultihopRiskDecision {
    Accept(MultihopAcceptance),
    Reject(RiskRejectReason),
    Unknown(RiskRejectReason),
}

impl MultihopRiskDecision {
    pub fn accepted(&self) -> bool {
        matches!(self, Self::Accept(_))
    }

    /// The check this answer is about, for the two answers that name one.
    pub fn check(&self) -> Option<RiskCheck> {
        match self {
            Self::Accept(_) => None,
            Self::Reject(reason) | Self::Unknown(reason) => Some(reason.check),
        }
    }

    pub fn reason(&self) -> Option<&RiskRejectReason> {
        match self {
            Self::Accept(_) => None,
            Self::Reject(reason) | Self::Unknown(reason) => Some(reason),
        }
    }

    /// The words of the answer, whichever it is.
    pub fn detail(&self) -> &str {
        match self {
            Self::Accept(figures) => figures.detail.as_str(),
            Self::Reject(reason) | Self::Unknown(reason) => reason.detail.as_str(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Accept(_) => "accept",
            Self::Reject(_) => "reject",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl std::fmt::Display for MultihopRiskDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accept(_) => write!(f, "Accept: {}", self.detail()),
            Self::Reject(reason) => write!(f, "Reject on {}: {}", reason.label(), reason.detail),
            Self::Unknown(reason) => write!(f, "Unknown on {}: {}", reason.label(), reason.detail),
        }
    }
}

/// §27's policy, asked of the simulation §19-§25 produced.
impl MultihopRiskPolicy {
    /// Walk §27's checklist in order and return the first answer it gives.
    ///
    /// One pass, not eleven independent questions, for the reason M4 gives in its own policy: a
    /// run that did not happen has no profit to compare, and reporting a profit rule as the
    /// failure would describe a route that lost money when the truth is that no route ran.
    pub fn evaluate(
        &self,
        simulated: &SimulatedOpportunity,
        facts: &MarketFacts,
    ) -> MultihopRiskDecision {
        // Check 1 — did the call deliver. The detail is §25's own word for the ending, so a
        // reject names the ending (`reverted`, `out_of_gas`, `halted`, `no_return`) without this
        // layer inventing a fifth one.
        let delivered = match &simulated.status {
            SimulationStatus::Delivered { final_amount } => *final_amount,
            other => {
                return MultihopRiskDecision::Reject(RiskRejectReason::new(
                    RiskCheck::SimulationSuccess,
                    format!(
                        "the run did not deliver anything: its ending is {} ({})",
                        other.name(),
                        status_detail(other)
                    ),
                ))
            }
        };

        // Check 2 (§27's `input > 0`). Read off the run, because this is the amount the
        // operator's wallet was actually asked to move.
        if simulated.input_amount.is_zero() {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::InputPositive,
                "the run was asked to move 0 of the input token, so its gross is a statement \
                 about nothing",
            ));
        }

        // Check 3 (§27's `output > input`), in the input token's units. `gross` is Option because
        // §25 keeps a failure out of the profit columns entirely: a record with a delivery and no
        // gross is an incomplete artifact, so it answers `Unknown` rather than being read as a
        // loss. Check 1 makes the `None` case near-impossible; it is answered, not unwrapped,
        // because every field of a `SimulatedOpportunity` is public.
        let gross =
            match simulated.gross {
                Some(gross) => gross,
                None => return MultihopRiskDecision::Unknown(RiskRejectReason::new(
                    RiskCheck::OutputAboveInput,
                    "the run reports a delivery and carries no gross figure, so there is nothing \
                     to compare the input against"
                        .to_string(),
                )),
            };
        let Some(gain) = gross.gain() else {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::OutputAboveInput,
                format!(
                    "the round trip came back {delivered} against an input of {}: gross is {}, \
                     and anything but a gain is the route not paying for itself before any bill",
                    simulated.input_amount,
                    gross.name(),
                ),
            ));
        };

        // Check 4 (§27's `min profit`) — strict, and on the gross. Gas is not netted in here: §24
        // says the bill sits beside the profit, and a policy that subtracted it would be answering
        // M7's question with M11's numbers.
        if gain <= self.minimum_gross_profit {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::MinimumGrossProfit,
                format!(
                    "the round trip gains {gain} of the input token against a floor of {} — \
                     not above it ({})",
                    self.minimum_gross_profit, self.provenance
                ),
            ));
        }

        // Check 5 (§27's `max gas`), on the measured figure.
        if simulated.gas_used > self.maximum_gas {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::MaximumGas,
                format!(
                    "the run measured {} of gas, above the ceiling of {}",
                    simulated.gas_used, self.maximum_gas
                ),
            ));
        }

        // Check 6 — added by this module, not named by §27. The floor the call carried, re-read
        // from the record rather than trusted from the contract: `execute` reverts short of it, so
        // a record where the delivery is below the guard it travelled with describes an edited
        // artifact, not a market.
        if delivered < simulated.min_final_output {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::FinalGuard,
                format!(
                    "the run delivered {delivered} under a guard of {} — the contract would have \
                     reverted this call, so the record and the call disagree",
                    simulated.min_final_output
                ),
            ));
        }

        // Check 7 (§27's `route validity`), re-derived from the candidate this record is filed
        // under. The same question the adapter asks before it will file a run at all; asked again
        // here because every field of a `SimulatedOpportunity` is public, so the object a policy
        // receives is not necessarily the object the adapter returned.
        if let Err(error) = simulated.route_agrees_with_pricing() {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::RouteValidity,
                format!("the legs are not the priced route's: {error}"),
            ));
        }

        // Checks 8 and 9 — §27's `executor validity`, asked as two questions. Chain first: on the
        // wrong network the address would be a different contract entirely, so answering the
        // address question after would be answering about the wrong thing.
        if simulated.chain_id != self.chain_id {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::ChainValidity,
                format!(
                    "the run is on chain {} and this policy is for chain {}",
                    simulated.chain_id.0, self.chain_id.0
                ),
            ));
        }
        if simulated.run.executor != self.executor {
            return MultihopRiskDecision::Reject(RiskRejectReason::new(
                RiskCheck::ExecutorValidity,
                format!(
                    "the run was made against executor {:#x} and this policy allows {:#x} ({})",
                    simulated.run.executor, self.executor, self.provenance
                ),
            ));
        }

        // Check 10 (§27's `simulation freshness`).
        let head = match facts.head {
            Some(head) => head,
            None => {
                return MultihopRiskDecision::Unknown(RiskRejectReason::new(
                    RiskCheck::SimulationFreshness,
                    format!(
                        "the caller has no head block, so the age of a run pinned at block {} \
                         cannot be answered ({})",
                        simulated.simulation_block.number.0, facts.provenance
                    ),
                ))
            }
        };
        if let Some(decision) = age_rejection(
            head.0,
            simulated.simulation_block.number.0,
            self.maximum_simulation_age,
            RiskCheck::SimulationFreshness,
            "the header this run was made on",
            "sealed since the run's pinned header",
        ) {
            return decision;
        }

        // Check 11 (§27's `state freshness`).
        let state_version = match facts.state_version {
            Some(state_version) => state_version,
            None => {
                return MultihopRiskDecision::Unknown(RiskRejectReason::new(
                    RiskCheck::StateFreshness,
                    format!(
                        "the caller does not say what block its live state is at, so whether \
                         this run was made on that state cannot be answered ({})",
                        facts.provenance
                    ),
                ))
            }
        };
        if let Some(decision) = age_rejection(
            state_version.0,
            simulated.simulation_block.number.0,
            self.maximum_state_age,
            RiskCheck::StateFreshness,
            "the state this run was made on",
            "sealed between the run's state version and the state the pipeline is reading now",
        ) {
            return decision;
        }

        MultihopRiskDecision::Accept(MultihopAcceptance {
            input_amount: simulated.input_amount,
            delivered,
            gross_profit: gain,
            minimum_gross_profit: self.minimum_gross_profit,
            min_final_output: simulated.min_final_output,
            gas_used: simulated.gas_used,
            maximum_gas: self.maximum_gas,
            gas_charge_wei: simulated.gas_charge.wei(),
            simulation_block: simulated.simulation_block.number,
            head: Some(head),
            state_version: Some(state_version),
            detail: format!(
                "no broadcast: this policy only judges a simulation. Delivered {delivered} \
                 against {input} in, a gross gain of {gain} above a floor of {floor}, {gas} gas \
                 of a ceiling {ceiling} (charged {charged}), guard {guard} met, on the executor \
                 this policy allows, at an age within {max_simulation_age} and {max_state_age} \
                 blocks. Deciding to build, sign or send anything is the execution layer's \
                 question (§28), not this answer.",
                input = simulated.input_amount,
                floor = self.minimum_gross_profit,
                gas = simulated.gas_used,
                ceiling = self.maximum_gas,
                charged = spelled_charge(simulated.gas_charge.wei()),
                guard = simulated.min_final_output,
                max_simulation_age = self.maximum_simulation_age,
                max_state_age = self.maximum_state_age,
            ),
        })
    }
}

/// The age answer §27 asks for: a rejection when the span is over the bound, nothing when it is
/// not. A span that computes against a target ahead of the reference is refused as its own case
/// — a run pinned in the future is not a fresh run, it is a record about a block that has not
/// happened.
fn age_rejection(
    reference: u64,
    pinned: u64,
    bound: u64,
    check: RiskCheck,
    subject: &str,
    span_words: &str,
) -> Option<MultihopRiskDecision> {
    if reference < pinned {
        return Some(MultihopRiskDecision::Reject(RiskRejectReason::new(
            check,
            format!(
                "{subject} is block {pinned} and the caller's reference is block {reference}; a \
                 run ahead of the thing it is supposed to be measured against is not fresh, it \
                 describes a block that has not happened"
            ),
        )));
    }
    let age = reference - pinned;
    if age > bound {
        return Some(MultihopRiskDecision::Reject(RiskRejectReason::new(
            check,
            format!(
                "{age} blocks of {span_words}, over the declared bound of {bound} — {subject} is \
                 block {pinned} and the reference is block {reference}"
            ),
        )));
    }
    None
}

/// §25's ending, spelled out for the detail of check 1. The four non-deliveries are different
/// facts and a rejection that said only "it failed" would throw that away.
fn status_detail(status: &SimulationStatus) -> String {
    match status {
        SimulationStatus::Delivered { final_amount } => format!("delivered {final_amount}"),
        SimulationStatus::Reverted {
            contract_error,
            revert_kind,
            reason,
        } => match (contract_error, revert_kind) {
            (Some(error), _) => format!("the contract's own {error}: {reason}"),
            (None, Some(kind)) => format!("a revert this contract did not name ({kind}): {reason}"),
            (None, None) => format!("a revert with no payload: {reason}"),
        },
        SimulationStatus::OutOfGas => "the whole gas limit was spent".to_string(),
        SimulationStatus::Halted { reason } => format!("REVM stopped it: {reason}"),
        SimulationStatus::NoReturn => "success with no return data".to_string(),
    }
}

/// The bill as one word, so an `Accept`'s sentence never prints a bare `None` where a number
/// belongs and never prints `0` where the price was simply not declared (§24/§31).
fn spelled_charge(wei: Option<U256>) -> String {
    match wei {
        Some(wei) => wei.to_string(),
        None => "unpriced".to_string(),
    }
}
