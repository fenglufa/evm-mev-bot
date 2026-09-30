//! Gas: what execution cost, in the chain's own money, and where that price came
//! from.
//!
//! §30 asks for `gas_used × effective_gas_price` in exact integers and forbids
//! `f64` as the final cost. That is the easy half. The half that decides whether
//! the number means anything is §29: a gas price is a property of the chain's
//! transaction model, and a simulation that invents one is asserting a market
//! outcome. So the price is never a constant here — [`GasPricing`] is declared by
//! whoever builds the request, with a provenance string, and resolving it against
//! the pinned block's header either produces a number an auditor can re-derive or
//! refuses.
//!
//! ```text
//! Eip1559     ->  effective price = block base fee + declared tip
//! Legacy      ->  effective price = the declared gas price, provided the block
//!                 has no base fee it would fail to beat
//! Unresolved  ->  no price asserted; gas_used is still a measurement
//! ```
//!
//! §44 rules out the bidding question entirely — no dynamic gas pricing, no
//! priority-fee optimization, no bribe or bundle pricing. This module is therefore
//! deliberately unable to answer "what would it cost to get in front of this
//! block". It answers "what would this sequence have cost at this price", which is
//! a statement about execution, and the price arrives as an input with a citation.
//!
//! The distinction between refusing and abstaining is the point of [`GasCharge`]:
//! a declared model that contradicts the header is a bad request and the engine
//! will not run it at all, while a chain whose pricing semantics have not been
//! established runs and reports `gas_used` with no cost attached — which is what
//! makes `net_profit` `NotComputable` for a reason (§31) instead of a silent zero.

use alloy_primitives::U256;
use serde::Serialize;

use evm_chain::BlockContext;

use crate::error::SimulationError;

/// How a hypothetical transaction pays for gas, declared with its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum GasPricing {
    /// An EIP-1559 transaction model: the sender accepts the block's base fee and
    /// adds this tip. `provenance` says where the tip came from — a value read off
    /// a real transaction in the same block, or a stated test choice.
    Eip1559 {
        priority_fee_per_gas: u128,
        provenance: String,
    },
    /// A legacy transaction model: the gas price is the whole effective price.
    Legacy { gas_price: u128, provenance: String },
    /// The chain's gas pricing has not been established, and §29 forbids guessing
    /// a model. The run measures gas; the cost stays uncomputed.
    Unresolved { reason: String },
}

impl GasPricing {
    pub const fn model(&self) -> &'static str {
        match self {
            Self::Eip1559 { .. } => "eip1559",
            Self::Legacy { .. } => "legacy",
            Self::Unresolved { .. } => "unresolved",
        }
    }

    pub fn provenance(&self) -> &str {
        match self {
            Self::Eip1559 { provenance, .. }
            | Self::Legacy { provenance, .. }
            | Self::Unresolved { reason: provenance } => provenance,
        }
    }

    /// Resolve the model against the pinned header: `Ok(Some(price))` when a price
    /// is asserted and coherent, `Ok(None)` for an honest abstention, `Err` when
    /// the assertion contradicts the block it claims to run on.
    ///
    /// This is the ceiling the sender declares it will pay — for an EIP-1559 model
    /// the base fee plus the tip — which is also the number the run has to be
    /// funded against before anything has been measured.
    pub fn max_fee_per_gas(&self, block: &BlockContext) -> Result<Option<u128>, SimulationError> {
        match self {
            Self::Unresolved { .. } => Ok(None),
            Self::Eip1559 {
                priority_fee_per_gas,
                ..
            } => {
                let Some(base_fee) = block.base_fee_per_gas else {
                    return Err(SimulationError::UnsupportedTransaction(format!(
                        "block {} carries no base fee, so an eip1559 price model cannot be \
                         applied to it; declare the chain's legacy model instead",
                        block.number.0
                    )));
                };
                base_fee
                    .checked_add(*priority_fee_per_gas)
                    .map(Some)
                    .ok_or_else(|| {
                        SimulationError::UnsupportedTransaction(format!(
                            "base fee {base_fee} plus tip {priority_fee_per_gas} overflows u128"
                        ))
                    })
            }
            Self::Legacy { gas_price, .. } => {
                if let Some(base_fee) = block.base_fee_per_gas {
                    if *gas_price < base_fee {
                        return Err(SimulationError::UnsupportedTransaction(format!(
                            "gas price {gas_price} is below block {}'s base fee {base_fee}: this \
                             transaction would not be included, so a cost computed from it would \
                             describe a run that cannot happen",
                            block.number.0
                        )));
                    }
                }
                Ok(Some(*gas_price))
            }
        }
    }
}

