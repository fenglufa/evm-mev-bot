//! §5–§8/§23/§36–§40: the plan M10 executes, and the one arrow from it into M6's ladder.
//!
//! M6's [`crate::intent::TransactionIntent`] is the object that carries a *risk decision* into
//! the transaction layer, and M7's [`crate::sequence`] executes a route as N transactions from
//! one EOA. M10's shape is neither: the executor contract turns a multi-leg route into
//! **one** transaction, so what the transaction layer needs is a *plan* — a route whose amounts,
//! floors and destination are already decided — rather than another intent-shaped summary of a
//! judgement. This file is that plan and nothing else:
//!
//! * [`ArbitrageExecutionPlan`] (§5) states the route and the three reasons it may exist: the
//!   block it was simulated at, the simulation's own answer, and the profit policy the contract
//!   will enforce.
//! * [`ArbitrageExecutionPlan::validate`] (§15/§16/§12/§14/§7/§36) refuses an undeliverable
//!   plan *before* calldata exists. Its rule is deliberately narrow, and the narrowness is the
//!   design: it checks exactly what the contract checks on chain, plus the two bindings the
//!   contract cannot see (chain id and executor address) and the one internal agreement no
//!   opcode reads (that the plan's floor and its profit policy name the same number). An
//!   off-chain rule the contract does not enforce is a rule a different caller can break, so it
//!   would be documentation rather than a guard.
//! * [`ExecutablePlan`] (§6) is a plan that validated. Its fields are private and there is no
//!   setter, which is §6's immutability as a type rather than as a paragraph: the route, the
//!   amounts, the floors, the pools and the tokens cannot be changed between validation and the
//!   node, and a changed plan is a different `plan_hash` and therefore a different execution —
//!   never a silent edit of the old one.
//! * [`ExecutionClass`] (§40) keeps the six ways an attempt can end distinguishable, including
//!   the three inequalities §40 names: submitted ≠ included, included ≠ successful, successful
//!   ≠ profitable. The last of those is not this file's to decide: `IncludedSucceeded` says the
//!   receipt reported status 1, and [`crate::profit::ProfitVerificationStatus`] still decides
//!   whether anything was earned.
//!
//! Nothing here discovers a route, prices one, or sizes one. §2 forbids it and §22 forbids it of
//! the contract; the same rule applied to the Rust side is why every field of a plan is an
//! argument and no field is a read.

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::Serialize;

use evm_core::BlockNumber;
use evm_protocol::{ExecutorCall, ExecutorLeg, RevertPayload};

use crate::error::{ExecutionError, Result};
use crate::gate::{Freshness, PlanBinding};
use crate::intent::{ExecutionIds, SenderFunding, TransactionIntent};
use crate::market::MarketKind;
use crate::profit::ProfitDenomination;
use crate::receipt::ReceiptStatus;
use crate::tx::TransactionType;

/// §5's "amount derivation", stated per leg rather than inferred by the reader.
///
/// The contract enforces the same chain (`AmountChainBroken`) as it walks the route, so this is
/// the off-chain half of an already-enforced rule: a plan whose stated derivation disagrees with
/// the leg above it never becomes calldata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountDerivation {
    /// Leg 0 only: this leg spends the plan's own `input_amount`, which the operator pre-funded
    /// to the executor (§18's model — the contract pulls with `transferFrom`, never borrows).
    PlanInput,
    /// Any leg after the first: this leg spends the previous leg's `amount_out`, carried whole.
    /// §21's balance safety is what makes "whole" the only option — the contract holds a leg's
    /// output between calls and settles nothing back early.
    PreviousLegOutput,
}

impl AmountDerivation {
    pub fn name(self) -> &'static str {
        match self {
            Self::PlanInput => "plan_input",
            Self::PreviousLegOutput => "previous_leg_output",
        }
    }
}

/// One leg of the route, as the plan states it and as the contract's `Leg` struct reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PlanLeg {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    /// What the simulation measured this leg produces. The contract requires the pair to deliver
    /// exactly this figure (§11: `DeliveryMismatch`), so `amount_out` is a claim about the swap,
    /// never a tolerance band.
    pub amount_out: U256,
    /// §14's per-leg floor.
    pub min_amount_out: U256,
    pub derivation: AmountDerivation,
}

impl PlanLeg {
    /// The same leg without the derivation field, which the contract has no parameter for.
    fn to_executor(self) -> ExecutorLeg {
        ExecutorLeg {
            pool: self.pool,
            token_in: self.token_in,
            token_out: self.token_out,
            amount_in: self.amount_in,
            amount_out: self.amount_out,
            min_amount_out: self.min_amount_out,
        }
    }

    fn canonical_lines(&self, index: usize) -> String {
        format!(
            "leg[{index}]|pool={:#x}|token_in={:#x}|token_out={:#x}|amount_in={}|amount_out={}|\
             min_amount_out={}|derivation={}\n",
            self.pool,
            self.token_in,
            self.token_out,
            self.amount_in,
            self.amount_out,
            self.min_amount_out,
            self.derivation.name(),
        )
    }
}

/// §8's freshness as the plan states it, feeding the existing gate rather than a second rule.
///
/// §8 is explicit that `target_block` is not a promise that the transaction runs in that block;
/// it is the provenance of the simulation. The age bound below is the plan's own contribution to
/// §31's question — "how old may this route be?" — and the answer it produces is the *existing*
/// [`Freshness`] value, which the pre-submit gate then judges. M10 adds no freshness semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlanValidity {
    /// §8's `simulation_block`: the canonical block the route's market facts were read at.
    pub simulated_at_block: BlockNumber,
    /// How many blocks may be sealed after `simulated_at_block` before the plan is stale. A
    /// caller declares it and the plan carries it verbatim; nothing here reads a chain to decide
    /// whether the number is wise, and §51 forbids the hot path from asking.
    pub max_block_age: u64,
    /// Where the declared bound came from, for §52's evidence rows.
    pub provenance: String,
}

impl PlanValidity {
    /// §8: the existing three-state answer, computed from the caller's current head. `Unknown`
    /// is not an option here because this function is the answer to an explicit question — the
    /// gate's own `Unknown` arm stays reserved for a caller that never asked.
    pub fn freshness_at(&self, head: BlockNumber) -> Freshness {
        if head.0 < self.simulated_at_block.0 {
            return Freshness::Stale {
                reason: format!(
                    "the head is at block {} and this plan was simulated at {}; a plan whose \
                     block is ahead of the chain it is about to be sent to describes state that \
                     has not happened",
                    head.0, self.simulated_at_block.0,
                ),
            };
        }
        let age = head.0 - self.simulated_at_block.0;
        if age > self.max_block_age {
            return Freshness::Stale {
                reason: format!(
                    "{} blocks have been sealed since the route was priced at block {}, over the \
                     declared bound of {} ({})",
                    age, self.simulated_at_block.0, self.max_block_age, self.provenance,
                ),
            };
        }
        Freshness::Active
    }
}

