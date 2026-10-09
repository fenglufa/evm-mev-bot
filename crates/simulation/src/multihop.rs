//! §19–§25: the multi-hop candidate put in front of the EVM, and the answer read
//! back.
//!
//! §19 is a prohibition with a reason behind it. The M10 executor run already
//! executes `EOA → Executor → Pool → Pool → … → EOA` against real bytecode at a
//! pinned header, and it is the thing the chain actually does. A second simulator
//! written here would be a second opinion about the same call, and the two would
//! drift; so this module builds the request [`ExecutorRun`][crate::executor::ExecutorRun]
//! and reads back [`ExecutorOutcome`][crate::executor::ExecutorOutcome], and owns
//! no EVM, no provider and no REVM type.
//!
//! ```text
//! OptimizedCandidate                  (§18; this module's only input)
//!        ↓  legs                      §20/§21
//! Vec<ExecutorLeg>                    the protocol crate's own leg type
//!        ↓  executor_run              §19
//! ExecutorRun                         (M10)
//!        ↓  crate::executor::run      the one existing run
//! ExecutorOutcome                     (M10)
//!        ↓  SimulatedOpportunity::new §23/§24/§25
//! SimulatedOpportunity
//! ```
//!
//! ## What the legs carry, and where each number comes from (§20/§21)
//!
//! One leg per priced hop, in trade order, with `amount_in` and `amount_out`
//! taken from the quote's own hop records rather than recomputed. The chain §21
//! demands — leg *k*'s input is leg *k-1*'s output — is therefore not enforced by
//! arithmetic on the caller's numbers: this module checks the quote's chain and
//! refuses if it is broken, which is the same distinction the contract draws with
//! `AmountChainBroken`.
//!
//! Each leg's `min_amount_out` is the quoted output, exactly. That is M10's rule
//! restated rather than a new one — the contract requires the pair to deliver the
//! figure the plan claims (`DeliveryMismatch`) — so a leg floor looser than the
//! quote would be a floor the simulation never tests. Slippage tolerance is not
//! this module's to invent; §27's risk layer decides whether a route is worth
//! asking at all, and the final guard is a parameter of [`executor_run`].
//!
//! ## What a failed simulation is, and what it is never called (§25)
//!
//! [`SimulationStatus`] keeps the EVM's endings and the contract's named error, so
//! "this route lost money", "the contract rejected it", "it ran out of gas" and
//! "the harness would not run it" stay four different sentences. A run that did
//! not deliver produces no [`Gross`] at all: [`SimulatedOpportunity::gross`] is
//! `None`, never a zero — the number zero already means "exactly even", and §4
//! forbids spending that meaning on a run that delivered nothing.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};
use evm_opportunity::{Gross, OptimizedCandidate};
use evm_protocol::{ExecutorCall, ExecutorLeg, MAX_LEGS};

use crate::executor::{ExecutorOutcome, ExecutorRun};
use crate::gas::{GasCharge, GasPricing};
use crate::request::EvmRules;
use crate::result::StepStatus;
use crate::state::BlockPin;