/// The cost of a run, or the reason it has no cost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum GasCharge {
    /// `gas_used × effective_gas_price`, in wei, with the header facts the price
    /// was derived from named so the number can be re-derived by an auditor.
    Priced {
        gas_used: u64,
        effective_gas_price: u128,
        base_fee_per_gas: Option<u128>,
        wei: U256,
        pricing: GasPricing,
    },
    /// The EVM measured the gas and no price could be attached to it. Anything
    /// that needs a cost in wei reports `NotComputable` from here (§31).
    Unpriced { gas_used: u64, reason: String },
}

impl GasCharge {
    /// Turn a gas measurement into wei using the declared model and the pinned
    /// header. A measurement is never thrown away because a price is missing —
    /// and a price is never invented because a measurement is there.
    pub fn charge(
        pricing: &GasPricing,
        block: &BlockContext,
        gas_used: u64,
    ) -> Result<Self, SimulationError> {
        match pricing.max_fee_per_gas(block)? {
            Some(effective_gas_price) => {
                // Exact by construction rather than by hope: a u64 times a u128
                // cannot exceed 2^192, so the product always fits the U256 §30 asks for.
                let wei = U256::from(gas_used) * U256::from(effective_gas_price);
                Ok(Self::Priced {
                    gas_used,
                    effective_gas_price,
                    base_fee_per_gas: block.base_fee_per_gas,
                    wei,
                    pricing: pricing.clone(),
                })
            }
            None => Ok(Self::Unpriced {
                gas_used,
                reason: pricing.provenance().to_string(),
            }),
        }
    }

    /// The wei figure, when there is one. `None` is §31's signal: gross profit may
    /// still be reportable, net profit is not.
    pub fn wei(&self) -> Option<U256> {
        match self {
            Self::Priced { wei, .. } => Some(*wei),
            Self::Unpriced { .. } => None,
        }
    }

    pub const fn gas_used(&self) -> u64 {
        match self {
            Self::Priced { gas_used, .. } | Self::Unpriced { gas_used, .. } => *gas_used,
        }
    }

    pub const fn effective_gas_price(&self) -> Option<u128> {
        match self {
            Self::Priced {
                effective_gas_price,
                ..
            } => Some(*effective_gas_price),
            Self::Unpriced { .. } => None,
        }
    }

    pub const fn is_priced(&self) -> bool {
        matches!(self, Self::Priced { .. })
    }

    /// The base fee of the block the price was resolved against, kept separate
    /// because §29 asks the report to say which part of the effective price was
    /// protocol fee and which was the sender's tip.
    pub const fn base_fee_of_block(&self) -> Option<u128> {
        match self {
            Self::Priced {
                base_fee_per_gas, ..
            } => *base_fee_per_gas,
            Self::Unpriced { .. } => None,
        }
    }

    pub fn tip_of_sender(&self) -> Option<u128> {
        match self {
            Self::Priced {
                effective_gas_price,
                base_fee_per_gas: Some(base_fee),
                ..
            } => effective_gas_price.checked_sub(*base_fee),
            Self::Priced { .. } | Self::Unpriced { .. } => None,
        }
    }

    /// Why there is no price, or a statement of which model produced the one there
    /// is. Reported alongside the number in every output, because §29 makes the
    /// provenance part of the result.
    pub fn pricing_note(&self) -> String {
        match self {
            Self::Priced {
                pricing,
                effective_gas_price,
                ..
            } => format!(
                "{} effective gas price {}, provenance: {}",
                pricing.model(),
                effective_gas_price,
                pricing.provenance()
            ),
            Self::Unpriced { reason, .. } => reason.clone(),
        }
    }