/// What the REVM run of this exact calldata answered (§25/§55).
///
/// Three states because §48's replayability and §40's error classes both turn on the difference
/// between "it reverted", "it succeeded", and "nobody ran it". A plan whose simulation was never
/// run cannot pass the gate's `simulation_succeeded` leg, and the gate is right to refuse it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum SimulationOutcome {
    /// The run completed and the route produced this much of the input token.
    ///
    /// `proved_gas_limit` is the limit that run was executed under, and it is the number the
    /// gas policy has to resolve against: a simulation's answer is a statement about a limit
    /// ("this call completes when it may spend this much"), not about a consumption. EIP-150
    /// gives a nested call 63/64 of what its caller has left, so a transaction whose limit is
    /// the gas the simulation happened to burn can leave the deepest frame short of gas and
    /// revert even though the same call succeeds with headroom. GIWA measured exactly that on
    /// §57's first attempt: 229,302 gas burned, 249,302 limit, `UniswapV2: TRANSFER_FAILED`
    /// from the second pair running out of gas while paying out — and the node's own minimum
    /// for this calldata at this block is 288,750, while its gas used at any larger limit is
    /// still exactly 229,302. Sending at the proved limit plus the declared margin is safe by
    /// monotonicity: a larger limit never takes gas away from a frame that already had enough,
    /// and neither this contract nor the V2 pairs branch on `gasleft`.
    Succeeded {
        gas_used: u64,
        proved_gas_limit: u64,
        final_amount: U256,
    },
    /// The contract rejected the route. `revert` is [`RevertPayload::kind`]'s name — a
    /// classification, never raw bytes, so a plan can be serialized and hashed with it.
    Reverted { revert: String },
    /// No run exists. Buildable as evidence, never submittable.
    NotRun,
}

impl SimulationOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    /// §13's gas measurement, present exactly when there is a measurement to have.
    pub fn gas_used(&self) -> Option<u64> {
        match self {
            Self::Succeeded { gas_used, .. } => Some(*gas_used),
            _ => None,
        }
    }

    /// The gas limit this run was executed under — what §13's policy resolves against, because
    /// it is the limit this call was proved to complete at.
    pub fn proved_gas_limit(&self) -> Option<u64> {
        match self {
            Self::Succeeded {
                proved_gas_limit, ..
            } => Some(*proved_gas_limit),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Succeeded { .. } => "succeeded",
            Self::Reverted { .. } => "reverted",
            Self::NotRun => "not_run",
        }
    }

    /// The revert classification, when the outcome is a revert.
    pub fn revert(&self) -> Option<&str> {
        match self {
            Self::Reverted { revert } => Some(revert.as_str()),
            _ => None,
        }
    }

    /// §25/§55: the outcome for a run whose revert data this crate could classify.
    pub fn reverted(payload: &RevertPayload) -> Self {
        Self::Reverted {
            revert: payload.kind().to_string(),
        }
    }
}

/// §5's `simulation_context`: everything about the run that made this plan executable, kept
/// together so a reader can tell which canonical state the route was priced against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SimulationContext {
    /// §39's opaque correlation id — the finding or the fixture this plan is about, verbatim as
    /// its own producer spelled it. M10 deliberately does not take `evm-pathfinder` as a
    /// dependency to type this field (§3/§39).
    pub correlation_id: String,
    pub block_number: BlockNumber,
    pub block_hash: B256,
    /// The state version the route was priced on, in the same string spelling the intent uses.
    pub state_fingerprint: String,
    /// [`evm_simulation::SimulationResult::fingerprint`] of the run, so §48's replay can name the
    /// simulation and not just the plan.
    pub simulation_id: B256,
    pub outcome: SimulationOutcome,
    /// §34's question, asked of the run that funded the simulation rather than assumed.
    pub funding: SenderFunding,
    /// §29's label, with its evidence. A plan carrying `ControlledFixture` can complete the
    /// whole ladder and still be forbidden from reading as a market opportunity.
    #[serde(serialize_with = "serialize_market")]
    pub market: MarketKind,
}

/// [`MarketKind`]'s own `to_json()`, routed through serde so the label and its evidence string
/// both reach an evidence row. The type is M7's and this milestone does not add a derive to it;
/// its existing serializer is the shape every M7 evidence file already uses.
fn serialize_market<S: serde::Serializer>(
    market: &MarketKind,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    market.to_json().serialize(serializer)
}

impl SimulationContext {
    fn canonical_lines(&self) -> String {
        let outcome = match &self.outcome {
            SimulationOutcome::Succeeded {
                gas_used,
                proved_gas_limit,
                final_amount,
            } => format!(
                "succeeded|gas_used={gas_used}|proved_gas_limit={proved_gas_limit}|\
                 final_amount={final_amount}"
            ),
            SimulationOutcome::Reverted { revert } => format!("reverted|{revert}"),
            SimulationOutcome::NotRun => "not_run".to_string(),
        };
        format!(
            "simulation|correlation_id={}|block={}|hash={:#x}|state={}|simulation_id={:#x}|\
             outcome={}|funding={}|market={}\n",
            self.correlation_id,
            self.block_number.0,
            self.block_hash,
            self.state_fingerprint,
            self.simulation_id,
            outcome,
            self.funding.describe(),
            self.market.describe(),
        )
    }
}

/// §5's `profit_policy`: the floor the plan enforces, in the unit that floor is measurable in.
///
/// §12 says plainly that M10 does not find profit — it executes a defined invariant. So this
/// type holds no formula. What it holds is the number the contract compares against, the unit
/// it is denominated in, and the statement of where it came from; [`ExecutionBinding`]'s checks
/// and [`ArbitrageExecutionPlan::validate`] then make sure the plan itself is not internally
/// at odds about either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfitPolicy {
    /// §34's unit rule as a type: a round trip settles in the input token while the bill is paid
    /// in ETH, so a token-denominated floor and a wei-denominated floor are different claims and
    /// only one of them can be compared to `min_final_output`.
    pub denomination: ProfitDenomination,
    /// §12's `required_final_balance`, stated in the denomination above.
    pub required_final_balance: U256,
    pub provenance: String,
}

impl ProfitPolicy {
    fn canonical_lines(&self) -> String {
        format!(
            "profit|denomination={}|required_final_balance={}|provenance={}\n",
            self.denomination.describe(),
            self.required_final_balance,
            self.provenance,
        )
    }
}

/// The two facts about the *deployment* a plan has to agree with, and cannot read for itself.
///
/// §7 and §36 are both refusals to trust what is nearby: the chain id comes from the runtime
/// configuration rather than from what an RPC endpoint happens to answer, and the executor
/// address comes from the configuration rather than from a default that could silently stand in
/// for the contract that was actually deployed. Grouped into one value so an entry point cannot
/// transpose them, and so the pair is visible in a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionBinding {
    pub chain_id: u64,
    pub executor: Address,
}

/// One reason a plan is not executable, named rather than described (§40's `PlanRejected`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanRejection {
    /// §7: the plan is for a chain this runtime is not configured for.
    Chain { plan: u64, configured: u64 },
    /// §36: the plan names an executor this runtime did not configure.
    Executor { plan: Address, configured: Address },
    /// §15/§10: an address field that cannot name a pool, a token or an account.
    ZeroAddress { field: String },
    /// §15: an amount field that cannot be spent.
    ZeroAmount { field: String },
    /// §15: a route with no legs is not a route.
    NoLegs,
    /// §9: a route longer than the contract's own bound.
    TooManyLegs { count: usize, maximum: usize },
    /// §15: a leg that trades a token against itself.
    SelfLoop { index: usize, token: Address },
    /// §16: `leg[i].token_out != leg[i+1].token_in`.
    BrokenContinuity {
        index: usize,
        expected: Address,
        found: Address,
    },
    /// §17: the route does not end in the token it started in, so there is no round trip to
    /// compare a final balance against.
    NotRoundTrip {
        input_token: Address,
        other_end: Address,
    },
    /// §5/§11: a leg's stated derivation disagrees with the plan or the leg above it.
    AmountDerivation {
        index: usize,
        stated: AmountDerivation,
        expected: U256,
        found: U256,
    },
    /// §14: a leg that asks for less than its own floor is not a protected leg.
    AskBelowFloor {
        index: usize,
        amount_out: U256,
        min_amount_out: U256,
    },
    /// §14: a leg with no floor at all — the contract would accept the route and M10 would have
    /// no on-chain guard on that step.
    NoLegFloor { index: usize },
    /// §12: the plan's floor and its profit policy name different numbers.
    FinalFloorDisagreement {
        min_final_output: U256,
        required_final_balance: U256,
    },
    /// §34: a token route whose floor is denominated in wei, or one denominated in a token
    /// other than the route's input token.
    Denomination {
        stated: String,
        expected_token: Address,
    },
    /// §13: a simulation that claims to have completed while reporting a burn larger than the
    /// limit it ran under. One of the two numbers is wrong, and the plan cannot be executed on a
    /// claim its own run contradicts.
    SimulatedLimitBelowBurn { burned: u64, proved_limit: u64 },
}