/// Why a candidate could not become a request.
///
/// Every arm here is a refusal to ask, not an answer from the chain: the candidate
/// and the caller's configuration disagree about what the route is, so no bytes
/// are produced and nothing is executed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MultiHopBuildError {
    /// The quote stopped before the route did, because a hop bought less than one
    /// whole unit. There is no leg to build from a hop that never ran.
    #[error(
        "the quote covers {quoted} of the route's {route} hops, stopping at hop {stopped_at}; a \
         route truncated by the AMM's own floor has no leg at that index"
    )]
    TruncatedQuote {
        route: usize,
        quoted: usize,
        stopped_at: usize,
    },
    /// §21: a leg's input is not the previous leg's output.
    #[error(
        "amount chain broken at leg {index}: the leg before it delivers {expected} and this leg \
         asks {found}"
    )]
    AmountChainBroken {
        index: usize,
        expected: U256,
        found: U256,
    },
    /// Leg 0 does not spend the amount the search says it searched for.
    #[error(
        "leg 0 spends {found} of the input token, but the search's best input is {expected}; a \
         plan built at a different amount than the one priced is not this candidate"
    )]
    InputMismatch { expected: U256, found: U256 },
    /// The quote's first hop is not entered on the route's input token.
    #[error("the quote starts on {found} but the route was entered on {expected}")]
    WrongInputToken { expected: Address, found: Address },
    /// The route does not end where it began.
    #[error(
        "the last leg delivers {found} but the route was entered on {expected}; the round trip \
         never closed"
    )]
    NotACycle { expected: Address, found: Address },
    /// The contract's own cap on legs. Pricing a four-hop route is legal (§45);
    /// asking the executor for more than `MAX_LEGS` is not, and refusing here
    /// saves a transaction that would revert.
    #[error("the route has {count} legs and the executor contract carries at most {maximum}")]
    TooManyLegs { count: usize, maximum: u64 },
    /// The run was asked at a block the candidate was not priced at.
    #[error(
        "the candidate is priced at block {} but the run pins block {}; the pinned block is part \
         of the simulation's identity and cannot be restated by the caller",
        priced.0,
        pinned.0
    )]
    BlockMismatch {
        priced: BlockNumber,
        pinned: BlockNumber,
    },
    /// The run belongs to another chain than the route does.
    #[error("the candidate is a route on chain {} and the run is on chain {}", expected.0, found.0)]
    RunChainMismatch { expected: ChainId, found: ChainId },
    /// The run's legs are not the legs this candidate's quote produces.
    #[error("leg {index} of the run is not the leg this candidate produces")]
    RunLegMismatch { index: usize },
    /// The run asks for something other than a route. A withdrawal has no legs, so
    /// filing its outcome under an arbitrage candidate would attach a balance move
    /// to a route that never ran.
    #[error("the run's call is `{signature}`, not an execute call: a simulation filed under a candidate must be a run of that candidate's route")]
    NotAnExecuteCall { signature: &'static str },
    /// The final guard is above what the route could ever deliver: a floor nobody
    /// can meet is a plan that must revert, and asking for one is a bug at the call
    /// site rather than a fact about the market.
    #[error(
        "the final guard is {min_final_output} but this route is priced to deliver {quoted}; a \
         floor above the quote cannot be met by the route it is attached to"
    )]
    UnreachableGuard {
        min_final_output: U256,
        quoted: U256,
    },
}

/// The route's legs, in trade order, exactly as the quote priced them (§20/§21).
///
/// Every field is read out of the candidate's own hop records, so the numbers a
/// later §30 binding compares are the numbers this function was given.
pub fn legs(candidate: &OptimizedCandidate) -> Result<Vec<ExecutorLeg>, MultiHopBuildError> {
    let route = &candidate.route;
    let quote = &candidate.search.quote;
    let hop_count = route.hop_count();
    if quote.hops.len() != hop_count {
        return Err(MultiHopBuildError::TruncatedQuote {
            route: hop_count,
            quoted: quote.hops.len(),
            stopped_at: quote.hops.len(),
        });
    }
    if hop_count > MAX_LEGS as usize {
        return Err(MultiHopBuildError::TooManyLegs {
            count: hop_count,
            maximum: MAX_LEGS,
        });
    }
    let input_token = route.input_token().address;
    let mut built: Vec<ExecutorLeg> = Vec::with_capacity(hop_count);
    for (index, hop) in quote.hops.iter().enumerate() {
        if index == 0 {
            if hop.token_in.address != input_token {
                return Err(MultiHopBuildError::WrongInputToken {
                    expected: input_token,
                    found: hop.token_in.address,
                });
            }
            if hop.amount_in != candidate.search.best_input {
                return Err(MultiHopBuildError::InputMismatch {
                    expected: candidate.search.best_input,
                    found: hop.amount_in,
                });
            }
        } else {
            let previous = &quote.hops[index - 1];
            if hop.amount_in != previous.amount_out {
                return Err(MultiHopBuildError::AmountChainBroken {
                    index,
                    expected: previous.amount_out,
                    found: hop.amount_in,
                });
            }
        }
        built.push(ExecutorLeg {
            pool: hop.pool.address,
            token_in: hop.token_in.address,
            token_out: hop.token_out.address,
            amount_in: hop.amount_in,
            amount_out: hop.amount_out,
            // M10's rule, not a new one: the contract requires this exact figure.
            min_amount_out: hop.amount_out,
        });
    }
    // `built` has one entry per hop, and the equality check above ran only when the
    // quote covers at least the route's two hops.
    let Some(last) = built.last() else {
        return Err(MultiHopBuildError::TruncatedQuote {
            route: hop_count,
            quoted: 0,
            stopped_at: 0,
        });
    };
    if last.token_out != input_token {
        return Err(MultiHopBuildError::NotACycle {
            expected: input_token,
            found: last.token_out,
        });
    }
    Ok(built)
}