    /// The abstention's reason, empty when a price exists.
    pub fn reason(&self) -> &str {
        match self {
            Self::Unpriced { reason, .. } => reason,
            Self::Priced { .. } => "",
        }
    }
}

/// `gas_limit` versus `gas_used`, kept apart (§28).
///
/// A sequence spends gas step by step, and each step is a transaction with its own
/// limit. The run's total is what those steps actually consumed, never what they
/// were allowed to consume — and an individual step's shortfall is what makes an
/// out-of-gas stop a fact rather than an inference (§41).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GasBudget {
    pub gas_limit_per_step: u64,
    pub planned_steps: usize,
    pub executed_steps: usize,
    pub gas_used: u64,
    /// Where the sequence stopped, when it stopped before the plan's last step.
    pub stopped_at: Option<usize>,
}

impl GasBudget {
    pub const fn new(gas_limit_per_step: u64, planned_steps: usize) -> Self {
        Self {
            gas_limit_per_step,
            planned_steps,
            executed_steps: 0,
            gas_used: 0,
            stopped_at: None,
        }
    }

    /// Record one executed step. A step that ran out of gas still consumed gas, so
    /// it is recorded too — with its limit as the used amount, which is what the
    /// EVM reports for an out-of-gas transaction.
    pub fn record(&mut self, gas_used: u64) {
        self.executed_steps += 1;
        self.gas_used = self.gas_used.saturating_add(gas_used);
    }

    pub fn stop_at(&mut self, index: usize) {
        self.stopped_at = Some(index);
    }

    /// The allowance the whole plan was given, as a floor for comparison — never as
    /// a substitute for `gas_used`.
    pub const fn limit_total(&self) -> u64 {
        self.gas_limit_per_step
            .saturating_mul(self.planned_steps as u64)
    }

    pub fn charged(
        &self,
        pricing: &GasPricing,
        block: &BlockContext,
    ) -> Result<GasCharge, SimulationError> {
        GasCharge::charge(pricing, block, self.gas_used)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256};

    use super::*;
    use evm_core::{BlockNumber, ChainId};

    const CHAIN: ChainId = ChainId(91342);

    /// A stand-in header, not a recorded one: the gas logic reads the base fee and
    /// the height, and nothing here claims these numbers came off a chain.
    fn block(base_fee: Option<u128>) -> BlockContext {
        BlockContext {
            chain_id: CHAIN,
            number: BlockNumber(37_191_169),
            hash: b256!("0x1111111111111111111111111111111111111111111111111111111111111111"),
            timestamp: 1_758_000_000,
            gas_limit: 60_000_000,
            base_fee_per_gas: base_fee,
            excess_blob_gas: None,
            beneficiary: address!("0x0000000000000000000000000000000000000000"),
            prevrandao: None,
        }
    }

    fn eip1559(tip: u128) -> GasPricing {
        GasPricing::Eip1559 {
            priority_fee_per_gas: tip,
            provenance: "declared by a test".to_string(),
        }
    }

    fn legacy(price: u128) -> GasPricing {
        GasPricing::Legacy {
            gas_price: price,
            provenance: "declared by a test".to_string(),
        }
    }

    /// A 1559 price is the block's base fee plus the declared tip. The base fee is
    /// a fact about the block, so it is read from the header rather than assumed:
    /// 1_000_000 + 500_000 = 1_500_000.
    #[test]
    fn eip1559_prices_are_base_fee_plus_tip() {
        let priced = GasCharge::charge(&eip1559(500_000), &block(Some(1_000_000)), 21_000)
            .expect("a coherent model");
        assert_eq!(
            priced,
            GasCharge::Priced {
                gas_used: 21_000,
                effective_gas_price: 1_500_000,
                base_fee_per_gas: Some(1_000_000),
                wei: U256::from(31_500_000_000u128),
                pricing: eip1559(500_000),
            }
        );
        assert_eq!(priced.effective_gas_price(), Some(1_500_000));
        assert_eq!(priced.gas_used(), 21_000);
        assert_eq!(priced.base_fee_of_block(), Some(1_000_000));
        assert_eq!(priced.tip_of_sender(), Some(500_000));
        assert!(priced.is_priced());

        let zero_tip = GasCharge::charge(&eip1559(0), &block(Some(1_000_000)), 1)
            .expect("a tip of zero is still a coherent model");
        assert_eq!(zero_tip.wei(), Some(U256::from(1_000_000u32)));
    }

