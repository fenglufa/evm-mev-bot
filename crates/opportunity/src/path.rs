//! Paths: which pools, in which direction, and whether the round trip closes.
//!
//! A hop here is not a new concept. [`evm_graph::EdgeId`] already names a pool
//! plus a direction through it, which is exactly the identity M3 needs (§7 of
//! the task: a pair alone cannot tell two markets apart, and `A -> B` and `B -> A`
//! through the *same* pool must not read as two hops). So a hop is a type alias
//! for that edge identity, and a path is a list of them plus the rules that make
//! the list a cycle.

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{PoolId, TokenId};
use evm_graph::EdgeId;

use crate::error::PathError;

/// One leg of a route: a pool and the direction traded through it.
pub type Hop = EdgeId;

/// A closed route: spends one token, ends holding that token again.
///
/// Constructed only through [`ArbitragePath::two_hops`], so the invariants are
/// guaranteed rather than checked by convention: exactly two hops, the middle
/// token lines up, the route returns to where it started, and the two hops are
/// different pools. The last one is what makes this an arbitrage instead of a
/// round trip — swapping `A -> B -> A` inside a single pool is a fee donation,
/// and letting it through would put fake profit in every report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ArbitragePath {
    first: Hop,
    second: Hop,
}

impl ArbitragePath {
    /// The only shape v0.1 prices: `A -> pool1 -> B -> pool2 -> A`.
    pub fn two_hops(first: Hop, second: Hop) -> Result<Self, PathError> {
        if first.pool == second.pool {
            return Err(PathError::SamePool(first.pool, second.pool));
        }
        if first.token_out != second.token_in {
            return Err(PathError::TokenMismatch(second.pool, first.token_out));
        }
        if second.token_out != first.token_in {
            return Err(PathError::NotACycle(first.token_in));
        }
        Ok(Self { first, second })
    }

    /// Build a two-pool path from two edges, in trade order.
    pub fn from_edges(
        first: &evm_graph::GraphEdge,
        second: &evm_graph::GraphEdge,
    ) -> Result<Self, PathError> {
        Self::two_hops(first.id, second.id)
    }

    pub fn hops(&self) -> [Hop; 2] {
        [self.first, self.second]
    }

    pub fn first(&self) -> Hop {
        self.first
    }

    pub fn second(&self) -> Hop {
        self.second
    }

    /// The token spent at the start and received at the end.
    pub fn input_token(&self) -> TokenId {
        self.first.token_in
    }

    /// The token the route passes through between the two pools.
    pub fn mid_token(&self) -> TokenId {
        self.first.token_out
    }

    pub fn pools(&self) -> [PoolId; 2] {
        [self.first.pool, self.second.pool]
    }

    /// Stable identity for ordering and deduplication: a path is one object, so
    /// the same snapshot and the same route always sort to the same place.
    pub fn identity(&self) -> (Hop, Hop) {
        (self.first, self.second)
    }

    /// How many hops this path has — always 2, kept because the error that
    /// reports it takes the count, and a future 3-hop path has to make that
    /// number mean something real.
    pub fn hop_count(&self) -> usize {
        2
    }
}

/// One path priced at one input amount: what goes in, what comes back out.
///
/// Deliberately not a signed profit. A round trip that loses money has `output <
/// input`, and `U256` has no negative — wrapping it would either panic or
/// silently produce `2^256 - loss`. The comparison stays here as two numbers and
/// an [`Option`], so "no profit" is a fact about the market rather than an
/// arithmetic accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PathSimulation {
    pub path: ArbitragePath,
    pub input: U256,
    pub output: U256,
}

impl PathSimulation {
    pub fn new(path: ArbitragePath, input: U256, output: U256) -> Self {
        Self {
            path,
            input,
            output,
        }
    }

    /// `Some(profit)` only when the round trip actually gains. A route that
    /// comes back exactly even reports `None`: it moved tokens for nothing, and
    /// `Some(0)` would let a break-even scan read as "found something".
    pub fn gross_profit(&self) -> Option<U256> {
        let profit = self.output.checked_sub(self.input)?;
        (!profit.is_zero()).then_some(profit)
    }