/// Everything about the run except the route and the block: the address space, the
/// label of the state source, the gas model. The chain and the pinned block are
/// not fields here, because the candidate owns them — see [`executor_run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    /// Where the pinned state comes from, spelled the way
    /// [`ExecutorRun::state_source`] wants it. The run compares it against what the
    /// provider actually serves.
    pub state_source: String,
    /// The deployment the route is asked of. §45's "wrong executor" case is this
    /// field pointed at an address with no code, which the run refuses as
    /// `SimulationError::MissingCode` rather than answering with a no-op.
    pub executor: Address,
    /// The account that funded the route and pays for gas (§18's model).
    pub operator: Address,
    pub recipient: Address,
    pub gas_limit: u64,
    pub rules: EvmRules,
    pub pricing: GasPricing,
    /// Native wei given to the operator for gas, or `None` to run at whatever the
    /// pinned block says the account holds.
    pub endowment: Option<U256>,
}

/// The M10 request for this candidate at this block (§19).
///
/// `chain_id` and `priced_at` come out of the candidate and nowhere else: they
/// are what the route *is*, and a caller that could restate them could ask a
/// route on one chain against another chain's state. The only monetary value the
/// caller supplies is the final guard, and it is checked against the quote.
pub fn executor_run(
    candidate: &OptimizedCandidate,
    config: &RunConfig,
    min_final_output: U256,
) -> Result<ExecutorRun, MultiHopBuildError> {
    let built = legs(candidate)?;
    if min_final_output > candidate.search.best_output {
        return Err(MultiHopBuildError::UnreachableGuard {
            min_final_output,
            quoted: candidate.search.best_output,
        });
    }
    Ok(ExecutorRun {
        chain_id: candidate.chain_id,
        priced_at: candidate.target_block,
        state_source: config.state_source.clone(),
        executor: config.executor,
        operator: config.operator,
        call: ExecutorCall::Execute {
            legs: built,
            input_token: candidate.route.input_token().address,
            amount_in: candidate.search.best_input,
            min_final_amount: min_final_output,
            recipient: config.recipient,
        },
        gas_limit: config.gas_limit,
        rules: config.rules,
        pricing: config.pricing.clone(),
        endowment: config.endowment,
    })
}

/// How the EVM's one transaction ended (§25).
///
/// Five states. `Delivered` is the only one that produced an amount to compare
/// with the pricing; the other four are answers about a run that paid nothing,
/// and none of them is expressible as a profit of zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum SimulationStatus {
    /// The call returned successfully and the contract answered with the amount it
    /// delivered.
    Delivered { final_amount: U256 },
    /// The call reverted. `contract_error` is one of the executor's own named
    /// errors when the payload decodes as one, `revert_kind` is the protocol
    /// crate's word for the payload, and `reason` is the revert data's description.
    /// The three stay separate because a revert whose payload is not the contract's
    /// own error is a different fact from the contract rejecting the route.
    Reverted {
        contract_error: Option<String>,
        revert_kind: Option<&'static str>,
        reason: String,
    },
    /// The call spent its whole gas limit inside the executor.
    OutOfGas,
    /// REVM stopped the call for a reason that is neither a revert nor gas.
    Halted { reason: String },
    /// The call reported success and returned no bytes. Not a delivery of zero:
    /// `execute` is declared to answer with what it delivered, so a silent success
    /// says the run did not do what its signature says.
    NoReturn,
}