impl PlanRejection {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Chain { .. } => "wrong_chain",
            Self::Executor { .. } => "wrong_executor",
            Self::ZeroAddress { .. } => "zero_address",
            Self::ZeroAmount { .. } => "zero_amount",
            Self::NoLegs => "no_legs",
            Self::TooManyLegs { .. } => "too_many_legs",
            Self::SelfLoop { .. } => "leg_self_loop",
            Self::BrokenContinuity { .. } => "broken_continuity",
            Self::NotRoundTrip { .. } => "not_round_trip",
            Self::AmountDerivation { .. } => "amount_derivation",
            Self::AskBelowFloor { .. } => "ask_below_floor",
            Self::NoLegFloor { .. } => "no_leg_floor",
            Self::FinalFloorDisagreement { .. } => "final_floor_disagreement",
            Self::Denomination { .. } => "wrong_denomination",
            Self::SimulatedLimitBelowBurn { .. } => "simulated_limit_below_burn",
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Self::Chain { plan, configured } => format!(
                "§7: the plan is for chain {plan} and this execution is configured for chain \
                 {configured}; the endpoint's own answer is a third opinion and never a \
                 substitute for the binding"
            ),
            Self::Executor { plan, configured } => format!(
                "§36: the plan targets executor {plan:#x} and the configured executor is \
                 {configured:#x}; sending it anyway is the target drift the rule exists to stop"
            ),
            Self::ZeroAddress { field } => {
                format!("§15: {field} is the zero address, which names no pool or token")
            }
            Self::ZeroAmount { field } => format!(
                "§15: {field} is zero, so the leg it guards either spends nothing or protects \
                 nothing"
            ),
            Self::NoLegs => "§15: a route with no legs moves no asset".to_string(),
            Self::TooManyLegs { count, maximum } => format!(
                "§9: the route has {count} legs and the contract refuses more than {maximum} \
                 (MAX_LEGS), so this plan would revert before it swapped"
            ),
            Self::SelfLoop { index, token } => format!(
                "§15: leg {index} trades {token:#x} against itself, which is a fee paid for \
                 nothing"
            ),
            Self::BrokenContinuity {
                index,
                expected,
                found,
            } => format!(
                "§16: leg {index} should start at {expected:#x}, where the leg before it ends, \
                 but names {found:#x}"
            ),
            Self::NotRoundTrip {
                input_token,
                other_end,
            } => format!(
                "§17: the route starts in {input_token:#x} and ends in {other_end:#x}, so \
                 min_final_output is not a comparison of the same asset"
            ),
            Self::AmountDerivation {
                index,
                stated,
                expected,
                found,
            } => format!(
                "§5: leg {index} says its input comes from {} and therefore should spend \
                 {expected}, but carries {found}",
                stated.name()
            ),
            Self::AskBelowFloor {
                index,
                amount_out,
                min_amount_out,
            } => format!(
                "§14: leg {index} asks the pair for {amount_out} and protects it with a floor of \
                 {min_amount_out}, which the contract refuses with AskBelowFloor"
            ),
            Self::NoLegFloor { index } => format!(
                "§14: leg {index} has no minimum output, so the step that §14 exists to protect \
                 is unprotected on chain"
            ),
            Self::FinalFloorDisagreement {
                min_final_output,
                required_final_balance,
            } => format!(
                "§12: the plan enforces a final balance of {min_final_output} and its profit \
                 policy requires {required_final_balance}; one of the two is what gets signed, \
                 and a plan that holds both is not a plan"
            ),
            Self::Denomination {
                stated,
                expected_token,
            } => format!(
                "§34: the floor is stated as {stated}, but this route settles in \
                 {expected_token:#x} — the asset it round-trips through. Comparing the two would \
                 be adding numbers in different units"
            ),
            Self::SimulatedLimitBelowBurn {
                burned,
                proved_limit,
            } => format!(
                "§13: the simulation reports {burned} gas burned at a limit of {proved_limit}, \
                 and a run cannot burn more than it was allowed — one of the two numbers is not \
                 from this run, and the send limit is resolved out of them"
            ),
        }
    }
}

/// The reasons, plural: a plan that fails three checks has to be reported with all three, for
/// the same reason [`crate::gate::GateOutcome`] reports every failing leg at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRejections(pub Vec<PlanRejection>);

impl PlanRejections {
    pub fn codes(&self) -> Vec<&'static str> {
        self.0.iter().map(PlanRejection::code).collect()
    }

    pub fn describe(&self) -> String {
        self.0
            .iter()
            .map(|rejection| rejection.reason())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// §40's `PlanRejected` in the crate's taxonomy, so the refusal reaches a caller as a named
    /// class rather than as a string someone invents at the call site.
    pub fn to_error(&self) -> ExecutionError {
        ExecutionError::PlanRejected(self.describe())
    }
}

/// §5's plan: a route that has already been decided, with the reasons attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArbitrageExecutionPlan {
    /// §7: the chain this route belongs to. Part of the plan hash, because the same legs on
    /// another chain are a different plan.
    pub chain_id: u64,
    /// §36: the deployed contract that will execute it. Also hashed.
    pub executor: Address,
    pub sender: Address,
    pub recipient: Address,
    pub input_token: Address,
    pub input_amount: U256,
    pub legs: Vec<PlanLeg>,
    /// §12's floor, enforced by the contract as `minFinalAmount`.
    pub min_final_output: U256,
    pub validity: PlanValidity,
    pub simulation: SimulationContext,
    pub profit: ProfitPolicy,
}