    pub fn is_profitable(&self) -> bool {
        self.output > self.input
    }

    /// What the round trip lost, if it lost. Break-even reports `None` here too,
    /// so `gross_profit() == None && gross_loss() == None` is the whole of
    /// "came back with exactly what went in".
    pub fn gross_loss(&self) -> Option<U256> {
        let loss = self.input.checked_sub(self.output)?;
        (!loss.is_zero()).then_some(loss)
    }

    /// `profit(a) > profit(b)` without subtracting, so a loss can never
    /// underflow: `out_a - in_a > out_b - in_b` rearranges to
    /// `out_a + in_b > out_b + in_a`, which is all-positive.
    pub(crate) fn compare_profit(
        left: (U256, U256),
        right: (U256, U256),
    ) -> Option<std::cmp::Ordering> {
        let lhs = left.0.checked_add(right.1)?;
        let rhs = right.0.checked_add(left.1)?;
        Some(lhs.cmp(&rhs))
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address};

    use super::*;
    use evm_core::{
        BlockNumber, ChainId, Fee, LogIndex, PoolMeta, PoolState, PoolType, ProtocolId,
    };
    use evm_graph::GraphEdge;

    const CHAIN: ChainId = ChainId(7);
    const A: Address = address!("0x000000000000000000000000000000000000000a");
    const B: Address = address!("0x000000000000000000000000000000000000000b");
    const C: Address = address!("0x000000000000000000000000000000000000000c");
    const P1: Address = address!("0x00000000000000000000000000000000000000f1");
    const P2: Address = address!("0x00000000000000000000000000000000000000f2");

    fn hop(pool: Address, from: Address, to: Address) -> Hop {
        EdgeId::new(
            PoolId::new(CHAIN, pool),
            TokenId::new(CHAIN, from),
            TokenId::new(CHAIN, to),
        )
    }