impl SimulationStatus {
    /// The status as one word, for an evidence row that has to be groupable.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Delivered { .. } => "delivered",
            Self::Reverted { .. } => "reverted",
            Self::OutOfGas => "out_of_gas",
            Self::Halted { .. } => "halted",
            Self::NoReturn => "no_return",
        }
    }

    /// Did the route pay? Only a delivery has an amount to ask about.
    pub fn delivered(&self) -> Option<U256> {
        match self {
            Self::Delivered { final_amount } => Some(*final_amount),
            _ => None,
        }
    }

    fn from_outcome(outcome: &ExecutorOutcome) -> Self {
        match &outcome.status {
            StepStatus::Success => match outcome.delivered {
                Some(final_amount) => Self::Delivered { final_amount },
                None => Self::NoReturn,
            },
            StepStatus::Reverted(data) => Self::Reverted {
                contract_error: outcome.contract_error.clone(),
                revert_kind: outcome.revert_kind,
                reason: data.reason(),
            },
            StepStatus::OutOfGas => Self::OutOfGas,
            StepStatus::Halted(reason) => Self::Halted {
                reason: reason.clone(),
            },
        }
    }
}

/// §24's model: the candidate, the run it was asked for, and what the EVM said.
///
/// The numbers a reader wants are flat and named, and they come from different
/// stages of the frozen chain — that separation is the point (§4: Estimated ≠
/// Simulated):
///
/// ```text
/// candidate.search.best_output   priced by the AMM's integer arithmetic   (Estimated)
/// final_amount                   delivered by the contract               (Simulated)
/// gas_charge                     the EVM's measurement, in wei           (native)
/// ```
///
/// [`SimulatedOpportunity::gross`] is in the input token's units and says nothing
/// about gas. §24's warning is why there is no net-profit field here: gas on this
/// chain is paid in the native token and the route settles in an ERC-20, and
/// subtracting one from the other would be adding numbers in different units. A
/// caller that needs a net number has to price gas in the route's token first, and
/// that is §27's job, not this struct's.
///
/// No `Serialize`: this type owns the EVM's [`ExecutorRun`] and the candidate's own
/// model, neither of which is serializable, and an evidence table that wants a JSON
/// spelling of a simulation asks for [`Self::canonical_text`] — the same text the
/// identity hash is taken over, so a report row and a hash cannot disagree.
#[derive(Clone, Debug)]
pub struct SimulatedOpportunity {
    /// The candidate, unchanged: §17's immutability survives the run.
    pub candidate: OptimizedCandidate,
    /// The request, kept so the calldata a report quotes can be re-encoded from
    /// what was actually asked rather than from what is claimed.
    pub run: ExecutorRun,
    /// The run's own answer, reserves and balances and state diff included.
    pub outcome: ExecutorOutcome,
    pub status: SimulationStatus,
    pub chain_id: ChainId,
    /// §23: the header the state was actually loaded from — number and hash, both
    /// inside [`Self::canonical_text`].
    pub simulation_block: BlockPin,
    pub input_amount: U256,
    /// What the contract delivered, or `None` for the endings that delivered
    /// nothing. Never a zero standing in for a failure.
    pub final_amount: Option<U256>,
    /// `final_amount` against `input_amount`, in the input token's units, and `None`
    /// whenever there is no delivery to compare.
    pub gross: Option<Gross>,
    pub gas_used: u64,
    /// The gas bill in wei, priced by M10's model — kept beside `gross`, never
    /// netted into it (§24).
    pub gas_charge: GasCharge,
    /// The final guard the call carried.
    pub min_final_output: U256,
}