impl ArbitrageExecutionPlan {
    /// §5's fields as they are, before any check. Construction is deliberately trivial: the
    /// place a plan becomes executable is [`ExecutablePlan::new`], and a type whose constructor
    /// cannot fail cannot be the boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_id: u64,
        executor: Address,
        sender: Address,
        recipient: Address,
        input_token: Address,
        input_amount: U256,
        legs: Vec<PlanLeg>,
        min_final_output: U256,
        validity: PlanValidity,
        simulation: SimulationContext,
        profit: ProfitPolicy,
    ) -> Self {
        Self {
            chain_id,
            executor,
            sender,
            recipient,
            input_token,
            input_amount,
            legs,
            min_final_output,
            validity,
            simulation,
            profit,
        }
    }

    /// §49's D1 and §48's replayability: one canonical spelling of every field the plan claims,
    /// in a fixed order. A hash over this text changes when the route, an amount, a floor, a
    /// pool, a token, the pinned block, the simulation's answer, the funding statement or the
    /// market label changes — which is §6's rule that a changed plan is a different execution,
    /// expressed as one number.
    pub fn canonical_text(&self) -> String {
        let mut text = format!(
            "m10-arbitrage-execution-plan\nchain_id={}\nexecutor={:#x}\nsender={:#x}\n\
             recipient={:#x}\ninput_token={:#x}\ninput_amount={}\nlegs={}\nmin_final_output={}\n",
            self.chain_id,
            self.executor,
            self.sender,
            self.recipient,
            self.input_token,
            self.input_amount,
            self.legs.len(),
            self.min_final_output,
        );
        for (index, leg) in self.legs.iter().enumerate() {
            text.push_str(&leg.canonical_lines(index));
        }
        text.push_str(&format!(
            "validity|simulated_at={}|max_block_age={}|provenance={}\n",
            self.validity.simulated_at_block.0,
            self.validity.max_block_age,
            self.validity.provenance,
        ));
        text.push_str(&self.simulation.canonical_lines());
        text.push_str(&self.profit.canonical_lines());
        text
    }

    /// The plan's identity (§39's `plan id`).
    pub fn plan_hash(&self) -> B256 {
        alloy_primitives::keccak256(self.canonical_text().as_bytes())
    }

    /// §23/§37: the exact bytes the executor contract will be called with. Deterministic by
    /// construction — [`ExecutorCall::encode`] is a `sol!` ABI encode of fields this plan
    /// already fixed, and nothing in it reads a clock, a nonce, or a node.
    pub fn calldata(&self) -> Bytes {
        self.to_call().encode()
    }

    /// The same bytes as an [`ExecutorCall`], so a reader can decode what was signed rather than
    /// trust that it was this route.
    pub fn to_call(&self) -> ExecutorCall {
        ExecutorCall::Execute {
            legs: self
                .legs
                .iter()
                .map(|leg| leg.to_executor())
                .collect::<Vec<_>>(),
            input_token: self.input_token,
            amount_in: self.input_amount,
            min_final_amount: self.min_final_output,
            recipient: self.recipient,
        }
    }

    pub fn calldata_hash(&self) -> B256 {
        alloy_primitives::keccak256(self.calldata().as_ref())
    }

    /// §38's execution-layer route identity: the chain, the pools in trade order, and the token
    /// transitions in trade order.
    ///
    /// Two things are deliberately absent. Amounts are not here because they are in
    /// [`Self::plan_hash`], and a route is the same route at two sizes — §55's six cases run one
    /// route at six amounts. The block is not here because M9.3's candidate identity already owns
    /// "this route, at this block" and §38 forbids redefining it; this string says only which
    /// route a transaction walked, which is what an execution record is asked to keep.
    pub fn route_id(&self) -> String {
        let pools = self
            .legs
            .iter()
            .map(|leg| format!("{:#x}", leg.pool))
            .collect::<Vec<_>>()
            .join(">");
        let mut tokens = self
            .legs
            .iter()
            .map(|leg| format!("{:#x}", leg.token_in))
            .collect::<Vec<_>>();
        if let Some(last) = self.legs.last() {
            tokens.push(format!("{:#x}", last.token_out));
        }
        format!("m10-{}-{pools}-{}", self.chain_id, tokens.join(">"))
    }

    /// The route's token transitions, start token first and ending where it began. Exposed
    /// because §32's reconciliation and §48's replay both name a route by its tokens rather than
    /// by recomputing this list.
    pub fn token_transitions(&self) -> Vec<Address> {
        let mut tokens = self.legs.iter().map(|leg| leg.token_in).collect::<Vec<_>>();
        if let Some(last) = self.legs.last() {
            tokens.push(last.token_out);
        }
        tokens
    }

    /// §15/§16/§14/§12/§7/§36: every reason this plan cannot be executed, reported together.
    ///
    /// The order is the order a caller can fix them in: the two bindings first (they decide
    /// whether the plan is even about this deployment), then the plan's own fields, then the legs
    /// in route order. Determinism is not decoration here — §49's D4 promises the same broken
    /// plan produces the same rejection list, and a negative control that reorders its refusals
    /// is a control that proves nothing.
    pub fn validate(&self, binding: &ExecutionBinding) -> Vec<PlanRejection> {
        let mut rejections = Vec::new();
        if self.chain_id != binding.chain_id {
            rejections.push(PlanRejection::Chain {
                plan: self.chain_id,
                configured: binding.chain_id,
            });
        }
        if self.executor != binding.executor {
            rejections.push(PlanRejection::Executor {
                plan: self.executor,
                configured: binding.executor,
            });
        }
        for (field, address) in [
            ("sender", self.sender),
            ("recipient", self.recipient),
            ("input_token", self.input_token),
            ("executor", self.executor),
        ] {
            if address == Address::ZERO {
                rejections.push(PlanRejection::ZeroAddress {
                    field: field.to_string(),
                });
            }
        }
        if self.input_amount.is_zero() {
            rejections.push(PlanRejection::ZeroAmount {
                field: "input_amount".to_string(),
            });
        }
        if self.min_final_output.is_zero() {
            rejections.push(PlanRejection::ZeroAmount {
                field: "min_final_output".to_string(),
            });
        }
        if self.legs.is_empty() {
            rejections.push(PlanRejection::NoLegs);
        } else if u64::try_from(self.legs.len()).unwrap_or(u64::MAX) > evm_protocol::MAX_LEGS {
            // The comparison is on the count's side, not the bound's: converting the bound
            // down to a `usize` is the direction that can fail, and a fallback there would
            // fail open — an unrepresentable bound would silently allow any route length.
            rejections.push(PlanRejection::TooManyLegs {
                count: self.legs.len(),
                maximum: usize::try_from(evm_protocol::MAX_LEGS).unwrap_or(usize::MAX),
            });
        }

        // §34: the floor has to be comparable with what the route delivers, which means the same
        // asset. A `NativeWei` policy on a token round trip would be §12's inequality between two
        // different units, and no on-chain check can catch it because the contract never sees a
        // unit — only a number.
        match &self.profit.denomination {
            ProfitDenomination::TokenSettled { token, .. } if *token == self.input_token => {}
            other => rejections.push(PlanRejection::Denomination {
                stated: other.describe(),
                expected_token: self.input_token,
            }),
        }
        if self.profit.required_final_balance != self.min_final_output {
            rejections.push(PlanRejection::FinalFloorDisagreement {
                min_final_output: self.min_final_output,
                required_final_balance: self.profit.required_final_balance,
            });
        }

        for (index, leg) in self.legs.iter().enumerate() {
            for (field, address) in [
                ("pool", leg.pool),
                ("token_in", leg.token_in),
                ("token_out", leg.token_out),
            ] {
                if address == Address::ZERO {
                    rejections.push(PlanRejection::ZeroAddress {
                        field: format!("legs[{index}].{field}"),
                    });
                }
            }
            if leg.amount_in.is_zero() || leg.amount_out.is_zero() {
                rejections.push(PlanRejection::ZeroAmount {
                    field: format!(
                        "legs[{index}].amount_{}",
                        if leg.amount_in.is_zero() { "in" } else { "out" }
                    ),
                });
            }
            if leg.min_amount_out.is_zero() {
                rejections.push(PlanRejection::NoLegFloor { index });
            }
            if leg.token_in == leg.token_out {
                rejections.push(PlanRejection::SelfLoop {
                    index,
                    token: leg.token_in,
                });
            }
            if leg.amount_out < leg.min_amount_out {
                rejections.push(PlanRejection::AskBelowFloor {
                    index,
                    amount_out: leg.amount_out,
                    min_amount_out: leg.min_amount_out,
                });
            }
        }

        // §16's continuity and §5's derivation, in route order, against the leg before each.
        if let Some(first) = self.legs.first() {
            if first.token_in != self.input_token {
                rejections.push(PlanRejection::BrokenContinuity {
                    index: 0,
                    expected: self.input_token,
                    found: first.token_in,
                });
            }
            if first.derivation != AmountDerivation::PlanInput
                || first.amount_in != self.input_amount
            {
                rejections.push(PlanRejection::AmountDerivation {
                    index: 0,
                    stated: first.derivation,
                    expected: self.input_amount,
                    found: first.amount_in,
                });
            }
        }
        for index in 1..self.legs.len() {
            let previous = &self.legs[index - 1];
            let leg = &self.legs[index];
            if previous.token_out != leg.token_in {
                rejections.push(PlanRejection::BrokenContinuity {
                    index,
                    expected: previous.token_out,
                    found: leg.token_in,
                });
            }
            if leg.derivation != AmountDerivation::PreviousLegOutput
                || leg.amount_in != previous.amount_out
            {
                rejections.push(PlanRejection::AmountDerivation {
                    index,
                    stated: leg.derivation,
                    expected: previous.amount_out,
                    found: leg.amount_in,
                });
            }
        }
        if let Some(last) = self.legs.last() {
            if last.token_out != self.input_token {
                rejections.push(PlanRejection::NotRoundTrip {
                    input_token: self.input_token,
                    other_end: last.token_out,
                });
            }
        }

        // §13: the two gas numbers a success carries have to be a pair one run could produce,
        // because the send limit is resolved out of them. `proved_gas_limit < gas_used` is the
        // shape that cannot happen — and it is the shape a plan gets into when the field is
        // filled with the burn twice, which is how §57's first attempt ended up with a limit
        // equal to a burn plus a margin and a nested pair call starved of gas.
        if let SimulationOutcome::Succeeded {
            gas_used,
            proved_gas_limit,
            ..
        } = &self.simulation.outcome
        {
            if proved_gas_limit < gas_used {
                rejections.push(PlanRejection::SimulatedLimitBelowBurn {
                    burned: *gas_used,
                    proved_limit: *proved_gas_limit,
                });
            }
        }
        rejections
    }

    /// §8: the plan's own answer to the freshness question, given the caller's head. The gate
    /// decides what to do with it; this only computes what §31 asks of a pinned route.
    pub fn freshness_at(&self, head: BlockNumber) -> Freshness {
        self.validity.freshness_at(head)
    }

    /// The intent for this plan: one transaction, to the executor, with the plan's calldata, and
    /// `value = 0` (§34).
    ///
    /// `simulated_steps` is `1` because that is M10's whole point of having a contract: the route
    /// has several legs and the *transaction* has one step, so [`crate::builder`]'s §4 rule about
    /// multi-step intents never applies and no sequence position is claimed. The nonce is left at
    /// `0` for the same reason an arbitrage intent leaves the simulation's assumed fee fields for
    /// [`crate::stage::ExecutionStage`] to replace: the lane reads the real pending view before a
    /// build, and a nonce a plan invented would be a nonce no node agreed to.
    fn to_intent(&self) -> TransactionIntent {
        let plan_hash = self.plan_hash();
        let simulation_id = self.simulation.simulation_id;
        // §39: the record has to be joinable to a plan without a cross-crate type, and
        // `risk_decision_id` is the third of the intent's three opaque ids. M10 runs no risk
        // layer, so this is not a risk decision — it is `keccak(simulation ‖ plan)`, which names
        // the plan that was judged and the run it was judged on. The lifecycle rung the risk
        // layer owns is deliberately not advanced for this path (`stage`'s ladder says so), and
        // the gate's `risk_accepted` leg is not claimed either.
        let risk_decision_id = {
            let mut bytes = Vec::with_capacity(64);
            bytes.extend_from_slice(simulation_id.as_slice());
            bytes.extend_from_slice(plan_hash.as_slice());
            alloy_primitives::keccak256(&bytes)
        };
        TransactionIntent {
            ids: ExecutionIds {
                opportunity_id: self.simulation.correlation_id.clone(),
                simulation_id,
                risk_decision_id,
            },
            chain_id: self.chain_id,
            block_number: self.simulation.block_number,
            block_hash: self.simulation.block_hash,
            state_fingerprint: self.simulation.state_fingerprint.clone(),
            sender: self.sender,
            target: self.executor,
            // §34: an ERC-20 route moves value with `transferFrom`, so a non-zero `value` here
            // would be ETH handed to a contract that has no use for it.
            value: U256::ZERO,
            calldata: self.calldata(),
            nonce: 0,
            // The measurement §13's gas policy resolves against; the builder replaces this with
            // the proved limit + the declared margin and refuses if it cannot. A run that never
            // executed has no gas number, and zero is the value the transaction layer already
            // refuses (`InvalidIntent: "gas limit is zero"`), so the missing measurement becomes
            // a refusal rather than an invented limit. It is the limit the run was executed
            // under and not the gas it burned: EIP-150 means a limit equal to the burn can starve
            // the deepest frame of a nested call, which is how §57's first attempt reverted.
            gas_limit: self.simulation.outcome.proved_gas_limit().unwrap_or(0),
            tx_type: TransactionType::DynamicFee,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            access_list: Vec::new(),
            // §34 again: the route's gain is in the input token and its bill is in ETH, so the
            // wei-denominated figures the risk layer would have filled are `None`/zero rather
            // than a conversion this layer is not allowed to invent. §52 decides profit.
            simulation_profit_wei: None,
            minimum_required_profit_wei: U256::ZERO,
            funding: self.simulation.funding.clone(),
            simulated_steps: 1,
            sequence: None,
        }
    }
}

