//! Finding the input amount that maximises a cycle's gross profit.
//!
//! `profit(input)` is not linear: a bigger input moves the first pool's price
//! against you and the second pool's price in your favour, so past some point
//! each extra unit earns less than it costs. Testing one token unit therefore
//! proves nothing about the peak (§20 of the task), which is why this module
//! exists separately from the math.
//!
//! # The domain, and why it is finite
//!
//! The search never runs to `U256::MAX`. For any input, the amount handed back
//! by the second hop is strictly less than that pool's reserve of the input
//! token:
//!
//! ```text
//! out2 = retained * reserve_out2 / (reserve_in2 * fee_denominator + retained)  <  reserve_out2
//! ```
//!
//! so an input of `reserve_out2` or more is guaranteed to lose: profit `< 0`
//! once `input >= reserve_out2`. The argmax therefore lives in
//!
//! ```text
//! 1 ..= second.reserve_out - 1
//! ```
//!
//! That is a derivation, not a tunable ratio, so it needs no configuration. It
//! is also loose — the peak sits well inside it — which is what the search below
//! is for.
//!
//! # The search
//!
//! The unfloored composition of two constant-product quotes is a single
//! fractional-linear term, `out2 = a*x / (b*x + c)`, whose derivative
//! `a*c/(b*x+c)^2 - 1` is strictly decreasing: one peak, no valleys. A discrete
//! ternary search walks to that peak in a few hundred U256 multiplications
//! instead of scanning the domain.
//!
//! The floors are the one thing that proof does not cover, and they are bounded:
//! hop 1 can drop a fraction of a unit of the middle token, and the second
//! quote's slope at that point is at most `reserve_out / reserve_in`, so the
//! floored round trip sits within `1 + second.reserve_out / second.reserve_in`
//! units of the exact rational at the same input. On a flat top that is enough
//! for the integer curve to step, and the steps do not join up into one
//! plateau: over the whole domain of the {A,C} cycle in `tests/fixtures.rs`, the
//! `C -> A -> C` direction tops out at 2 815 on 120 inputs with 2 814 on the 212
//! that fill the gaps between them, and the `A -> C -> A` direction's top of
//! 8 441 is held by 82 inputs spread across `20 372 ..= 20 815`. A search that
//! only ever compares two probes can stop on the wrong step of a staircase like
//! that.
//!
//! Two things follow. Ties close the interval from both ends instead of cutting
//! a plateau in half, and [`SearchRecord`] reports the domain searched and the
//! work done rather than claiming the integer argmax — which is what §23 of the
//! task asks for: an opportunity is "the best input under this policy", not "the
//! global optimum". The exhaustive cross-checks in `tests/fixtures.rs` and in
//! `tests/optimizer_cross_check.rs` compare the search against every input of
//! every domain small enough to enumerate, so the gap between the two, when
//! there is one, is a measured number and not a rumour: over the 2 401 reserve
//! quadruples that sweep covers, the search ties the exhaustive maximum on
//! 2 352, falls short on 49 — the worst by 14 units on a route whose floors
//! allow 31 — and on the one route where the scan profits it reports no profit
//! at all, on a 1-unit peak.
//!
//! What the search does *not* claim is a proof of the global optimum over every
//! integer: if the round budget runs out before the window closes,
//! [`SearchRecord::interval_closed`] says so instead of the result pretending.

use std::cmp::Ordering;

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{Fee, PoolId};
use evm_graph::GraphEdge;

use crate::error::{MathError, OpportunityError, Result};
use crate::math::swap_through_two_hops;
use crate::path::{ArbitragePath, PathSimulation};

/// One pool direction, reduced to what the math needs: reserves in trade
/// direction plus the pool's attested fee.
///
/// Built from a graph edge only. A pool with no attested fee is refused here,
/// never priced with a guessed one: `None` means nobody proved it, and treating
/// it as 0.3 % would invent a market.
///
/// This is also what an [`crate::Opportunity`] carries with its profit, so an
/// audit of a finding can see the two reserves and the two fees the number came
/// from without re-reading the graph (§55 of the task).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PricedHop {
    pub pool: PoolId,
    pub reserve_in: U256,
    pub reserve_out: U256,
    pub fee: Fee,
}