impl SimulatedOpportunity {
    /// Read the outcome back into §24's shape, after checking that the outcome
    /// belongs to the candidate it is being filed under.
    ///
    /// The check is not paranoia about this crate: the three arguments arrive from
    /// different stages of the chain, and §30 makes an execution claim about a
    /// simulation. A run that is not this candidate's run would make that claim
    /// about a route nobody priced, so it is refused here, where the three are
    /// still in reach of each other.
    pub fn new(
        candidate: &OptimizedCandidate,
        run: &ExecutorRun,
        outcome: ExecutorOutcome,
    ) -> Result<Self, MultiHopBuildError> {
        if run.chain_id != candidate.chain_id {
            return Err(MultiHopBuildError::RunChainMismatch {
                expected: candidate.chain_id,
                found: run.chain_id,
            });
        }
        if run.priced_at != candidate.target_block {
            return Err(MultiHopBuildError::BlockMismatch {
                priced: candidate.target_block,
                pinned: run.priced_at,
            });
        }
        Self::call_matches_candidate(candidate, &run.call)?;
        let status = SimulationStatus::from_outcome(&outcome);
        let final_amount = status.delivered();
        let gross = final_amount.map(|delivered| Gross::between(outcome.amount_in, delivered));
        Ok(Self {
            candidate: candidate.clone(),
            run: run.clone(),
            simulation_block: outcome.block,
            chain_id: outcome.chain_id,
            input_amount: outcome.amount_in,
            final_amount,
            gross,
            gas_used: outcome.gas_used,
            gas_charge: outcome.charge.clone(),
            min_final_output: outcome.min_final_amount,
            status,
            outcome,
        })
    }

    /// The legs a candidate's pricing produces, compared position by position with
    /// the legs a call carries.
    ///
    /// Shared by [`Self::new`], which will not file a run that is not its
    /// candidate's run, and [`Self::route_agrees_with_pricing`], which is asked
    /// later, by a layer holding only the finished record. Two implementations of
    /// this question would be two chances to answer it differently.
    fn call_matches_candidate(
        candidate: &OptimizedCandidate,
        call: &ExecutorCall,
    ) -> Result<(), MultiHopBuildError> {
        let expected = legs(candidate)?;
        let found: &[ExecutorLeg] = match call {
            ExecutorCall::Execute { legs, .. } => legs.as_slice(),
            other => {
                return Err(MultiHopBuildError::NotAnExecuteCall {
                    signature: other.signature(),
                })
            }
        };
        if found.len() != expected.len() {
            return Err(MultiHopBuildError::RunLegMismatch {
                index: expected.len(),
            });
        }
        // Position by position, in trade order. Neither list is sorted by anything a
        // reader could reconstruct afterwards, so a comparison by amount or by pool
        // address would accept a route whose legs were reordered — the same route
        // walked in a different order is a different set of transactions.
        if let Some(index) = expected
            .iter()
            .zip(found.iter())
            .position(|(want, got)| want != got)
        {
            return Err(MultiHopBuildError::RunLegMismatch { index });
        }
        Ok(())
    }

    /// §27's `route validity`, asked of a finished record.
    ///
    /// [`Self::new`] already refuses a run whose call is not its candidate's route, so
    /// why ask again: because every field of this struct is public. A record that has
    /// travelled through an evidence table, a deserialised report, or a hand-edited
    /// test can arrive at a risk layer carrying a candidate and a call that stopped
    /// agreeing with each other, and §30 makes an execution claim about a simulation —
    /// a claim that has to survive being re-checked, not one that rests on the
    /// producer having been careful once.
    ///
    /// `Ok(())` says the legs in the call are exactly the legs this candidate's quote
    /// produces — one per hop, chained amount to amount, entered on the route's input
    /// token, closing the circle, within the contract's leg cap — because that is what
    /// [`legs`] derives and nothing else is compared.
    pub fn route_agrees_with_pricing(&self) -> Result<(), MultiHopBuildError> {
        Self::call_matches_candidate(&self.candidate, &self.run.call)
    }

    /// Did the contract deliver what the pricing claimed? `None` when there was no
    /// delivery to compare — the honest answer for a reverted run, rather than a
    /// `Some(false)` that reads as a price disagreement.
    pub fn matches_pricing(&self) -> Option<bool> {
        self.final_amount
            .map(|delivered| delivered == self.candidate.search.best_output)
    }

    /// The priced promise against the delivery, as the three states §4 requires.
    /// Only meaningful on a delivery: the AMM's arithmetic and the contract's
    /// answer disagree by this much, in the input token's units.
    pub fn priced_against_delivered(&self) -> Option<Gross> {
        self.final_amount
            .map(|delivered| Gross::between(delivered, self.candidate.search.best_output))
    }

