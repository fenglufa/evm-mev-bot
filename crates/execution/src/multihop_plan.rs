//! §29/§30: the plan M11 hands to M10's executor, and the binding check that lets it.
//!
//! §29 is a prohibition with a positive half: M11 adds no `FinalExecutionPlan`, it generates the
//! [`ArbitrageExecutionPlan`] M10 already defines, from the eleven fields M10 already names
//! (`chain_id`, `executor`, `sender`, `recipient`, `input_token`, `input_amount`, `legs`,
//! `min_final_output`, `validity`, `simulation`, `profit_policy`). So this module is a translator,
//! not a model: every monetary field and every address it puts in a plan is read out of a
//! [`SimulatedOpportunity`] or out of the [`MultihopAcceptance`] a risk pass granted over that
//! same record, and the only values it takes as arguments are the ones a simulation structurally
//! cannot know — which wallet signs, how old a plan the operator will stand, whether the state was
//! funded for real, and what the run is being claimed to prove (§29's market label).
//!
//! ```text
//! SimulatedOpportunity  +  MultihopAcceptance  +  MultihopPlanContext
//!   -> plan_from_simulation       §30: three route identities, two calldata hashes
//!   -> ExecutablePlan             (M10's own type, M10's own validate())
//!   -> M10's gate, builder, signer, submitter — unchanged
//! ```
//!
//! ## Why the refusal is here rather than in a test
//!
//! §30 requires the plan to *prove* `simulation calldata hash == execution calldata hash` and
//! `route identity == simulation route identity == execution route identity`, and to refuse
//! execution otherwise. A test can assert that equality for the fixtures it builds; only the
//! builder can refuse a caller that hands it a record which does not satisfy it. So both checks
//! run inside [`plan_from_simulation`], on values read from the two sides independently, and the
//! [`MultihopBinding`] they consult is public so an evidence table can quote the same six strings
//! the decision was taken on rather than a paraphrase of them.
//!
//! ## What a mismatch would mean
//!
//! Every field of a `SimulatedOpportunity` is public, so a record can reach this layer with legs
//! that no longer match the candidate they were priced from, an outcome that no longer matches the
//! call that produced it, or an acceptance that was granted over a different run. None of those is
//! a market fact and none is a warning: a plan built on any of them would be a claim that a
//! simulation happened which the simulation itself contradicts. Each is refused by name.
//!
//! §28's boundary survives the translation: this crate is downstream of risk, never upstream of
//! it, and nothing here signs or sends. The [`ExecutablePlan`] this returns is the handoff — the
//! same object M10's stage already consumes.

use alloy_primitives::{Address, B256};
use serde::Serialize;

use evm_protocol::{ExecutorCall, ExecutorLeg};
use evm_risk::MultihopAcceptance;
use evm_simulation::{legs, SimulatedOpportunity};

use crate::arbitrage::{
    AmountDerivation, ArbitrageExecutionPlan, ExecutablePlan, ExecutionBinding, PlanLeg,
    PlanValidity, ProfitPolicy, SimulationContext, SimulationOutcome,
};
use crate::intent::SenderFunding;
use crate::market::MarketKind;
use crate::profit::ProfitDenomination;

/// The operator-owned facts a simulation does not carry.
///
/// Each field is here because the record cannot answer it: [`SimulatedOpportunity`] knows the
/// executor, the recipient and the gas numbers, but not which wallet will sign, how long the
/// operator will stand an old plan, whether the sender's balance came from the chain or from an
/// override, or what the run is entitled to claim about the market.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultihopPlanContext {
    /// The account that will sign. Checked against the run's operator, because the balances,
    /// reserves and allowances the simulation measured are the operator's: a plan sent from
    /// another wallet is an execution of a different funding story than the one that was run.
    pub sender: Address,
    /// §39's opaque id for the finding (or the fixture) this plan is about, verbatim as its
    /// producer spelled it — the same rule M10 states for
    /// [`SimulationContext::correlation_id`].
    pub correlation_id: String,
    /// §8's window, declared by the caller and carried verbatim into
    /// [`PlanValidity::max_block_age`]. M11 adds no freshness semantics: the risk layer's
    /// `maximum_simulation_age` is a judgement about a run, this is the plan's own expiry, and the
    /// pre-submit gate is what turns either into a stop.
    pub max_block_age: u64,
    /// Where `max_block_age` came from, for §43's rows.
    pub validity_provenance: String,
    /// §34's answer: was the sender's ability to pay read off the pinned state or written into it.
    pub funding: SenderFunding,
    /// §29's label with its evidence. A `ControlledFixture` plan may complete the whole ladder and
    /// still be forbidden from reading as a market opportunity.
    pub market: MarketKind,
    /// The state version the route was priced on, spelled the way the pipeline spells it. Taken as
    /// an argument rather than built here because the spelling belongs to the state engine, and an
    /// execution crate that invented one would be asserting a fingerprint it never computed.
    pub state_fingerprint: String,
    /// Where the plan's floor came from, for [`ProfitPolicy::provenance`].
    pub floor_provenance: String,
}