    #[test]
    fn a_two_pool_cycle_is_accepted() {
        let path = ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, A)).expect("cycle");
        assert_eq!(path.input_token(), TokenId::new(CHAIN, A));
        assert_eq!(path.mid_token(), TokenId::new(CHAIN, B));
        assert_eq!(
            path.pools(),
            [PoolId::new(CHAIN, P1), PoolId::new(CHAIN, P2)]
        );
        assert_eq!(path.hop_count(), 2);
        // The reverse direction is a different path: different input token.
        let backwards = ArbitragePath::two_hops(hop(P2, A, B), hop(P1, B, A)).expect("cycle");
        assert_ne!(path, backwards);
    }

    #[test]
    fn one_pool_round_trip_is_refused_by_name() {
        assert_eq!(
            ArbitragePath::two_hops(hop(P1, A, B), hop(P1, B, A)),
            Err(PathError::SamePool(
                PoolId::new(CHAIN, P1),
                PoolId::new(CHAIN, P1)
            ))
        );
    }

    #[test]
    fn hops_that_do_not_join_at_the_middle_token_are_refused() {
        assert_eq!(
            ArbitragePath::two_hops(hop(P1, A, B), hop(P2, C, A)),
            Err(PathError::TokenMismatch(
                PoolId::new(CHAIN, P2),
                TokenId::new(CHAIN, B)
            ))
        );
    }

    #[test]
    fn a_route_that_does_not_close_is_refused() {
        assert_eq!(
            ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, C)),
            Err(PathError::NotACycle(TokenId::new(CHAIN, A)))
        );
    }

    #[test]
    fn a_path_that_loses_money_reports_no_profit_and_the_loss() {
        let path = ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, A)).expect("cycle");
        let losing = PathSimulation::new(path, U256::from(100u32), U256::from(90u32));
        assert_eq!(losing.gross_profit(), None);
        assert_eq!(losing.gross_loss(), Some(U256::from(10u32)));
        assert!(!losing.is_profitable());
        let winning = PathSimulation::new(path, U256::from(100u32), U256::from(115u32));
        assert_eq!(winning.gross_profit(), Some(U256::from(15u32)));
        assert_eq!(winning.gross_loss(), None);
    }

    #[test]
    fn break_even_is_not_an_opportunity() {
        let path = ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, A)).expect("cycle");
        let flat = PathSimulation::new(path, U256::from(77u32), U256::from(77u32));
        assert_eq!(flat.gross_profit(), None);
        assert_eq!(flat.gross_loss(), None);
    }

    #[test]
    fn profit_comparison_never_subtracts() {
        // (90 in, 100 out) beats (1 in, 2 out): 10 > 1.
        let better = PathSimulation::compare_profit(
            (U256::from(100u32), U256::from(90u32)),
            (U256::from(2u32), U256::from(1u32)),
        );
        assert_eq!(better, Some(std::cmp::Ordering::Greater));
        // The same comparison with a loss on the left still holds: -5 < 1.
        let worse = PathSimulation::compare_profit(
            (U256::from(5u32), U256::from(10u32)),
            (U256::from(2u32), U256::from(1u32)),
        );
        assert_eq!(worse, Some(std::cmp::Ordering::Less));
        // Beyond 2^256 the rearrangement itself would overflow, and says so.
        assert_eq!(
            PathSimulation::compare_profit(
                (U256::MAX, U256::from(1u32)),
                (U256::from(1u32), U256::from(1u32))
            ),
            None
        );
    }

    #[test]
    fn the_same_route_always_sorts_to_the_same_place() {
        let p1_first = ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, A)).expect("p");
        let p2_first = ArbitragePath::two_hops(hop(P2, B, A), hop(P1, A, B)).expect("p");
        // Two different routes, so the ordering below is a real decision and not
        // a tie.
        assert_ne!(p1_first, p2_first);
        let mut from_one_side = [p1_first, p2_first];
        let mut from_the_other = [p2_first, p1_first];
        from_one_side.sort();
        from_the_other.sort();
        assert_eq!(
            from_one_side, from_the_other,
            "the order comes from the routes, not from the order they were pushed in"
        );
        // `Ord` on the struct and `Ord` on the identity must be the same rule, or
        // `identity()` would be documentation rather than the sort key.
        let mut by_identity = [p2_first, p1_first];
        by_identity.sort_by_key(|path| path.identity());
        assert_eq!(from_one_side, by_identity);
        assert!(from_one_side[0].identity() < from_one_side[1].identity());
    }

    // Silence the unused-import lint for the types the helper constructors below
    // need in scope, and keep `GraphEdge` reachable for `from_edges` tests.
    #[allow(dead_code)]
    fn meta(pool: Address, t0: Address, t1: Address, fee: Option<Fee>) -> PoolMeta {
        PoolMeta {
            id: PoolId::new(CHAIN, pool),
            protocol: ProtocolId::new("test"),
            token0: TokenId::new(CHAIN, t0),
            token1: TokenId::new(CHAIN, t1),
            fee,
            pool_type: PoolType::ConstantProduct,
        }
    }

    #[allow(dead_code)]
    fn state(pool: Address, r0: u64, r1: u64) -> PoolState {
        PoolState {
            pool: PoolId::new(CHAIN, pool),
            reserve0: U256::from(r0),
            reserve1: U256::from(r1),
            block_number: BlockNumber(1),
            log_index: LogIndex(0),
        }
    }

    #[test]
    fn a_path_can_be_built_straight_off_two_edges() {
        let meta1 = meta(P1, A, B, None);
        let meta2 = meta(P2, A, B, None);
        let edges1 = GraphEdge::pair(&meta1, &state(P1, 100, 200)).expect("edges");
        let edges2 = GraphEdge::pair(&meta2, &state(P2, 300, 400)).expect("edges");
        let path = ArbitragePath::from_edges(&edges1[0], &edges2[1]).expect("cycle");
        assert_eq!(
            path,
            ArbitragePath::two_hops(hop(P1, A, B), hop(P2, B, A)).expect("same")
        );
    }
}
