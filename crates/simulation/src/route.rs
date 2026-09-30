//! The route a simulation is asked to execute, in a form this crate owns.
//!
//! §6 of the task forbids a `simulation -> opportunity` dependency, and this
//! module is where that rule is paid for: M3's [`evm_opportunity::Opportunity`]
//! and the [`PricedRoute`] here carry the same facts, but the second is a list
//! of things to *do* — which pool, in which direction, at what reserves and fee
//! — rather than a priced finding. The pipeline crate converts one into the
//! other; this crate never has to know the first exists.
//!
//! The conversion keeps M3's own words with it. [`PricedRoute::priced_by`]
//! stores the text M3 printed for this finding, both hop reserves, both fees and
//! the whole input search record included. §69 says a difference between
//! analytical and simulated output must be investigated, not averaged away, and
//! an audit cannot investigate a number that arrived without the reasoning that
//! produced it.
//!
//! What this module checks is structural only: that the legs join up into a
//! cycle, that they are different pools, that one chain is involved. It
//! recomputes no quote. Re-deriving M3's arithmetic here would give the
//! simulation a second formula to agree with, which is the one thing §3 of the
//! task rules out.

use alloy_primitives::{Address, U256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};

use crate::error::SimulationError;

/// One leg of the route: a pool, the direction traded through it, and the
/// market state M3 priced it against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RouteLeg {
    pub pool: PoolId,
    pub token_in: TokenId,
    pub token_out: TokenId,
    /// The reserve of the token being handed to this pool, as of the priced
    /// block. Carried so the simulation can prove it executed against the state
    /// the opportunity was found on — a reserve that moved is a different market.
    pub reserve_in: U256,
    pub reserve_out: U256,
    /// The attested fee, in the same `numerator/denominator` form M3 insisted on
    /// (§26 of that task: no fee is ever defaulted).
    pub fee: Fee,
}

impl RouteLeg {
    pub fn address(&self) -> Address {
        self.pool.address
    }
}

/// A two-pool cycle a simulation can be asked to execute.
///
/// Built only through [`PricedRoute::new`], so the four things a plan has to
/// assume are true are checked once, at the boundary: the legs join at a middle
/// token, the route closes on the token it spent, the two legs are different
/// pools, and every identity on it belongs to one chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PricedRoute {
    pub chain_id: ChainId,
    /// The block this was found on. §20 makes this the block the state must come
    /// from, and [`crate::engine`] refuses to run if the pin disagrees.
    pub block_number: BlockNumber,
    pub legs: [RouteLeg; 2],
    /// The token spent at the start and expected back at the end.
    pub input_token: TokenId,
    /// The amount spent. Simulation measures what comes back against this, and
    /// the sender is funded with exactly this much (§58) so the comparison is
    /// apples to apples.
    pub input_amount: U256,
    /// What M3 expected to hold after the first hop, before the second. Not a
    /// bound the simulation enforces — the second transfer moves whatever the
    /// first hop *actually* delivered, which is how a token tax shows up
    /// (§23/§39).
    pub analytical_mid_amount: U256,
    /// What M3 expected back. This is also the default `minimum_amount_out`
    /// (§21): asking a V2 pair for exactly the analytical output is the
    /// zero-slippage form of the route.
    pub analytical_output: U256,
    /// M3's profit number, kept verbatim and never adjusted here (§4).
    pub analytical_gross_profit: U256,
    /// The finding this came from, in M3's own words.
    pub priced_by: String,
}