/// Why a simulation could not become a plan, named rather than described.
///
/// The same reasoning M10 gives for [`crate::arbitrage::PlanRejection`]: a caller counting
/// refusals needs a rule label, not a sentence to pattern-match.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MultihopPlanRefusal {
    /// §30: the two calldata hashes differ, so the bytes that would be signed are not the bytes
    /// that were simulated. This is the headline refusal §30 asks for, and the one a reader should
    /// quote first, because every field-level cause below also shows up here.
    #[error(
        "§30: the simulation's calldata hashes to {simulation} and this plan's hashes to \
         {execution}; the transaction that would be signed is not the run that was judged"
    )]
    CalldataHashMismatch { simulation: B256, execution: B256 },
    /// §30: the three route identities are not one identity. Quoted in full because which of the
    /// three differs is the diagnosis — `priced` against `simulated` is a route that moved after
    /// it was priced, `simulated` against `execution` is a plan built from someone else's legs.
    #[error(
        "§30: the priced route is {priced}, the simulated route is {simulated} and the \
             execution route is {execution}"
    )]
    RouteIdMismatch {
        priced: String,
        simulated: String,
        execution: String,
    },
    /// The plan's own call, re-encoded by [`ArbitrageExecutionPlan::to_call`], is not the call the
    /// simulation ran. The hash equality would already refuse this plan; the named variant says
    /// whether it was the selector or the arguments, rather than leaving a reader to guess between
    /// an amount, a token, a pool and a recipient.
    #[error(
        "§30: re-encoding the plan produced the call `{plan_signature}` where the run made \
         `{simulation_signature}`, or the same call with different arguments"
    )]
    CallDiverged {
        plan_signature: &'static str,
        simulation_signature: &'static str,
    },
    /// §26: the acceptance does not describe this record. Both numbers are quoted, because the
    /// failure this guards is a stale grant handed to a fresh run — and which side is the old one
    /// is a question the caller can only answer from the pair.
    #[error("§26: the acceptance says {acceptance} for `{field}` and the record says {record}")]
    AcceptanceMismatch {
        field: String,
        acceptance: String,
        record: String,
    },
    /// The run delivered nothing, so there is no delivery to plan an execution around. Reached when
    /// a record was edited after the risk pass, since an acceptance cannot have been granted over
    /// a non-delivery.
    #[error(
        "§26: the record's ending is `{status}`, so no acceptance over a delivery binds to it"
    )]
    NotDelivered { status: &'static str },
    /// §27's route validity, re-asked at this boundary. The risk layer asks the same question; both
    /// ask it because the two layers can be handed different objects.
    #[error("§27/§30: the run's legs are not the priced route's legs: {detail}")]
    RouteValidity { detail: String },
    /// §7/§36's binding, stated at the plan's own edge: a plan whose sender is not the account the
    /// run funded has no claim on the balances that run measured.
    #[error(
        "§30: the run was made by operator {simulated:#x} and this plan would be sent by \
         {plan:#x}; the simulation's balances and allowances are the operator's, not the sender's"
    )]
    SenderNotOperator { plan: Address, simulated: Address },
    /// M10's own rejection list, forwarded verbatim: this module does not re-implement the plan
    /// checks, and a plan M10 would not execute is not made executable by arriving through M11's
    /// door.
    #[error("§15-§17: M10 refused the plan it was handed: {detail}")]
    PlanRejected { detail: String },
}