    /// A 1559 model on a block with no base fee is a contradictory request, and the
    /// engine refuses to run it rather than inventing the missing half of the price.
    #[test]
    fn eip1559_on_a_block_without_a_base_fee_is_refused() {
        let err = GasCharge::charge(&eip1559(1), &block(None), 21_000)
            .expect_err("no base fee to add to");
        assert!(matches!(err, SimulationError::UnsupportedTransaction(_)));
        assert!(err.to_string().contains("no base fee"), "{err}");
    }

    /// A legacy price below the base fee is a transaction that does not get
    /// included, so its "cost" would describe a run that cannot happen. At the
    /// floor it is coherent, and on a chain with no base fee the gas price is
    /// genuinely the whole effective price.
    #[test]
    fn legacy_is_refused_below_the_base_fee_and_accepted_otherwhere() {
        let err = GasCharge::charge(&legacy(1_999), &block(Some(2_000)), 1)
            .expect_err("below the base fee");
        assert!(err.to_string().contains("would not be included"), "{err}");

        let at_floor = GasCharge::charge(&legacy(2_000), &block(Some(2_000)), 1).expect("at floor");
        assert_eq!(at_floor.effective_gas_price(), Some(2_000));
        assert_eq!(at_floor.base_fee_of_block(), Some(2_000));

        let legacy_chain = GasCharge::charge(&legacy(7), &block(None), 3).expect("legacy chain");
        assert_eq!(legacy_chain.wei(), Some(U256::from(21u32)));
    }

    /// Declaring no model is not an error: the run still measures gas, and the cost
    /// is simply absent — which is what §31 wants a reason for rather than a zero.
    #[test]
    fn an_unresolved_model_charges_nothing_and_says_why() {
        let unresolved = GasPricing::Unresolved {
            reason: "chain 91342 gas pricing not yet established".to_string(),
        };
        let charge = GasCharge::charge(&unresolved, &block(Some(1_000_000)), 90_000)
            .expect("abstaining is not a refusal");
        assert_eq!(
            charge,
            GasCharge::Unpriced {
                gas_used: 90_000,
                reason: "chain 91342 gas pricing not yet established".to_string(),
            }
        );
        assert_eq!(charge.wei(), None);
        assert_eq!(charge.gas_used(), 90_000);
        assert_eq!(unresolved.model(), "unresolved");
    }

    /// The budget separates what was allowed from what was used (§28): seven steps
    /// at a 500_000 limit were allowed 3_500_000, and the run's number is whatever
    /// the steps actually consumed — including the one that ran out.
    #[test]
    fn a_budget_adds_used_gas_without_touching_the_limit() {
        let mut budget = GasBudget::new(500_000, 7);
        assert_eq!(budget.limit_total(), 3_500_000);
        assert_eq!(
            budget.gas_used, 0,
            "a plan's allowance is not a measurement"
        );
        assert_eq!(budget.executed_steps, 0);

        for step in 0..7 {
            budget.record(if step == 2 { 500_000 } else { 100_000 });
        }
        assert_eq!(budget.gas_used, 1_100_000);
        assert_eq!(budget.executed_steps, 7);
        assert!(budget.gas_used < budget.limit_total());

        let mut out_of_gas = GasBudget::new(30_000, 3);
        out_of_gas.record(21_000);
        out_of_gas.record(30_000);
        out_of_gas.stop_at(1);
        assert_eq!(out_of_gas.stopped_at, Some(1));
        assert_eq!(out_of_gas.gas_used, 51_000);
        assert_eq!(out_of_gas.executed_steps, 2);
    }

    #[test]
    fn a_budget_charges_itself_against_a_header() {
        let mut budget = GasBudget::new(500_000, 2);
        budget.record(60_000);
        budget.record(60_000);
        let charge = budget
            .charged(&legacy(2_000), &block(None))
            .expect("legacy price on a block without a base fee");
        assert_eq!(charge.wei(), Some(U256::from(240_000_000u128)));
        assert!(budget.charged(&eip1559(1), &block(None)).is_err());
    }
}