impl PricedRoute {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_id: ChainId,
        block_number: BlockNumber,
        first: RouteLeg,
        second: RouteLeg,
        input_amount: U256,
        analytical_mid_amount: U256,
        analytical_output: U256,
        analytical_gross_profit: U256,
        priced_by: String,
    ) -> Result<Self, SimulationError> {
        let unsupported = |reason: String| SimulationError::UnsupportedTransaction(reason);
        for leg in [&first, &second] {
            if leg.pool.chain_id != chain_id
                || leg.token_in.chain_id != chain_id
                || leg.token_out.chain_id != chain_id
            {
                return Err(unsupported(format!(
                    "pool {} and its tokens must live on chain {}",
                    leg.pool.address, chain_id.0
                )));
            }
            if leg.token_in == leg.token_out {
                return Err(unsupported(format!(
                    "pool {} would trade a token for itself",
                    leg.pool.address
                )));
            }
            if leg.reserve_in.is_zero() || leg.reserve_out.is_zero() {
                return Err(unsupported(format!(
                    "pool {} was priced against a zero reserve",
                    leg.pool.address
                )));
            }
            if leg.fee.numerator > leg.fee.denominator || leg.fee.denominator == 0 {
                return Err(unsupported(format!(
                    "pool {} carries fee {}/{}, which is not a fraction of the input",
                    leg.pool.address, leg.fee.numerator, leg.fee.denominator
                )));
            }
        }
        if first.pool == second.pool {
            return Err(unsupported(format!(
                "both legs are pool {}, and a round trip through one pool is a fee donation",
                first.pool.address
            )));
        }
        if first.token_out != second.token_in {
            return Err(unsupported(format!(
                "the first leg ends in {} but the second starts from {}",
                first.token_out.address, second.token_in.address
            )));
        }
        if second.token_out != first.token_in {
            return Err(unsupported(format!(
                "the route does not close: it spends {} and ends holding {}",
                first.token_in.address, second.token_out.address
            )));
        }
        if input_amount.is_zero() {
            return Err(unsupported("a zero input is not a trade".to_string()));
        }
        Ok(Self {
            chain_id,
            block_number,
            legs: [first, second],
            input_token: first.token_in,
            input_amount,
            analytical_mid_amount,
            analytical_output,
            analytical_gross_profit,
            priced_by,
        })
    }

    pub fn first(&self) -> &RouteLeg {
        &self.legs[0]
    }

    pub fn second(&self) -> &RouteLeg {
        &self.legs[1]
    }

    /// The token the route passes through between the pools.
    pub fn mid_token(&self) -> TokenId {
        self.legs[0].token_out
    }

    pub fn pools(&self) -> [PoolId; 2] {
        [self.legs[0].pool, self.legs[1].pool]
    }

    /// Every contract address a plan touches: both pools and both tokens. Used
    /// by the preflight's `eth_getCode` gate (§60).
    pub fn touched_contracts(&self) -> Vec<Address> {
        vec![
            self.legs[0].pool.address,
            self.legs[1].pool.address,
            self.input_token.address,
            self.mid_token().address,
        ]
    }

    /// `expected_output / slippage_policy / minimum_output`, as §22 requires.
    /// The minimum is what the plan will actually ask the second pool for.
    pub fn slippage_record(&self, minimum_output: U256) -> SlippageRecord {
        SlippageRecord {
            expected_output: self.analytical_output,
            policy: SlippagePolicy::of(self.analytical_output, minimum_output),
            minimum_output,
        }
    }
}

/// How much slack the plan leaves between what M3 expected and what it demands.
///
/// This is a *simulation* slippage bound, not a production one: it is the number
/// the second pair is asked to pay, and asking for less is what makes a route
/// whose middle token is taxed executable at all. Deciding it is not the same as
/// modelling the tax — the bound is a request, and only execution says whether
/// the request is met.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SlippagePolicy {
    /// Ask for exactly what M3 expected. Any divergence in the token's own
    /// transfer behaviour shows up as a revert instead of as a fudge factor.
    Exact,
    /// Ask for `basis_points` of the expected output, out of 10_000.
    BasisPoints { basis_points: u16 },
    /// Ask for an amount that is not a fraction of anything: the plan is
    /// bracketing a ceiling by trial, so the number is absolute.
    Requested { amount: U256 },
}

impl SlippagePolicy {
    /// The policy that turns `expected` into `requested`.
    ///
    /// Returns `BasisPoints` only when the request is genuinely a discount —
    /// below `expected`, and exactly representable at whole basis points. A
    /// request that came from a ladder step, and one that asks for *more* than
    /// was expected, are both reported as `Requested`, because describing them as
    /// a rounded percentage would claim a precision or a policy the number does
    /// not have.
    pub fn of(expected: U256, requested: U256) -> Self {
        if expected == requested {
            return Self::Exact;
        }
        if expected.is_zero() || requested > expected {
            return Self::Requested { amount: requested };
        }
        let Some(scaled) = requested.checked_mul(U256::from(10_000u32)) else {
            return Self::Requested { amount: requested };
        };
        let quotient = scaled / expected;
        let remainder = scaled % expected;
        let fits = remainder.is_zero() && quotient < U256::from(10_000u32);
        if fits {
            Self::BasisPoints {
                // `fits` bounds the quotient below 10_000.
                basis_points: quotient.to::<u32>() as u16,
            }
        } else {
            Self::Requested { amount: requested }
        }
    }