impl MultihopPlanRefusal {
    /// The label an evidence row groups by.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CalldataHashMismatch { .. } => "calldata_hash_mismatch",
            Self::RouteIdMismatch { .. } => "route_identity_mismatch",
            Self::CallDiverged { .. } => "call_diverged",
            Self::AcceptanceMismatch { .. } => "acceptance_mismatch",
            Self::NotDelivered { .. } => "not_delivered",
            Self::RouteValidity { .. } => "route_validity",
            Self::SenderNotOperator { .. } => "sender_not_operator",
            Self::PlanRejected { .. } => "plan_rejected",
        }
    }
}

/// §30's six quotable values, computed from the two sides independently.
///
/// The hashes come from [`SimulatedOpportunity::calldata_hash`] (a keccak over the encoded call the
/// run actually made) and [`ArbitrageExecutionPlan::calldata_hash`] (a keccak over the encoded call
/// this plan will be signed with) — two paths, through two crates, over two field sets, so their
/// equality is a comparison and not a tautology. The three route identities are the same route
/// spelled from the priced legs, from the legs that ran, and from M10's own
/// [`ArbitrageExecutionPlan::route_id`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MultihopBinding {
    /// §23's simulation identity: `m11-sim-{chain}-{block}-{hash}`.
    pub simulation_id: String,
    pub simulation_calldata_hash: B256,
    pub execution_calldata_hash: B256,
    pub plan_hash: B256,
    pub priced_route_id: String,
    pub simulated_route_id: String,
    pub execution_route_id: String,
}

impl MultihopBinding {
    /// Read the six values off a simulation and the plan claimed to come from it.
    pub fn observe(simulated: &SimulatedOpportunity, plan: &ArbitrageExecutionPlan) -> Self {
        let chain = simulated.chain_id.0;
        Self {
            simulation_id: simulated.identity(),
            simulation_calldata_hash: simulated.calldata_hash(),
            execution_calldata_hash: plan.calldata_hash(),
            plan_hash: plan.plan_hash(),
            priced_route_id: route_id(chain, &priced_legs_for_display(simulated)),
            simulated_route_id: route_id(chain, call_legs(&simulated.run.call)),
            execution_route_id: plan.route_id(),
        }
    }

    /// Are the three route identities one identity and the two calldata hashes one hash?
    pub fn bound(&self) -> bool {
        self.unbound().is_none()
    }

    /// The first §30 equality that fails, as the refusal it is at the boundary. The hashes are
    /// checked before the route strings: a hash is the tighter claim, and when both fail the hash
    /// is the one a reader should quote.
    ///
    /// Public because [`plan_from_simulation`] asks this question after a field-level comparison
    /// that fires first — the two sides hash the same [`ExecutorCall`] type, so a plan whose call
    /// diverges is refused as [`MultihopPlanRefusal::CallDiverged`] before its hashes are read. A
    /// caller that wants to see the hash check answer for itself consults it here, over a simulation
    /// and a plan that were not built as a pair.
    pub fn unbound(&self) -> Option<MultihopPlanRefusal> {
        if self.simulation_calldata_hash != self.execution_calldata_hash {
            return Some(MultihopPlanRefusal::CalldataHashMismatch {
                simulation: self.simulation_calldata_hash,
                execution: self.execution_calldata_hash,
            });
        }
        if self.priced_route_id != self.simulated_route_id
            || self.simulated_route_id != self.execution_route_id
        {
            return Some(MultihopPlanRefusal::RouteIdMismatch {
                priced: self.priced_route_id.clone(),
                simulated: self.simulated_route_id.clone(),
                execution: self.execution_route_id.clone(),
            });
        }
        None
    }
}

/// The priced legs, for the route identity that has to be reportable even when it is the side that
/// broke.
///
/// [`evm_simulation::legs`] refuses a candidate whose quote is truncated or whose circle never
/// closed, and this is a display path — the refusal itself is raised by
/// [`plan_from_simulation`], which asks the same question first — so an unspellable route is
/// spelled as no legs at all. The empty list produces a route identity that cannot equal either of
/// the other two, which keeps a display failure pointing the same way a refusal does: away from
/// execution.
fn priced_legs_for_display(simulated: &SimulatedOpportunity) -> Vec<ExecutorLeg> {
    legs(&simulated.candidate).unwrap_or_else(|_| Vec::new())
}