impl PricedHop {
    pub fn from_edge(edge: &GraphEdge) -> Result<Self> {
        if edge.reserve_in.is_zero() || edge.reserve_out.is_zero() {
            return Err(OpportunityError::Math(MathError::InvalidReserve));
        }
        let fee = edge.fee.ok_or(OpportunityError::UnattestedFee(
            edge.pool(),
            edge.token_in(),
        ))?;
        Ok(Self {
            pool: edge.pool(),
            reserve_in: edge.reserve_in,
            reserve_out: edge.reserve_out,
            fee,
        })
    }
}

/// One candidate route with its market data resolved: the path, proved by
/// construction, plus both hops in trade order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PricedCycle {
    pub path: ArbitragePath,
    pub first: PricedHop,
    pub second: PricedHop,
}

impl PricedCycle {
    /// The path is read off the two edges, so it cannot disagree with them.
    pub fn new(first: &GraphEdge, second: &GraphEdge) -> Result<Self> {
        let path = ArbitragePath::two_hops(first.id, second.id)?;
        Ok(Self {
            path,
            first: PricedHop::from_edge(first)?,
            second: PricedHop::from_edge(second)?,
        })
    }

    /// What comes back out for `input`, or the arithmetic reason it cannot be
    /// stated. Delegates to the one composition in `math`, so the search and a
    /// hand quote of the same route can never drift apart.
    pub fn quote(&self, input: U256) -> Result<U256> {
        swap_through_two_hops(
            (
                self.first.reserve_in,
                self.first.reserve_out,
                self.first.fee,
            ),
            (
                self.second.reserve_in,
                self.second.reserve_out,
                self.second.fee,
            ),
            input,
        )
        .map_err(OpportunityError::Math)
    }

    /// The derived upper bound of the search domain: `second.reserve_out - 1`.
    /// `None` when the exit pool holds a single unit or less, so the bound would
    /// sit below the smallest legal input and there is no domain at all.
    pub fn input_upper_bound(&self) -> Option<U256> {
        let upper = self.second.reserve_out.checked_sub(U256::ONE)?;
        (upper >= U256::ONE).then_some(upper)
    }
}

/// How the peak was looked for. One strategy exists in v0.1, and naming it in
/// the result is what keeps a reported opportunity from reading like a proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum SearchStrategy {
    /// Discrete ternary search on a unimodal integer profit sequence, closed by
    /// comparing inputs inside the final window.
    BoundedTernary,
}

/// The knobs of [`find_optimal_input`]: every one of them is documented and
/// echoed back in the result, so no search limit hides in the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SearchPolicy {
    /// Hard cap on ternary rounds. 256 is enough to close the window for any
    /// domain reachable from a `Sync` log, which encodes reserves as `uint112`:
    /// each round multiplies the interval by 2/3, so `112 * ln 2 / ln(3/2)`
    /// (192) rounds already reduce it below `scan_width`.
    pub max_rounds: u32,
    /// Interval width at which the search stops narrowing and starts comparing
    /// inputs one by one. Inside that window the smallest input wins ties; a
    /// plateau wider than the window is resolved by whatever the ternary phase
    /// left standing, which is why the exhaustive cross-check compares profits
    /// and not inputs.
    pub scan_width: u32,
}

impl Default for SearchPolicy {
    fn default() -> Self {
        Self {
            max_rounds: 256,
            scan_width: 32,
        }
    }
}

/// What the search actually did, so a result can be audited without rerunning
/// it (§23 of the task: an approximate optimum has to be reported as one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SearchRecord {
    pub strategy: SearchStrategy,
    pub rounds: u32,
    pub evaluations: u32,
    pub lower_bound: U256,
    pub upper_bound: U256,
    /// `true` when the interval reached `scan_width` and every input in it was
    /// compared. `false` means the round budget ran out first, so the reported
    /// peak is the best of a still-wide window sampled at `scan_count` points.
    pub interval_closed: bool,
    /// Inputs compared in the closing pass.
    pub scan_count: u32,
    pub policy: SearchPolicy,
}

/// The best input found for one cycle, with the search behind it.
///
/// The priced cycle travels with the result: a peak input means nothing without
/// the two reserves and two fees it was found against, and carrying them here
/// is what stops an audit from having to trust that the caller still holds the
/// same snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptimizedCycle {
    pub cycle: PricedCycle,
    pub simulation: PathSimulation,
    pub record: SearchRecord,
}