    /// Did the market move? M10's own answer, forwarded: false on a run that
    /// reverted before anything settled, which is §27's residue check.
    pub fn market_moved(&self) -> bool {
        self.outcome.market_moved()
    }

    pub fn calldata(&self) -> Bytes {
        self.run.call.encode()
    }

    pub fn calldata_hash(&self) -> B256 {
        keccak256(self.calldata().as_ref())
    }

    /// §23/§30: one canonical spelling of the simulation. A hash over this text
    /// changes when the chain, the pinned header, the deployment, a leg's amounts,
    /// the final guard, the gas limit, the outcome's class or the delivered amount
    /// changes — so an execution plan that claims to have been simulated can be
    /// checked against the simulation that produced it, and a stale claim is a
    /// different number rather than a paragraph a reader can reinterpret.
    pub fn canonical_text(&self) -> String {
        let legs = self.leg_lines();
        format!(
            "m11-multi-hop-simulation\nchain_id={}\nblock={}\nblock_hash={}\n\
             executor={:#x}\noperator={:#x}\nrecipient={:#x}\ninput_token={:#x}\n\
             input_amount={}\npriced_input={}\npriced_output={}\n\
             legs={}\nmin_final_output={}\ngas_limit={}\ngas_used={}\n\
             status={}\nfinal_amount={}\ngross={}\ncalldata_hash={}\nroute={}\n{}",
            self.chain_id.0,
            self.simulation_block.number.0,
            self.simulation_block.hash,
            self.run.executor,
            self.run.operator,
            self.outcome.recipient,
            self.outcome.input_token,
            self.input_amount,
            self.candidate.search.best_input,
            self.candidate.search.best_output,
            legs.len(),
            self.min_final_output,
            self.run.gas_limit,
            self.gas_used,
            self.status.name(),
            spell(self.final_amount),
            spell_gross(self.gross),
            self.calldata_hash(),
            self.route_line(),
            legs.join("\n"),
        )
    }

    /// The simulation's identity (§23), as one number.
    pub fn identity_hash(&self) -> B256 {
        keccak256(self.canonical_text().as_bytes())
    }

    /// The identity spelled out, so an evidence table can name a simulation without
    /// hashing it first.
    pub fn identity(&self) -> String {
        format!(
            "m11-sim-{}-{}-{}",
            self.chain_id.0,
            self.simulation_block.number.0,
            self.identity_hash()
        )
    }

    fn leg_lines(&self) -> Vec<String> {
        match &self.run.call {
            ExecutorCall::Execute { legs, .. } => legs
                .iter()
                .enumerate()
                .map(|(index, leg)| {
                    format!(
                        "leg[{index}]|pool={:#x}|token_in={:#x}|token_out={:#x}|amount_in={}|\
                         amount_out={}|min_amount_out={}",
                        leg.pool,
                        leg.token_in,
                        leg.token_out,
                        leg.amount_in,
                        leg.amount_out,
                        leg.min_amount_out
                    )
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The rotation-invariant route identity, spelled from its edges rather than
    /// from a `Debug` implementation, so the number a report quotes does not change
    /// when someone formats a type differently.
    fn route_line(&self) -> String {
        let edges = self.candidate.route.identity().edges();
        let spelled = edges
            .iter()
            .map(|edge| {
                format!(
                    "{:#x}/{:#x}>{:#x}",
                    edge.pool.address, edge.token_in.address, edge.token_out.address
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        format!("{} hops:{spelled}", edges.len())
    }
}

/// An `Option<U256>` spelled the same way everywhere it appears: a missing amount
/// is the word `none`, never the number 0.
fn spell(amount: Option<U256>) -> String {
    match amount {
        Some(value) => value.to_string(),
        None => "none".to_string(),
    }
}

/// [`Gross`] in the same words the pricing module uses, with the magnitude beside
/// the state so a report row can carry both in one field.
fn spell_gross(gross: Option<Gross>) -> String {
    match gross {
        None => "none".to_string(),
        Some(Gross::Gain(amount)) => format!("gain:{amount}"),
        Some(Gross::Even) => "even:0".to_string(),
        Some(Gross::Loss(amount)) => format!("loss:{amount}"),
    }
}