/// §27/§30's route identity, spelled the way M10 spells it in
/// [`ArbitrageExecutionPlan::route_id`]: the chain, the pools in trade order, the token transitions
/// in trade order — and no amounts and no block, since a route is the same route at two sizes (§55)
/// and the block belongs to the simulation's identity, not the route's.
///
/// The `m10-` prefix is kept deliberately: this is M10's execution-layer identity computed over a
/// leg list that came from somewhere else, which is exactly what §30's three-way comparison needs.
/// If M10 ever changes the spelling, this helper and `route_id()` stop agreeing, and the result is
/// a refusal rather than a silent pass — the safe direction for a drift this easy to miss.
fn route_id(chain_id: u64, legs: &[ExecutorLeg]) -> String {
    let pools = legs
        .iter()
        .map(|leg| format!("{:#x}", leg.pool))
        .collect::<Vec<_>>()
        .join(">");
    let mut tokens = legs
        .iter()
        .map(|leg| format!("{:#x}", leg.token_in))
        .collect::<Vec<_>>();
    if let Some(last) = legs.last() {
        tokens.push(format!("{:#x}", last.token_out));
    }
    format!("m10-{chain_id}-{pools}-{}", tokens.join(">"))
}

/// The legs a call carries, or the empty list for a call that is not a route. A `withdraw` call
/// cannot reach a plan builder in practice — a `SimulatedOpportunity` refuses to file one — and the
/// empty list is what makes it refuse here too, through [`route_id`] producing an identity that
/// matches nothing.
fn call_legs(call: &ExecutorCall) -> &[ExecutorLeg] {
    match call {
        ExecutorCall::Execute { legs, .. } => legs.as_slice(),
        _ => &[],
    }
}