impl OptimizedCycle {
    pub fn gross_profit(&self) -> Option<U256> {
        self.simulation.gross_profit()
    }
}

/// Search `1 ..= second.reserve_out - 1` for the input that maximises gross
/// profit and return that round trip.
///
/// An empty domain (an exit pool holding at most one unit of the input token)
/// is reported as [`OpportunityError::EmptySearchDomain`]: no input could pay,
/// so there is nothing to optimise over.
pub fn find_optimal_input(cycle: &PricedCycle, policy: SearchPolicy) -> Result<OptimizedCycle> {
    let lower = U256::ONE;
    let upper = cycle
        .input_upper_bound()
        .filter(|upper| *upper >= lower)
        .ok_or(OpportunityError::EmptySearchDomain {
            lower,
            upper: cycle.second.reserve_out.saturating_sub(U256::ONE),
        })?;

    let mut lo = lower;
    let mut hi = upper;
    let mut rounds = 0u32;
    let mut evaluations = 0u32;
    let width_limit = U256::from(policy.scan_width.max(1));

    while hi - lo > width_limit && rounds < policy.max_rounds {
        let third = (hi - lo) / U256::from(3u32);
        let left = lo + third;
        let right = hi - third;
        let left_out = quote_counting(cycle, left, &mut evaluations)?;
        let right_out = quote_counting(cycle, right, &mut evaluations)?;
        let ordering = PathSimulation::compare_profit((left_out, left), (right_out, right))
            .ok_or(OpportunityError::Math(MathError::Overflow))?;
        // On a unimodal curve: right better means the peak is at or right of the
        // left probe, left better means at or left of the right probe, and a tie
        // means both probes sit on the top, so the interval closes from both ends
        // at once. Cutting one side on a tie is what loses a plateau: the floored
        // curve steps rather than runs — on the {A,C} cycle's `C -> A -> C`
        // direction it reads 2 815 at 120 inputs with 2 814 at the 212 that
        // separate them — and a search that dropped one side on a tie stopped one
        // unit under that top.
        match ordering {
            Ordering::Less => lo = left,
            Ordering::Greater => hi = right,
            Ordering::Equal => {
                lo = left;
                hi = right;
            }
        }
        rounds += 1;
    }
    let interval_closed = hi - lo <= width_limit;

    // Closing pass. Within `scan_width` this compares every remaining input and
    // the smallest wins ties; a starved budget samples the wide remainder at
    // `scan_width + 1` points and says so through `interval_closed`.
    let span = hi - lo;
    let step = if interval_closed {
        U256::ONE
    } else {
        span / width_limit + U256::ONE
    };
    let mut best: Option<(U256, U256)> = None;
    let mut scan_count = 0u32;
    let mut x = lo;
    loop {
        let out = quote_counting(cycle, x, &mut evaluations)?;
        let better = match best {
            None => true,
            Some((best_out, best_in)) => {
                PathSimulation::compare_profit((out, x), (best_out, best_in))
                    .map(|ordering| ordering.is_gt())
                    .unwrap_or(false)
            }
        };
        if better {
            best = Some((out, x));
        }
        scan_count = scan_count.saturating_add(1);
        if x >= hi {
            break;
        }
        x = (x + step).min(hi);
    }
    let (output, input) = best.unwrap_or((U256::ZERO, lower));

    Ok(OptimizedCycle {
        cycle: *cycle,
        simulation: PathSimulation::new(cycle.path, input, output),
        record: SearchRecord {
            strategy: SearchStrategy::BoundedTernary,
            rounds,
            evaluations,
            lower_bound: lower,
            upper_bound: upper,
            interval_closed,
            scan_count,
            policy,
        },
    })
}