/// §6's immutable plan: an [`ArbitrageExecutionPlan`] that validated, with the two derived
/// identities it will be quoted by.
///
/// The plan is held privately and there is no accessor that returns a mutable reference, because
/// §6 lists four stages a plan must not be edited by and a `&mut` handed to one of them is the
/// hole that rule is about. What this type gives instead is recomputation: every call below
/// returns the same bytes for the same value, so a signed transaction can be checked against the
/// plan it came from (§48) without anything having to remember what was built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutablePlan {
    plan: ArbitrageExecutionPlan,
    plan_hash: B256,
    calldata: Bytes,
    calldata_hash: B256,
    route_id: String,
}

impl ExecutablePlan {
    /// §6's boundary: validate, then freeze. The first refusal is reported in full and stops
    /// everything after it — no intent, no build, no nonce, no bytes (§40's `PlanRejected`).
    pub fn new(plan: ArbitrageExecutionPlan, binding: &ExecutionBinding) -> Result<Self> {
        let rejections = plan.validate(binding);
        if !rejections.is_empty() {
            return Err(PlanRejections(rejections).to_error());
        }
        let calldata = plan.calldata();
        let calldata_hash = alloy_primitives::keccak256(calldata.as_ref());
        Ok(Self {
            plan_hash: plan.plan_hash(),
            route_id: plan.route_id(),
            calldata_hash,
            calldata,
            plan,
        })
    }

    pub fn plan(&self) -> &ArbitrageExecutionPlan {
        &self.plan
    }

    pub fn plan_hash(&self) -> B256 {
        self.plan_hash
    }

    /// §23: the calldata, byte-for-byte stable for a given plan.
    pub fn calldata(&self) -> &Bytes {
        &self.calldata
    }

    pub fn calldata_hash(&self) -> B256 {
        self.calldata_hash
    }

    /// §38: the execution-layer route identity.
    pub fn route_id(&self) -> &str {
        &self.route_id
    }

    /// The intent this plan becomes. §24's requirement is that the existing lifecycle consumes
    /// it, so this hands over M6's own type and changes nothing about it.
    pub fn to_intent(&self) -> TransactionIntent {
        self.plan.to_intent()
    }

