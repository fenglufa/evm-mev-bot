//! The AMM math, and nothing else.
//!
//! One pure function, exact integer arithmetic, no chain, no store, no graph.
//! That separation is the point: every number a detector reports has to be
//! reproducible from four inputs alone, so this module is where "is the
//! arithmetic right?" can be answered without trusting anything upstream.
//!
//! The model is the V2-style constant-product quote with fee taken on the input
//! side, which is the only model M2 has attested pools for:
//!
//! ```text
//! amount_in_with_fee = amount_in * fee.numerator
//! amount_out = amount_in_with_fee * reserve_out
//!            / (reserve_in * fee.denominator + amount_in_with_fee)
//! ```
//!
//! `fee` is the *retained* fraction from the pool's attestation (`evm_core::Fee`,
//! 997/1000 for a 0.3 % fee), never a constant written down here. Division
//! floors toward zero, which is what the on-chain formula does; the resulting
//! one-unit discrepancy against a closed-form rational is expected and is the
//! reason fee evidence is stated as "reproduces the observed `amountOut`" rather
//! than "equals the observed ratio".

use alloy_primitives::U256;
use evm_core::Fee;

use crate::error::MathError;

/// Quote `amount_in` of the input token through one pool direction.
///
/// Fails on anything that cannot be priced: an empty side, a zero amount, a
/// fee ratio that is not a fraction of the input, or an intermediate product
/// that does not fit in 256 bits. Never panics, never substitutes a guess.
pub fn swap_exact_in(
    reserve_in: U256,
    reserve_out: U256,
    fee: Fee,
    amount_in: U256,
) -> Result<U256, MathError> {
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(MathError::InvalidReserve);
    }
    if amount_in.is_zero() {
        return Err(MathError::InvalidAmount);
    }
    // A retained fraction must be a part of the input: `0 <= numerator <=
    // denominator`, and a denominator of zero is not a ratio at all. Retaining
    // *more* than the whole input would mean the pool pays out above the
    // fee-free curve, which no attested pool does and no result should assume.
    if fee.denominator == 0 || fee.numerator > fee.denominator {
        return Err(MathError::InvalidFee);
    }

    let retained = amount_in
        .checked_mul(U256::from(fee.numerator))
        .ok_or(MathError::Overflow)?;
    let numerator = retained
        .checked_mul(reserve_out)
        .ok_or(MathError::Overflow)?;
    let denominator = reserve_in
        .checked_mul(U256::from(fee.denominator))
        .ok_or(MathError::Overflow)?
        .checked_add(retained)
        .ok_or(MathError::Overflow)?;
    if denominator.is_zero() {
        return Err(MathError::InvalidFee);
    }
    Ok(numerator / denominator)
}

/// `swap_exact_in` composed over two pool directions, feeding hop 1's output
/// straight into hop 2. Returns the amount of the original token that comes
/// back out — never a profit, because a loss underflows and this type has no
/// signed representation for one (see [`crate::path::PathSimulation`]).
///
/// A first hop that floors to zero pays out nothing, and the round trip ends
/// there: the answer is `0`, not an error. That is a real market outcome — an
/// input too small to buy a single whole unit of the middle token — and
/// refusing it would drop the smallest candidates from the enumeration instead
/// of pricing them at their true loss.
pub fn swap_through_two_hops(
    first: (U256, U256, Fee),
    second: (U256, U256, Fee),
    amount_in: U256,
) -> Result<U256, MathError> {
    let between = swap_exact_in(first.0, first.1, first.2, amount_in)?;
    if between.is_zero() {
        return Ok(U256::ZERO);
    }
    swap_exact_in(second.0, second.1, second.2, between)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE_030: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };

    fn u(v: u128) -> U256 {
        U256::from(v)
    }

    /// Hand-checked against the floor of the exact rational:
    /// `100 * 997 * 10000 / (10000 * 1000 + 100 * 997) = 997_000_000 / 10_099_700`.
    #[test]
    fn amount_out_floors_down_to_the_last_whole_unit() {
        assert_eq!(
            swap_exact_in(u(10_000), u(10_000), FEE_030, u(100)),
            Ok(u(98))
        );
        let exact = 997_000_000u128 / 10_099_700;
        assert_eq!(exact, 98);
    }

    #[test]
    fn zero_fee_takes_the_whole_fee_free_curve() {
        // 100 in against 10_000/10_000: the un-fee'd quote is 99 units.
        assert_eq!(
            swap_exact_in(u(10_000), u(10_000), Fee::ZERO, u(100)),
            Ok(u(99))
        );
    }

    #[test]
    fn a_deeper_outside_pays_more_and_price_impact_is_not_linear() {
        let small = swap_exact_in(u(10_000), u(10_000), FEE_030, u(100)).expect("quote");
        let double_input_but_less_than_double_output =
            swap_exact_in(u(10_000), u(10_000), FEE_030, u(200)).expect("quote");
        assert!(double_input_but_less_than_double_output < small * u(2));
    }

    #[test]
    fn an_empty_side_is_no_market() {
        assert_eq!(
            swap_exact_in(u(0), u(10), FEE_030, u(1)),
            Err(MathError::InvalidReserve)
        );
        assert_eq!(
            swap_exact_in(u(10), u(0), FEE_030, u(1)),
            Err(MathError::InvalidReserve)
        );
    }

    #[test]
    fn a_zero_input_buys_nothing_and_is_refused() {
        assert_eq!(
            swap_exact_in(u(10), u(10), FEE_030, U256::ZERO),
            Err(MathError::InvalidAmount)
        );
    }

    #[test]
    fn an_impossible_fee_ratio_is_refused() {
        let zero_denominator = Fee {
            numerator: 997,
            denominator: 0,
        };
        let more_than_whole = Fee {
            numerator: 1001,
            denominator: 1000,
        };
        assert_eq!(
            swap_exact_in(u(10), u(10), zero_denominator, u(1)),
            Err(MathError::InvalidFee)
        );
        assert_eq!(
            swap_exact_in(u(10), u(10), more_than_whole, u(1)),
            Err(MathError::InvalidFee)
        );
    }

    #[test]
    fn huge_reserves_overflow_to_an_error_instead_of_panicking() {
        let max = U256::MAX;
        assert_eq!(
            swap_exact_in(max, max, FEE_030, u(1)),
            Err(MathError::Overflow)
        );
    }

    #[test]
    fn two_hops_chain_the_same_way_by_hand() {
        // 100 -> pool A (10_000/10_000) -> 98 -> pool B (10_000/10_000) -> 96.
        assert_eq!(
            swap_through_two_hops(
                (u(10_000), u(10_000), FEE_030),
                (u(10_000), u(10_000), FEE_030),
                u(100)
            ),
            Ok(u(96))
        );
        assert_eq!(
            swap_exact_in(u(10_000), u(10_000), FEE_030, u(98)),
            Ok(u(96))
        );
    }

    #[test]
    fn an_input_too_small_to_buy_one_unit_comes_back_as_zero_not_an_error() {
        // 1 wei of A against 100_000/50_000 buys floor(997 * 50_000 / 100_000_997)
        // = 0 of B, so nothing reaches the second pool.
        assert_eq!(
            swap_exact_in(u(100_000), u(50_000), FEE_030, u(1)),
            Ok(U256::ZERO)
        );
        assert_eq!(
            swap_through_two_hops(
                (u(100_000), u(50_000), FEE_030),
                (u(50_000), u(125_000), FEE_030),
                u(1)
            ),
            Ok(U256::ZERO)
        );
    }
}