/// §29/§30: the M10 plan for a simulation a risk layer accepted, before it is frozen.
///
/// Ordered so each refusal names the earliest thing that is wrong: the record's agreement with its
/// own pricing, then the acceptance's agreement with the record, then the sender, then the plan
/// built from the record's legs, then §30's hashes over that plan. M10's own validation runs last,
/// in [`executable_plan`], because it is a different fact about a different object.
pub fn plan_from_simulation(
    simulated: &SimulatedOpportunity,
    acceptance: &MultihopAcceptance,
    context: &MultihopPlanContext,
) -> Result<ArbitrageExecutionPlan, MultihopPlanRefusal> {
    // §30's precondition, asked before anything is built: if the record no longer describes its
    // own candidate, the legs this function would put in a plan are a fiction.
    simulated
        .route_agrees_with_pricing()
        .map_err(|error| MultihopPlanRefusal::RouteValidity {
            detail: error.to_string(),
        })?;

    let delivered = simulated
        .status
        .delivered()
        .ok_or(MultihopPlanRefusal::NotDelivered {
            status: simulated.status.name(),
        })?;

    // The acceptance has to be about THIS record. Five figures, each compared as a pair of spelled
    // numbers so a mismatch quotes both sides in the units they were granted in.
    let grants: [(&str, String, String); 5] = [
        (
            "input_amount",
            acceptance.input_amount.to_string(),
            simulated.input_amount.to_string(),
        ),
        (
            "delivered",
            acceptance.delivered.to_string(),
            delivered.to_string(),
        ),
        (
            "min_final_output",
            acceptance.min_final_output.to_string(),
            simulated.min_final_output.to_string(),
        ),
        (
            "gas_used",
            acceptance.gas_used.to_string(),
            simulated.gas_used.to_string(),
        ),
        (
            "simulation_block",
            acceptance.simulation_block.0.to_string(),
            simulated.simulation_block.number.0.to_string(),
        ),
    ];
    for (field, granted, recorded) in grants {
        if granted != recorded {
            return Err(MultihopPlanRefusal::AcceptanceMismatch {
                field: field.to_string(),
                acceptance: granted,
                record: recorded,
            });
        }
    }

    if context.sender != simulated.run.operator {
        return Err(MultihopPlanRefusal::SenderNotOperator {
            plan: context.sender,
            simulated: simulated.run.operator,
        });
    }

    // The legs, taken from the call that ran rather than re-derived from the candidate. The two
    // agree at this point — `route_agrees_with_pricing` just said so — and this is the source whose
    // bytes the hash check compares against, so it is the source the plan is built from.
    let plan_legs = call_legs(&simulated.run.call)
        .iter()
        .enumerate()
        .map(|(index, leg)| PlanLeg {
            pool: leg.pool,
            token_in: leg.token_in,
            token_out: leg.token_out,
            amount_in: leg.amount_in,
            amount_out: leg.amount_out,
            min_amount_out: leg.min_amount_out,
            derivation: if index == 0 {
                AmountDerivation::PlanInput
            } else {
                AmountDerivation::PreviousLegOutput
            },
        })
        .collect::<Vec<_>>();

    let plan = ArbitrageExecutionPlan::new(
        simulated.chain_id.0,
        simulated.run.executor,
        context.sender,
        simulated.outcome.recipient,
        simulated.outcome.input_token,
        simulated.input_amount,
        plan_legs,
        simulated.min_final_output,
        PlanValidity {
            simulated_at_block: simulated.simulation_block.number,
            max_block_age: context.max_block_age,
            provenance: context.validity_provenance.clone(),
        },
        SimulationContext {
            correlation_id: context.correlation_id.clone(),
            block_number: simulated.simulation_block.number,
            block_hash: simulated.simulation_block.hash,
            state_fingerprint: context.state_fingerprint.clone(),
            // §23's identity is the run's own: chain, block, and a keccak over the canonical
            // spelling of the call, the outcome and the guard. Not a fresh number for the plan.
            simulation_id: simulated.identity_hash(),
            outcome: SimulationOutcome::Succeeded {
                gas_used: simulated.gas_used,
                // The limit the run executed under, which is what M10's §13 gas policy resolves the
                // send limit against — `run.gas_limit` is that number, `gas_used` is not.
                proved_gas_limit: simulated.run.gas_limit,
                final_amount: delivered,
            },
            funding: context.funding.clone(),
            market: context.market.clone(),
        },
        ProfitPolicy {
            denomination: ProfitDenomination::TokenSettled {
                token: simulated.outcome.input_token,
                reason: "§34: the route round-trips in its input token, so the floor the contract \
                         enforces and the floor this policy requires are the same asset"
                    .to_string(),
            },
            // §12's single statement of the floor: the number the call carries. The risk acceptance
            // confirms it; it does not set it.
            required_final_balance: simulated.min_final_output,
            provenance: context.floor_provenance.clone(),
        },
    );

    // §30, on the plan as built. The call comparison is the diagnostic and the binding is the
    // claim: equal calldata hashes, and one route identity across the three sources.
    let plan_call = plan.to_call();
    if plan_call != simulated.run.call {
        return Err(MultihopPlanRefusal::CallDiverged {
            plan_signature: plan_call.signature(),
            simulation_signature: simulated.run.call.signature(),
        });
    }
    match MultihopBinding::observe(simulated, &plan).unbound() {
        Some(refusal) => Err(refusal),
        None => Ok(plan),
    }
}

/// §30's second half: freeze the plan through M10's own boundary, after the binding is proven.
///
/// Split from [`plan_from_simulation`] because the two refusals are different facts for different
/// audiences — a binding failure says the record and the plan are not about the same run, a
/// validation failure says the plan is not executable at all — and §40's error taxonomy lives on
/// this side of the split.
///
/// The returned [`MultihopBinding`] is read off the frozen plan, so the numbers an evidence row
/// quotes are the numbers the executable object carries.
pub fn executable_plan(
    simulated: &SimulatedOpportunity,
    acceptance: &MultihopAcceptance,
    context: &MultihopPlanContext,
    binding: &ExecutionBinding,
) -> Result<(ExecutablePlan, MultihopBinding), MultihopPlanRefusal> {
    let plan = plan_from_simulation(simulated, acceptance, context)?;
    let executable =
        ExecutablePlan::new(plan, binding).map_err(|error| MultihopPlanRefusal::PlanRejected {
            detail: error.to_string(),
        })?;
    let observed = MultihopBinding::observe(simulated, executable.plan());
    Ok((executable, observed))
}