    /// Apply the policy to an expected amount. Never rounds up and never asks
    /// for more than was expected: `BasisPoints` above 10_000 would be a demand
    /// for a profit that was never priced, so it is clamped rather than honored.
    pub fn apply(&self, expected: U256) -> U256 {
        match self {
            Self::Exact => expected,
            Self::BasisPoints { basis_points } => {
                // `floor(expected * bp / 10_000)` without an intermediate that can
                // overflow: the head carries the whole thousands and the tail is
                // under `10_000 * bp`, so the division stays exact.
                let points = U256::from(*basis_points as u32);
                let ten_thousand = U256::from(10_000u32);
                let head = (expected / ten_thousand).saturating_mul(points);
                let tail = (expected % ten_thousand).saturating_mul(points) / ten_thousand;
                head.saturating_add(tail).min(expected)
            }
            Self::Requested { amount } => *amount,
        }
    }
}

/// The three numbers §22 asks to see recorded together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SlippageRecord {
    pub expected_output: U256,
    pub policy: SlippagePolicy,
    pub minimum_output: U256,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    const CHAIN: ChainId = ChainId(91342);
    const OTHER: ChainId = ChainId(1);
    const WETH: Address = address!("0x4200000000000000000000000000000000000006");
    const TTAX: Address = address!("0xcffe7472a7a1a6947f56233854ae91a54c862f62");
    const POOL_A: Address = address!("0xf487d533cae6cddd0c7e7bbbac084dd04d876578");
    const POOL_B: Address = address!("0x5bef6275607901dcd58160356660151be0637440");

    const FEE: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };

    fn leg(pool: Address, from: Address, to: Address, r_in: u128, r_out: u128) -> RouteLeg {
        RouteLeg {
            pool: PoolId::new(CHAIN, pool),
            token_in: TokenId::new(CHAIN, from),
            token_out: TokenId::new(CHAIN, to),
            reserve_in: U256::from(r_in),
            reserve_out: U256::from(r_out),
            fee: FEE,
        }
    }

    fn weth_in_ttax() -> RouteLeg {
        leg(
            POOL_A,
            WETH,
            TTAX,
            35_099_900_253_008,
            45_655_538_604_883_371_699,
        )
    }

    fn ttax_in_weth() -> RouteLeg {
        leg(
            POOL_B,
            TTAX,
            WETH,
            43_677_608_078_641_141_054,
            36_641_298_079_327,
        )
    }

    fn route() -> PricedRoute {
        PricedRoute::new(
            CHAIN,
            BlockNumber(37_191_169),
            weth_in_ttax(),
            ttax_in_weth(),
            U256::from(714_844_720_992u128),
            U256::from(910_000_000_000u128),
            U256::from(744_486_240_802u128),
            U256::from(29_641_519_810u128),
            "opportunity on chain 91342 at block 37191169".to_string(),
        )
        .expect("a valid route")
    }

    #[test]
    fn a_closed_two_pool_cycle_is_accepted() {
        let route = route();
        assert_eq!(route.input_token, TokenId::new(CHAIN, WETH));
        assert_eq!(route.mid_token(), TokenId::new(CHAIN, TTAX));
        assert_eq!(
            route.pools(),
            [PoolId::new(CHAIN, POOL_A), PoolId::new(CHAIN, POOL_B)]
        );
        assert_eq!(route.touched_contracts(), vec![POOL_A, POOL_B, WETH, TTAX]);
    }

    /// Every refusal names what was wrong. A route that arrives malformed is a
    /// pipeline bug, and the message has to be enough to find it.
    #[test]
    fn a_route_that_is_not_a_cycle_is_refused_with_a_reason() {
        let same_pool = PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            weth_in_ttax(),
            leg(POOL_A, TTAX, WETH, 10, 10),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .expect_err("one pool twice");
        assert!(matches!(
            same_pool,
            SimulationError::UnsupportedTransaction(_)
        ));
        assert!(same_pool.to_string().contains("fee donation"));

        let open_route = PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            weth_in_ttax(),
            leg(POOL_B, TTAX, POOL_A, 10, 10),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .expect_err("does not close");
        assert!(open_route.to_string().contains("does not close"));

        let broken_join = PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            weth_in_ttax(),
            leg(POOL_B, POOL_A, WETH, 10, 10),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .expect_err("hops do not join");
        assert!(broken_join
            .to_string()
            .contains("but the second starts from"));
    }

    /// Identities are chain-scoped (§63 of the earlier milestones): a route that
    /// mixes chains is not a route, and a zero reserve or a fee above par is not
    /// a market either.
    #[test]
    fn mixed_chain_and_degenerate_markets_are_refused() {
        let mut foreign = weth_in_ttax();
        foreign.pool = PoolId::new(OTHER, POOL_A);
        assert!(PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            foreign,
            ttax_in_weth(),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .is_err());

        let mut drained = weth_in_ttax();
        drained.reserve_in = U256::ZERO;
        assert!(PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            drained,
            ttax_in_weth(),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .is_err());

        let mut fee_above_par = weth_in_ttax();
        fee_above_par.fee = Fee {
            numerator: 1001,
            denominator: 1000,
        };
        assert!(PricedRoute::new(
            CHAIN,
            BlockNumber(1),
            fee_above_par,
            ttax_in_weth(),
            U256::from(1u8),
            U256::from(1u8),
            U256::from(2u8),
            U256::from(1u8),
            String::new(),
        )
        .is_err());
    }

    #[test]
    fn slippage_policies_round_trip_through_the_three_numbers() {
        let route = route();
        let expected = route.analytical_output;

        let exact = route.slippage_record(expected);
        assert_eq!(exact.policy, SlippagePolicy::Exact);
        assert_eq!(exact.minimum_output, expected);
        assert_eq!(exact.expected_output, expected);

        let halved = expected / U256::from(2u8);
        let record = route.slippage_record(halved);
        assert_eq!(
            record.policy,
            SlippagePolicy::BasisPoints { basis_points: 5000 }
        );
        assert_eq!(record.minimum_output, halved);

        // A ladder step is an absolute ask, and rounding it to basis points
        // would report a precision it does not have.
        let awkward = expected - U256::from(1u8);
        assert_eq!(
            route.slippage_record(awkward).policy,
            SlippagePolicy::Requested { amount: awkward }
        );
    }

    #[test]
    fn applying_a_policy_never_asks_for_more_than_was_expected() {
        let expected = U256::from(1_000_000u32);
        assert_eq!(SlippagePolicy::Exact.apply(expected), expected);
        assert_eq!(
            SlippagePolicy::BasisPoints { basis_points: 9215 }.apply(expected),
            U256::from(921_500u32)
        );
        assert_eq!(
            SlippagePolicy::BasisPoints {
                basis_points: 10_000
            }
            .apply(expected),
            expected
        );
        assert_eq!(
            SlippagePolicy::Requested {
                amount: U256::from(5u8)
            }
            .apply(expected),
            U256::from(5u8)
        );
        // Zero is a legal ask — it is the "take whatever the pool will send"
        // end of the ladder, and it must not underflow to a huge number.
        assert!(SlippagePolicy::BasisPoints { basis_points: 0 }
            .apply(expected)
            .is_zero());
    }

    /// The route serializes, so a plan can be committed next to its evidence.
    #[test]
    fn route_serializes_to_json() {
        let route = route();
        let text = serde_json::to_string(&route).expect("serializes");
        assert!(text.contains("91342"), "{text}");
        assert!(text.contains("priced_by"), "{text}");
        // `Address` serializes as lowercase hex, which is what makes an evidence
        // file diffable.
        assert!(
            text.contains("\"address\":\"0xf487d533cae6cddd0c7e7bbbac084dd04d876578\""),
            "{text}"
        );
        let _: serde_json::Value = serde_json::from_str(&text).expect("is json");
    }

    /// An ask above the analytical output is not a discount, and a percentage is
    /// the wrong way to describe it — the absolute number is kept. Same for a
    /// `BasisPoints` value past 10_000 handed in by hand: the plan must not
    /// demand output that was never priced.
    #[test]
    fn an_ask_above_the_expected_output_stays_an_absolute_number() {
        let expected = U256::from(1_000_000u32);
        let over = expected + U256::from(1u8);
        assert_eq!(
            SlippagePolicy::of(expected, over),
            SlippagePolicy::Requested { amount: over }
        );
        assert_eq!(
            SlippagePolicy::BasisPoints {
                basis_points: 20_000
            }
            .apply(expected),
            expected
        );
    }

    /// The split multiplication has to stay exact at the top of the uint range:
    /// a saturating product there would silently turn a half-output ask into a
    /// full-output one.
    #[test]
    fn applying_basis_points_is_exact_at_the_top_of_the_uint_range() {
        let huge = U256::MAX / U256::from(3u8);
        let half = SlippagePolicy::BasisPoints { basis_points: 5000 }.apply(huge);
        assert_eq!(half, huge / U256::from(2u8));
        let odd = huge - U256::from(1u8);
        assert_eq!(
            SlippagePolicy::BasisPoints { basis_points: 9999 }.apply(odd),
            odd - odd / U256::from(10_000u32) - U256::from(1u8)
        );
    }
}