    /// §8: the plan's freshness statement, given the head the caller already has.
    pub fn freshness_at(&self, head: BlockNumber) -> Freshness {
        self.plan.freshness_at(head)
    }

    /// The gate's plan leg, as this plan and binding answer it. A plan reaching here has
    /// validated — that is what [`Self::new`] is for — so the honest statement is the one the
    /// gate can re-check against the facts it was handed rather than a bare `true`.
    pub fn binding_evidence(&self, binding: &ExecutionBinding) -> PlanBinding {
        let rejections = self.plan.validate(binding);
        if rejections.is_empty() {
            PlanBinding::Valid {
                plan_hash: self.plan_hash,
                route_id: self.route_id.clone(),
            }
        } else {
            PlanBinding::Rejected {
                reason: PlanRejections(rejections).describe(),
            }
        }
    }

    /// §52's evidence line: the identities, in the order a reader asks for them.
    pub fn describe(&self) -> String {
        format!(
            "plan {} calldata {} route {}",
            self.plan_hash,
            self.calldata.len(),
            self.route_id,
        )
    }
}

/// §40's six ways an attempt can end, kept apart rather than collapsed into one failure word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    /// Validation refused the plan. No calldata was produced, so nothing could be signed.
    PlanRejected,
    /// The contract rejected the route under simulation (§26's NC3–NC10). Nothing was sent.
    ContractReverted,
    /// A node was reached and said no. §25: the transaction is not in flight.
    SubmissionFailed,
    /// Bytes were sent and no receipt arrived within the budget. Not a failure of the
    /// transaction — a failure to learn about it.
    ReceiptTimeout,
    /// Included with `status = 0`. The route left no partial state (§13) and earned nothing.
    IncludedReverted,
    /// Included with `status = 1`. §40's last inequality lives here: this is not the same
    /// sentence as "profitable", and §52 decides that from balance deltas.
    IncludedSucceeded,
}

impl ExecutionClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::PlanRejected => "PlanRejected",
            Self::ContractReverted => "ContractReverted",
            Self::SubmissionFailed => "SubmissionFailed",
            Self::ReceiptTimeout => "ReceiptTimeout",
            Self::IncludedReverted => "IncludedReverted",
            Self::IncludedSucceeded => "IncludedSucceeded",
        }
    }

    /// Whether this class is the one §59 and §68 are about: the chain included the route and the
    /// contract's own guards all held. It says nothing about profit.
    pub fn included_successfully(self) -> bool {
        matches!(self, Self::IncludedSucceeded)
    }
}

/// §40's question, as read. Grouped into one value for the same reason
/// [`crate::gate::GateFacts`] is: four positional arguments whose types overlap would let a call
/// site transpose two of them and still compile.
#[derive(Clone, Debug)]
pub struct ClassFacts<'a> {
    /// The executable plan, when validation produced one. `None` is §40's `PlanRejected`: a plan
    /// that never validated has no calldata to have been sent. A plan that *did* validate can also
    /// end in that class, when the runtime refused it before anything was built — that answer
    /// arrives on `stopped_with`, because after validation the plan is present and the refusal is
    /// the fact carrying the information.
    pub executable: Option<&'a ExecutablePlan>,
    /// The refusal this attempt stopped with, when it stopped. `SubmissionRejected` is
    /// §40's `SubmissionFailed`, `PlanRejected` is §40's `PlanRejected`; anything else is a
    /// stop §40 does not name.
    pub stopped_with: Option<&'a ExecutionError>,
    /// Bytes reached a node (§2.2's second boundary).
    pub sent: bool,
    /// What the chain finally said, when it said anything.
    pub receipt: Option<ReceiptStatus>,
}