fn quote_counting(cycle: &PricedCycle, input: U256, evaluations: &mut u32) -> Result<U256> {
    *evaluations = evaluations.saturating_add(1);
    cycle.quote(input)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address};

    use evm_core::{BlockNumber, ChainId, LogIndex, TokenId};

    use super::*;

    const CHAIN: ChainId = ChainId(7);
    const A: Address = address!("0x000000000000000000000000000000000000000a");
    const B: Address = address!("0x000000000000000000000000000000000000000b");
    const P1: Address = address!("0x00000000000000000000000000000000000000f1");
    const P2: Address = address!("0x00000000000000000000000000000000000000f2");
    const FEE: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };

    fn hop(pool: Address, from: Address, to: Address) -> evm_graph::EdgeId {
        evm_graph::EdgeId::new(
            PoolId::new(CHAIN, pool),
            TokenId::new(CHAIN, from),
            TokenId::new(CHAIN, to),
        )
    }

    fn edge(
        pool: Address,
        from: Address,
        to: Address,
        reserve_in: u128,
        reserve_out: u128,
        fee: Option<Fee>,
    ) -> GraphEdge {
        GraphEdge {
            id: hop(pool, from, to),
            reserve_in: U256::from(reserve_in),
            reserve_out: U256::from(reserve_out),
            fee,
            state_position: evm_state::UpdatePosition::new(BlockNumber(1), LogIndex(0)),
        }
    }

    /// `A -> pool1 -> B -> pool2 -> A`, both hops at a 0.3 % fee.
    fn cycle(r1_in: u128, r1_out: u128, r2_in: u128, r2_out: u128) -> PricedCycle {
        let first = edge(P1, A, B, r1_in, r1_out, Some(FEE));
        let second = edge(P2, B, A, r2_in, r2_out, Some(FEE));
        PricedCycle::new(&first, &second).expect("priced")
    }

    #[test]
    fn a_hop_without_an_attested_fee_is_refused_not_defaulted() {
        let unattested = edge(P1, A, B, 100_000, 50_000, None);
        assert_eq!(
            PricedHop::from_edge(&unattested),
            Err(OpportunityError::UnattestedFee(
                PoolId::new(CHAIN, P1),
                TokenId::new(CHAIN, A)
            ))
        );
    }

    #[test]
    fn an_empty_side_cannot_be_priced() {
        let first = edge(P1, A, B, 100_000, 50_000, Some(FEE));
        let empty = edge(P2, B, A, 50_000, 0, Some(FEE));
        assert_eq!(
            PricedCycle::new(&first, &empty),
            Err(OpportunityError::Math(MathError::InvalidReserve))
        );
    }

    #[test]
    fn one_pool_round_trip_never_reaches_the_optimizer() {
        let first = edge(P1, A, B, 100_000, 50_000, Some(FEE));
        let back = edge(P1, B, A, 50_000, 100_000, Some(FEE));
        assert_eq!(
            PricedCycle::new(&first, &back),
            Err(OpportunityError::Path(crate::error::PathError::SamePool(
                PoolId::new(CHAIN, P1),
                PoolId::new(CHAIN, P1)
            )))
        );
    }

    #[test]
    fn the_derived_upper_bound_contains_the_peak_and_brute_force_agrees() {
        // Pool 1 sells B cheap, pool 2 buys B dear: a 1.25 price product.
        let c = cycle(100_000, 50_000, 50_000, 125_000);
        let best = find_optimal_input(&c, SearchPolicy::default()).expect("search");
        assert!(best.simulation.is_profitable(), "{best:?}");
        assert!(
            best.simulation.input < c.second.reserve_out,
            "the bound holds"
        );
        assert_eq!(
            best.record.upper_bound,
            c.second.reserve_out - U256::ONE,
            "the bound is the derived one, not a ratio"
        );

        // Exhaustive check over the same domain: 125k inputs, small enough to
        // enumerate and large enough to contain the peak.
        let mut brute: Option<(U256, U256)> = None;
        let mut brute_ties = 0u32;
        let domain_end = best.record.upper_bound;
        let mut x = U256::ONE;
        while x <= domain_end {
            let out = c.quote(x).expect("quote");
            match brute {
                None => brute = Some((out, x)),
                Some((out_b, in_b)) => {
                    match PathSimulation::compare_profit((out, x), (out_b, in_b)) {
                        Some(Ordering::Greater) => {
                            brute = Some((out, x));
                            brute_ties = 1;
                        }
                        Some(Ordering::Equal) => brute_ties += 1,
                        _ => {}
                    }
                }
            }
            x += U256::ONE;
        }
        let brute = brute.expect("domain non-empty");
        assert_eq!(
            best.simulation.gross_profit(),
            PathSimulation::new(c.path, brute.1, brute.0).gross_profit(),
            "the search and the exhaustive scan must find the same maximum"
        );
        // The peak is a plateau here, so which input a run reports is a tie-break
        // decision (`SearchPolicy::scan_width`), not a difference in profit. That
        // is exactly why the cross-check above compares money and not inputs.
        assert!(
            brute_ties > 1,
            "expected the integer peak to tie: {brute_ties}"
        );
        assert_eq!(
            c.quote(best.simulation.input),
            Ok(best.simulation.output),
            "the reported round trip must reproduce from the reported input"
        );
    }

    #[test]
    fn the_search_beats_the_closed_form_instead_of_only_matching_it() {
        // The algebraic peak of the unfloored function, as an independent bound.
        let c = cycle(100_000, 50_000, 50_000, 125_000);
        let best = find_optimal_input(&c, SearchPolicy::default()).expect("search");
        let analytic_input = U256::from(5_759u32);
        let analytic_out = c.quote(analytic_input).expect("quote");
        assert!(
            PathSimulation::compare_profit(
                (best.simulation.output, best.simulation.input),
                (analytic_out, analytic_input),
            )
            .is_some_and(|ordering| !ordering.is_lt()),
            "the searched peak must not lose to the closed form"
        );
    }

    #[test]
    fn an_unprofitable_cycle_is_returned_as_a_loss_not_a_zero_profit() {
        // Same pair, prices within the fee: nothing to capture.
        let c = cycle(100_000, 50_000, 50_000, 50_125);
        let best = find_optimal_input(&c, SearchPolicy::default()).expect("search");
        assert!(!best.simulation.is_profitable());
        assert_eq!(best.gross_profit(), None);
        assert!(best.simulation.gross_loss().is_some());
    }

    #[test]
    fn a_second_hop_holding_one_unit_leaves_no_domain() {
        let c = cycle(100_000, 50_000, 50_000, 1);
        assert_eq!(
            find_optimal_input(&c, SearchPolicy::default()),
            Err(OpportunityError::EmptySearchDomain {
                lower: U256::ONE,
                upper: U256::ZERO
            })
        );
    }

    #[test]
    fn the_record_states_what_the_search_did() {
        let c = cycle(100_000, 50_000, 50_000, 125_000);
        let best = find_optimal_input(&c, SearchPolicy::default()).expect("search");
        assert_eq!(best.record.strategy, SearchStrategy::BoundedTernary);
        assert_eq!(best.record.lower_bound, U256::ONE);
        assert!(best.record.interval_closed);
        assert!(best.record.rounds > 0);
        assert!(best.record.evaluations >= best.record.rounds * 2 + best.record.scan_count);
        assert_eq!(best.record.policy, SearchPolicy::default());
    }

    #[test]
    fn a_starved_round_budget_is_reported_instead_of_claiming_the_peak() {
        let c = cycle(
            1_000_000_000_000_000,
            500_000_000_000_000,
            500_000_000_000_000,
            1_250_000_000_000_000,
        );
        let starved = find_optimal_input(
            &c,
            SearchPolicy {
                max_rounds: 1,
                scan_width: 8,
            },
        )
        .expect("search");
        assert!(!starved.record.interval_closed);
        assert_eq!(starved.record.rounds, 1);
        assert_eq!(starved.record.scan_count, 9);
        let closed = find_optimal_input(&c, SearchPolicy::default()).expect("search");
        assert!(closed.record.interval_closed);
        assert!(PathSimulation::compare_profit(
            (closed.simulation.output, closed.simulation.input),
            (starved.simulation.output, starved.simulation.input),
        )
        .is_some_and(|ordering| !ordering.is_lt()));
    }

    #[test]
    fn identical_inputs_produce_identical_search_results() {
        let c = cycle(
            40_000_000_000_000_000_000,
            48_000_000_000_000,
            40_000_000_000_000,
            40_000_000_000_000_000_000,
        );
        let a = find_optimal_input(&c, SearchPolicy::default()).expect("first");
        let b = find_optimal_input(&c, SearchPolicy::default()).expect("second");
        assert_eq!(a, b);
        assert_eq!(
            serde_json::to_string(&a.record).expect("json"),
            serde_json::to_string(&b.record).expect("json")
        );
        assert!(a.simulation.is_profitable(), "this shape is profitable");
    }
}