impl ExecutionClass {
    /// Decide from the facts, in the order the milestone asks them. Returns `None` for a stop
    /// §40's six do not cover — an attempt blocked by the gate, or by a mode that may not
    /// broadcast — because inventing a class for it would be the collapse §40 forbids.
    pub fn decide(facts: &ClassFacts<'_>) -> Option<Self> {
        let Some(executable) = facts.executable else {
            return Some(Self::PlanRejected);
        };
        match &executable.plan.simulation.outcome {
            SimulationOutcome::Reverted { .. } => return Some(Self::ContractReverted),
            // §40's list has no class for "the plan was never simulated", and the honest answer
            // to an unread fact in this repository is that nothing is decided.
            SimulationOutcome::NotRun => return None,
            SimulationOutcome::Succeeded { .. } => {}
        }
        if let Some(ExecutionError::SubmissionRejected(_)) = facts.stopped_with {
            return Some(Self::SubmissionFailed);
        }
        // Checked *after* the outcome, and that order is the point: a route REVM saw revert is
        // §40's `ContractReverted` even though the stage refused to build it, and a plan the
        // runtime refused for a chain or an executor-address disagreement has no revert to read,
        // so the refusal is the only fact that can answer. Deciding the second from the first
        // would name a contract guard that never ran.
        if let Some(ExecutionError::PlanRejected(_)) = facts.stopped_with {
            return Some(Self::PlanRejected);
        }
        match facts.receipt {
            Some(ReceiptStatus::Included) => Some(Self::IncludedSucceeded),
            Some(ReceiptStatus::Reverted) => Some(Self::IncludedReverted),
            // A budget that ran out with no answer (§26) and a node that definitively says the
            // transaction is absent (§25's unknown) are the two remaining `sent` answers; neither
            // is a receipt, and `NotFound` is the one that means the send did not take.
            Some(ReceiptStatus::Timeout) => Some(Self::ReceiptTimeout),
            Some(ReceiptStatus::NotFound) => Some(Self::SubmissionFailed),
            Some(ReceiptStatus::Submitted) | Some(ReceiptStatus::Pending) | None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    const EXECUTOR: Address = address!("00000000000000000000000000000000000000ee");
    const SENDER: Address = address!("00000000000000000000000000000000000000aa");
    const RECIPIENT: Address = address!("00000000000000000000000000000000000000bb");
    const TOKEN_A: Address = address!("00000000000000000000000000000000000000a1");
    const TOKEN_B: Address = address!("00000000000000000000000000000000000000b1");
    const PAIR_0: Address = address!("00000000000000000000000000000000000000f1");
    const PAIR_1: Address = address!("00000000000000000000000000000000000000f2");
    const CHAIN: u64 = 91_342;

    fn leg(index: usize, amount_in: u64, amount_out: u64, min: u64) -> PlanLeg {
        let (token_in, token_out, pool) = match index {
            0 => (TOKEN_A, TOKEN_B, PAIR_0),
            _ => (TOKEN_B, TOKEN_A, PAIR_1),
        };
        PlanLeg {
            pool,
            token_in,
            token_out,
            amount_in: U256::from(amount_in),
            amount_out: U256::from(amount_out),
            min_amount_out: U256::from(min),
            derivation: if index == 0 {
                AmountDerivation::PlanInput
            } else {
                AmountDerivation::PreviousLegOutput
            },
        }
    }

    fn plan_input() -> ArbitrageExecutionPlan {
        ArbitrageExecutionPlan::new(
            CHAIN,
            EXECUTOR,
            SENDER,
            RECIPIENT,
            TOKEN_A,
            U256::from(1_000u64),
            vec![leg(0, 1_000, 1_050, 1_040), leg(1, 1_050, 1_100, 1_090)],
            U256::from(1_060u64),
            PlanValidity {
                simulated_at_block: BlockNumber(37_984_319),
                max_block_age: 3,
                provenance: "test fixture bound".to_string(),
            },
            SimulationContext {
                correlation_id: "m10-test-route".to_string(),
                block_number: BlockNumber(37_984_319),
                block_hash: B256::left_padding_from(&[7]),
                state_fingerprint: "37984319:0".to_string(),
                simulation_id: B256::left_padding_from(&[9]),
                outcome: SimulationOutcome::Succeeded {
                    gas_used: 210_000,
                    proved_gas_limit: 230_000,
                    final_amount: U256::from(1_100u64),
                },
                funding: SenderFunding::RealState {
                    source: "operator balance read at the pinned block".to_string(),
                },
                market: MarketKind::ControlledFixture {
                    proves: "execution system works".to_string(),
                },
            },
            ProfitPolicy {
                denomination: ProfitDenomination::TokenSettled {
                    token: TOKEN_A,
                    reason: "the route round-trips through the input token".to_string(),
                },
                required_final_balance: U256::from(1_060u64),
                provenance: "fixture floor".to_string(),
            },
        )
    }

    fn binding() -> ExecutionBinding {
        ExecutionBinding {
            chain_id: CHAIN,
            executor: EXECUTOR,
        }
    }

    #[test]
    fn a_validated_two_hop_plan_becomes_an_intent_that_names_the_route() {
        let executable = ExecutablePlan::new(plan_input(), &binding())
            .expect("the two-hop fixture plan validates");
        assert_eq!(executable.calldata().len(), 580, "2 legs + 4 scalars");
        // Spelled out rather than re-derived from the fixtures: this string is §38's identity as
        // a reader of the evidence would match on it, so the test is the witness for its exact
        // bytes, not a second copy of the formatting code.
        assert_eq!(
            executable.route_id(),
            "m10-91342-0x00000000000000000000000000000000000000f1>\
             0x00000000000000000000000000000000000000f2\
             -0x00000000000000000000000000000000000000a1>\
             0x00000000000000000000000000000000000000b1>\
             0x00000000000000000000000000000000000000a1"
        );

        // §38's identity is amount-free on purpose, and §6's is not: the same route at a
        // different size is the same route and a different plan.
        let mut resized = plan_input();
        resized.input_amount = U256::from(2_000u64);
        resized.legs[0].amount_in = U256::from(2_000u64);
        let resized = ExecutablePlan::new(resized, &binding())
            .expect("the same route at twice the size is still a valid plan");
        assert_eq!(resized.route_id(), executable.route_id(), "D5");
        assert_ne!(resized.plan_hash(), executable.plan_hash());

        let intent = executable.to_intent();
        assert_eq!(intent.target, EXECUTOR);
        assert_eq!(intent.calldata, executable.calldata().clone());
        assert_eq!(
            intent.value,
            U256::ZERO,
            "§34: an ERC-20 route sends no value"
        );
        assert_eq!(
            intent.simulated_steps, 1,
            "the contract makes the route one step"
        );
        assert!(intent.sequence.is_none());
        assert_eq!(intent.chain_id, CHAIN);
        assert_eq!(intent.ids.opportunity_id, "m10-test-route");
        assert_eq!(intent.state_fingerprint, "37984319:0");
    }

    #[test]
    fn the_same_plan_hash_to_the_same_bytes_and_the_calldata_is_reproducible() {
        let one = plan_input();
        let two = {
            let mut copy = one.clone();
            // Re-derive the second value independently of the first's ordering so a field added
            // later cannot silently join one spelling and not the other.
            copy.legs.reverse();
            copy.legs.reverse();
            copy
        };
        assert_eq!(one.canonical_text(), two.canonical_text());
        assert_eq!(one.plan_hash(), two.plan_hash());
        assert_eq!(one.calldata(), two.calldata(), "D2: same plan, same bytes");
        assert_eq!(
            ExecutablePlan::new(one, &binding())
                .unwrap()
                .calldata_hash(),
            ExecutablePlan::new(two, &binding())
                .unwrap()
                .calldata_hash()
        );
    }

    #[test]
    fn changing_the_route_changes_the_plan_hash_so_the_old_plan_is_a_different_execution() {
        let original = plan_input();
        let hash = original.plan_hash();
        for mutation in ["amount", "floor", "pool", "token", "block", "market"] {
            let mut changed = original.clone();
            match mutation {
                "amount" => changed.legs[0].amount_in += U256::from(1u64),
                "floor" => changed.min_final_output += U256::from(1u64),
                "pool" => changed.legs[1].pool = PAIR_0,
                "token" => changed.legs[0].token_out = TOKEN_A,
                "block" => changed.validity.simulated_at_block = BlockNumber(37_984_320),
                _ => {
                    changed.simulation.market = MarketKind::RealMarket {
                        attested_by: "no such attestation in a unit test".to_string(),
                    };
                }
            }
            assert_ne!(
                changed.plan_hash(),
                hash,
                "§6: {mutation} is part of the plan's identity, so changing it must not read as \
                 the same execution"
            );
        }
    }

    #[test]
    fn every_negative_control_lands_on_its_own_named_rejection() {
        type Mutation = fn(&mut ArbitrageExecutionPlan);
        let cases: Vec<(&str, Mutation)> = vec![
            ("wrong_chain", |p| p.chain_id = 1),
            ("wrong_executor", |p| {
                p.executor = Address::left_padding_from(&[0x5e])
            }),
            ("zero_amount", |p| p.input_amount = U256::ZERO),
            ("no_legs", |p| p.legs.clear()),
            ("too_many_legs", |p| {
                p.legs = (0..5).map(|i| leg(i % 2, 1_000, 1_050, 1_040)).collect();
            }),
            ("leg_self_loop", |p| p.legs[0].token_out = TOKEN_A),
            ("broken_continuity", |p| {
                p.legs[1].token_in = TOKEN_A;
                p.legs[1].token_out = TOKEN_B;
            }),
            ("not_round_trip", |p| {
                p.legs[1].token_out = TOKEN_B;
                p.min_final_output = U256::from(1_060u64);
            }),
            ("amount_derivation", |p| {
                p.legs[1].amount_in = U256::from(999u64);
            }),
            ("ask_below_floor", |p| {
                p.legs[0].min_amount_out = U256::from(2_000u64)
            }),
            ("no_leg_floor", |p| p.legs[0].min_amount_out = U256::ZERO),
            ("final_floor_disagreement", |p| {
                p.profit.required_final_balance = U256::from(1_061u64)
            }),
            ("wrong_denomination", |p| {
                p.profit.denomination = ProfitDenomination::NativeWei;
            }),
            ("simulated_limit_below_burn", |p| {
                p.simulation.outcome = SimulationOutcome::Succeeded {
                    gas_used: 230_000,
                    proved_gas_limit: 210_000,
                    final_amount: U256::from(1_100u64),
                };
            }),
        ];
        for (code, mutate) in cases {
            let mut broken = plan_input();
            mutate(&mut broken);
            let rejections = broken.validate(&binding());
            let codes = rejections
                .iter()
                .map(PlanRejection::code)
                .collect::<Vec<_>>();
            assert!(
                codes.contains(&code),
                "{code} must be refused by name; this plan produced {codes:?}"
            );
        }
        assert!(plan_input().validate(&binding()).is_empty());
    }

    #[test]
    fn rejections_are_reported_together_and_in_one_stable_order() {
        let mut broken = plan_input();
        broken.chain_id = 1;
        broken.executor = Address::left_padding_from(&[0x5e]);
        broken.input_amount = U256::ZERO;
        let first = broken.validate(&binding());
        let second = broken.validate(&binding());
        assert_eq!(
            first, second,
            "D4: the same broken plan names the same refusals"
        );
        assert_eq!(
            first.iter().map(PlanRejection::code).collect::<Vec<_>>(),
            [
                "wrong_chain",
                "wrong_executor",
                "zero_amount",
                "amount_derivation"
            ],
            "the two bindings the contract cannot see come first, because they decide whether \
             the plan is about this deployment at all"
        );
        let error = PlanRejections(first).to_error();
        assert!(
            matches!(error, ExecutionError::PlanRejected(_)),
            "§40: a refused plan is its own class, not an InvalidIntent with a string"
        );
    }

    #[test]
    fn the_send_limit_is_resolved_against_the_proved_limit_and_not_the_burn() {
        // EIP-150: a call frame hands its callee at most 63/64 of the gas it has left, so a
        // transaction limited to exactly what the simulation burned can still starve the deepest
        // nested frame. §57's first real attempt was limited that way — 229,302 burned, 249,302
        // sent — and the second pair ran out of gas while paying out
        // (`UniswapV2: TRANSFER_FAILED`).
        let executable = ExecutablePlan::new(plan_input(), &binding())
            .expect("the two-hop fixture plan validates");
        assert_eq!(
            executable.to_intent().gas_limit,
            230_000,
            "§13: the measurement the builder resolves its margin against is the limit the run \
             was executed under. 210,000 is what it burned, and a burn is not a safe limit"
        );

        let mut burned = plan_input();
        burned.simulation.outcome = SimulationOutcome::Succeeded {
            gas_used: 230_000,
            proved_gas_limit: 210_000,
            final_amount: U256::from(1_100u64),
        };
        let rejections = burned.validate(&binding());
        assert_eq!(
            rejections
                .iter()
                .map(PlanRejection::code)
                .collect::<Vec<_>>(),
            ["simulated_limit_below_burn"],
            "a run cannot burn more gas than it was allowed to, so this pair of numbers is not \
             from one run and the plan cannot choose a send limit out of them"
        );
        let reason = rejections[0].reason();
        assert!(
            reason.contains("230000") && reason.contains("210000"),
            "the refusal names both numbers it compared: {reason}"
        );
    }

    #[test]
    fn freshness_answers_in_the_existing_gate_terms_and_invents_no_rule() {
        let plan = plan_input();
        assert_eq!(
            plan.freshness_at(BlockNumber(37_984_319)),
            Freshness::Active,
            "the plan's own block is never stale against itself"
        );
        assert_eq!(
            plan.freshness_at(BlockNumber(37_984_322)),
            Freshness::Active,
            "the declared bound is inclusive at its edge"
        );
        let stale = plan.freshness_at(BlockNumber(37_984_323));
        assert!(
            matches!(stale, Freshness::Stale { .. }),
            "one block past the bound is stale: {stale:?}"
        );
        // A plan pinned ahead of the head is refused too: it describes state that has not
        // happened, and §31 has no version of "act on the future" that ends well.
        assert!(matches!(
            plan.freshness_at(BlockNumber(37_984_318)),
            Freshness::Stale { .. }
        ));
    }

    #[test]
    fn the_six_error_classes_stay_apart_across_the_three_boundaries() {
        let executable = ExecutablePlan::new(plan_input(), &binding()).unwrap();
        let decided = |facts: ClassFacts<'_>| ExecutionClass::decide(&facts).map(|c| c.name());
        assert_eq!(
            decided(ClassFacts {
                executable: None,
                stopped_with: None,
                sent: false,
                receipt: None,
            }),
            Some("PlanRejected")
        );

        let mut reverted_plan = plan_input();
        reverted_plan.simulation.outcome = SimulationOutcome::Reverted {
            revert: evm_protocol::ExecutorRevert::FinalShortfall {
                delivered: U256::from(1_000u64),
                floor: U256::from(1_060u64),
            }
            .name()
            .to_string(),
        };
        let reverted = ExecutablePlan::new(reverted_plan, &binding()).unwrap();
        assert_eq!(
            decided(ClassFacts {
                executable: Some(&reverted),
                stopped_with: None,
                sent: false,
                receipt: None,
            }),
            Some("ContractReverted"),
            "a route the contract refused under simulation must not read as a send failure"
        );

        for (name, facts) in [
            (
                "SubmissionFailed",
                ClassFacts {
                    executable: Some(&executable),
                    stopped_with: Some(&ExecutionError::SubmissionRejected("no".to_string())),
                    sent: false,
                    receipt: None,
                },
            ),
            (
                "ReceiptTimeout",
                ClassFacts {
                    executable: Some(&executable),
                    stopped_with: None,
                    sent: true,
                    receipt: Some(ReceiptStatus::Timeout),
                },
            ),
            (
                "IncludedReverted",
                ClassFacts {
                    executable: Some(&executable),
                    stopped_with: None,
                    sent: true,
                    receipt: Some(ReceiptStatus::Reverted),
                },
            ),
            (
                "IncludedSucceeded",
                ClassFacts {
                    executable: Some(&executable),
                    stopped_with: None,
                    sent: true,
                    receipt: Some(ReceiptStatus::Included),
                },
            ),
        ] {
            assert_eq!(decided(facts), Some(name));
        }

        // §40's first inequality, stated as a test: bytes sent with no answer is not a class.
        assert_eq!(
            decided(ClassFacts {
                executable: Some(&executable),
                stopped_with: None,
                sent: true,
                receipt: Some(ReceiptStatus::Pending),
            }),
            None,
            "submitted is not included, and an attempt that is only submitted has no §40 class"
        );
        assert!(
            ExecutionClass::IncludedSucceeded.included_successfully(),
            "and that word still does not mean profitable: §52 decides profit from deltas"
        );
    }

    #[test]
    fn an_unexecutable_plan_never_reaches_an_intent_or_a_build() {
        let mut broken = plan_input();
        broken.executor = Address::left_padding_from(&[0x5e]);
        let error = ExecutablePlan::new(broken, &binding())
            .expect_err("§36: a plan for another executor is refused");
        let ExecutionError::PlanRejected(why) = error else {
            panic!("the refusal has to be §40's PlanRejected, not {error:?}");
        };
        assert!(why.contains("configured executor"), "{why}");
    }

    #[test]
    fn the_binding_leg_repeats_the_check_the_gate_will_be_handed() {
        let executable = ExecutablePlan::new(plan_input(), &binding()).unwrap();
        assert!(matches!(
            executable.binding_evidence(&binding()),
            PlanBinding::Valid { .. }
        ));
        let elsewhere = ExecutionBinding {
            chain_id: 1,
            executor: EXECUTOR,
        };
        let PlanBinding::Rejected { reason } = executable.binding_evidence(&elsewhere) else {
            panic!("a plan asked to bind to another chain cannot be Valid");
        };
        assert!(reason.contains("§7"), "{reason}");
    }

    #[test]
    fn a_legs_array_encodes_in_route_order_and_decodes_back_to_the_plan() {
        let executable = ExecutablePlan::new(plan_input(), &binding()).unwrap();
        let decoded = evm_protocol::decode_calldata(executable.calldata().as_ref())
            .expect("the plan's own bytes decode");
        let ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } = decoded
        else {
            panic!("a plan encodes exactly one of the executor's nine calls");
        };
        assert_eq!(legs.len(), 2);
        assert_eq!(legs[0].pool, PAIR_0);
        assert_eq!(legs[1].pool, PAIR_1, "route order survives the encoding");
        assert_eq!(legs[1].amount_in, U256::from(1_050u64));
        assert_eq!(input_token, TOKEN_A);
        assert_eq!(amount_in, U256::from(1_000u64));
        assert_eq!(min_final_amount, U256::from(1_060u64));
        assert_eq!(recipient, RECIPIENT);
    }
}
