//! What a search hands back: one closed route, named, with nothing about money.
//!
//! A [`CycleCandidate`] is a statement about topology and nothing else. It says
//! which chain and which block a route was read at, which pools it walks, in
//! which direction, and whether the fees along it have been proved. It does not
//! say that anything could be bought or sold on it, that any amount would come
//! out ahead, or that a simulation or a transaction would succeed. Those are
//! later levels — `Path != CycleCandidate != Executable Opportunity != Simulated
//! Profit != Executed Profit != Realized Profit` — and collapsing them here is
//! how a graph walk ends up claiming profit it never computed.

use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, ChainId, PoolId, TokenId};
use evm_graph::EdgeId;

/// The rotation-free identity of a cycle.
///
/// `A -> B -> C -> A` walked from `B` is `B -> C -> A -> B`: the same three
/// edges, the same three pools, the same route, entered one seat to the left.
/// Reporting both would inflate every count on the way to a decision that should
/// be made once. A rotation is therefore not a second candidate, and the key is
/// the lexicographically smallest rotation of the edge sequence — a property of
/// the route alone, computable without knowing where the walk started, and with
/// no dependence on pointer order, hash iteration, or the order the edges were
/// discovered in.
///
/// Reversal is *not* folded into this key. `A -> B -> C -> A` and `A -> C -> B ->
/// A` trade in the opposite direction through every pool, which puts different
/// reserves in the `reserve_in` seat, different fee arithmetic, and different
/// price impact. They are two candidates, and a search that merged them would
/// throw away half of what it found.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalKey(Vec<EdgeId>);

impl CanonicalKey {
    /// The key of one cycle, from its edges in trade order. The entry point does
    /// not matter: every rotation of one route produces one key.
    pub fn new(edges: &[EdgeId]) -> Self {
        Self(minimum_rotation(edges))
    }

    /// The edges as they sort in the output: smallest edge first, then following
    /// trade order.
    pub fn edges(&self) -> &[EdgeId] {
        &self.0
    }

    /// The token this cycle is entered at once rotations are folded — the first
    /// edge's `token_in` under the canonical rotation.
    pub fn start_token(&self) -> Option<TokenId> {
        self.0.first().map(|edge| edge.token_in)
    }

    pub fn hop_count(&self) -> usize {
        self.0.len()
    }
}

impl std::fmt::Display for CanonicalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cycle[")?;
        for (index, edge) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{:?}", edge.token_in.address)?;
            write!(f, ">{}", edge.pool.address)?;
        }
        f.write_str("]")
    }
}

/// The smallest rotation of `edges`, compared lexicographically by [`EdgeId`].
fn minimum_rotation(edges: &[EdgeId]) -> Vec<EdgeId> {
    if edges.len() < 2 {
        return edges.to_vec();
    }
    let len = edges.len();
    let mut best: Vec<EdgeId> = edges.to_vec();
    for shift in 1..len {
        let rotation: Vec<EdgeId> = (shift..shift + len)
            .map(|index| edges[index % len])
            .collect();
        if rotation < best {
            best = rotation;
        }
    }
    best
}

/// Whether every fee on a cycle has been proved.
///
/// Two states, and the second is neither "zero" nor "the usual 0.3%":
/// `GraphEdge::fee` is an `Option`, and `None` means nobody attested that pool's
/// fee. A cycle can be *enumerated* with an unattested fee — it is a real shape
/// in the graph — but it cannot be priced, because pricing it would mean inventing
/// a number and then calling the result profit. This layer resolves no fee and
/// defaults none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FeeStatus {
    /// Every edge carries an attested fee, so a later stage has everything it
    /// needs to try to price this route.
    Complete,
    /// At least one edge's fee is `None`: a topological finding, not a quote.
    Incomplete,
}

impl FeeStatus {
    /// Fold one edge's attestation into the running status of a cycle. Once a
    /// cycle is `Incomplete` it stays incomplete: one unproved fee is enough.
    pub fn merge(self, edge_fee_attested: bool) -> Self {
        match (self, edge_fee_attested) {
            (Self::Complete, true) => Self::Complete,
            _ => Self::Incomplete,
        }
    }

    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// One closed route found in one graph at one block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleCandidate {
    pub chain_id: ChainId,
    /// The block the graph is taken at, copied onto the candidate so a candidate
    /// cannot be read apart from the market state it came from. Never a later
    /// block and never `latest`: that would let a route found at one moment be
    /// quoted against another.
    pub target_block: BlockNumber,
    /// The token spent at the start and received again at the end, as it reads
    /// under [`CycleCandidate::canonical_key`] — so two walks that entered the
    /// same cycle at different seats still report one start token.
    pub start_token: TokenId,
    /// The directed edges in trade order, starting at `start_token`. Identities
    /// only: reserves live in the graph, and copying them here would let a
    /// candidate assert a price the search never used.
    pub edges: Vec<EdgeId>,
    /// Always `edges.len()`, stored so a reader can check the bound without
    /// walking the vector.
    pub hop_count: usize,
    pub canonical_key: CanonicalKey,
    pub fee_status: FeeStatus,
}

impl CycleCandidate {
    /// Build a candidate from a closed trail, refusing anything that is not a
    /// cycle this layer is allowed to name: fewer than two hops, a route that
    /// does not return to its start token, a pool used twice, or a token repeated
    /// before the route closes.
    ///
    /// These are the candidate's own invariants rather than a re-check of the
    /// graph — the graph's state, block and reserves were proved upstream — and
    /// keeping them in one constructor is what lets [`crate::search`] stay a
    /// search instead of a validator.
    pub fn assemble(
        chain_id: ChainId,
        target_block: BlockNumber,
        edges: &[EdgeId],
        fee_status: FeeStatus,
    ) -> Option<Self> {
        if edges.len() < crate::config::PathFinderConfig::MIN_MAX_HOPS {
            return None;
        }
        if edges.len() > crate::config::PathFinderConfig::MAX_MAX_HOPS {
            return None;
        }
        let start = match edges.first() {
            Some(edge) => edge.token_in,
            None => return None,
        };
        let closed_by = match edges.last() {
            Some(edge) => edge.token_out,
            None => return None,
        };
        if closed_by != start {
            return None;
        }
        let mut pools: Vec<PoolId> = Vec::with_capacity(edges.len());
        // The tokens the route has already stood on. The start token is on that
        // list from the beginning, and returning to it is what closes the cycle —
        // so it is the one token a route may arrive at twice, and only on the
        // last hop.
        let mut visited: Vec<TokenId> = vec![start];
        for (index, edge) in edges.iter().enumerate() {
            if edge.token_in == edge.token_out {
                return None;
            }
            if pools.contains(&edge.pool) {
                return None;
            }
            pools.push(edge.pool);
            let is_closing = index + 1 == edges.len();
            if edge.token_out == start {
                // Coming home early would make this two cycles walked as one, and
                // the shorter one is the finding.
                if !is_closing {
                    return None;
                }
            } else if visited.contains(&edge.token_out) {
                return None;
            }
            visited.push(edge.token_out);
        }
        if !edges
            .windows(2)
            .all(|pair| pair[0].token_out == pair[1].token_in)
        {
            return None;
        }
        let canonical_key = CanonicalKey::new(edges);
        let start_token = canonical_key.start_token()?;
        Some(Self {
            chain_id,
            target_block,
            start_token,
            edges: canonical_key.edges().to_vec(),
            hop_count: canonical_key.hop_count(),
            fee_status,
            canonical_key,
        })
    }

    /// The pools walked, in trade order.
    pub fn pools(&self) -> Vec<PoolId> {
        self.edges.iter().map(|edge| edge.pool).collect()
    }

    /// The tokens the route passes through, start token first, `hop_count + 1`
    /// long, ending where it began.
    pub fn tokens(&self) -> Vec<TokenId> {
        let mut tokens: Vec<TokenId> = self.edges.iter().map(|edge| edge.token_in).collect();
        if let Some(last) = self.edges.last() {
            tokens.push(last.token_out);
        }
        tokens
    }

    /// Adjacency, restated as a check: `edge[i].token_out == edge[i+1].token_in`.
    pub fn is_connected(&self) -> bool {
        self.edges
            .windows(2)
            .all(|pair| pair[0].token_out == pair[1].token_in)
    }

    /// The block and chain this candidate must agree with, so a test or an
    /// evidence gate can require it instead of assuming it.
    pub fn belongs_to(&self, graph: &evm_graph::GraphSnapshot) -> bool {
        self.chain_id == graph.chain_id()
            && self.target_block == graph.block_number()
            && self.edges.iter().all(|edge| graph.edge(*edge).is_some())
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    const CHAIN: ChainId = ChainId(7);
    const BLOCK: BlockNumber = BlockNumber(100);
    const A: TokenId = TokenId::new(
        CHAIN,
        address!("0x000000000000000000000000000000000000000a"),
    );
    const B: TokenId = TokenId::new(
        CHAIN,
        address!("0x000000000000000000000000000000000000000b"),
    );
    const C: TokenId = TokenId::new(
        CHAIN,
        address!("0x000000000000000000000000000000000000000c"),
    );
    const P1: PoolId = PoolId::new(
        CHAIN,
        address!("0x00000000000000000000000000000000000000f1"),
    );
    const P2: PoolId = PoolId::new(
        CHAIN,
        address!("0x00000000000000000000000000000000000000f2"),
    );
    const P3: PoolId = PoolId::new(
        CHAIN,
        address!("0x00000000000000000000000000000000000000f3"),
    );
    const P4: PoolId = PoolId::new(
        CHAIN,
        address!("0x00000000000000000000000000000000000000f4"),
    );

    fn ab() -> EdgeId {
        EdgeId::new(P1, A, B)
    }

    fn bc() -> EdgeId {
        EdgeId::new(P2, B, C)
    }

    fn ca() -> EdgeId {
        EdgeId::new(P3, C, A)
    }

    /// The smallest rotation is a property of the route, not of where the walk
    /// entered it.
    #[test]
    fn every_rotation_of_one_cycle_shares_one_key() {
        let forward = [ab(), bc(), ca()];
        let from_b = [bc(), ca(), ab()];
        let from_c = [ca(), ab(), bc()];
        let key = CanonicalKey::new(&forward);
        assert_eq!(key, CanonicalKey::new(&from_b));
        assert_eq!(key, CanonicalKey::new(&from_c));
        // And the key is one of those rotations, not a third sequence.
        assert!([forward, from_b, from_c]
            .iter()
            .any(|rotation| rotation.as_slice() == key.edges()));
    }

    #[test]
    fn the_key_starts_at_its_smallest_edge() {
        let rotated = [bc(), ca(), ab()];
        let key = CanonicalKey::new(&rotated);
        assert_eq!(key.edges(), [ab(), bc(), ca()]);
        assert_eq!(key.start_token(), Some(A));
        assert_eq!(key.hop_count(), 3);
    }

    #[test]
    fn a_reversal_is_a_different_key() {
        // The same three pools traded the other way round: every edge is flipped,
        // so no rotation of one sequence equals the other.
        let there = CanonicalKey::new(&[ab(), bc(), ca()]);
        let back = CanonicalKey::new(&[
            EdgeId::new(P3, A, C),
            EdgeId::new(P2, C, B),
            EdgeId::new(P1, B, A),
        ]);
        assert_ne!(there, back);
    }

    #[test]
    fn an_open_route_has_no_candidate() {
        assert!(
            CycleCandidate::assemble(CHAIN, BLOCK, &[ab(), bc()], FeeStatus::Complete).is_none()
        );
    }

    #[test]
    fn hops_that_do_not_join_have_no_candidate() {
        // A -> B, then C -> A: the second hop does not start where the first ended.
        let edges = [ab(), EdgeId::new(P2, C, A)];
        assert!(CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).is_none());
    }

    #[test]
    fn a_pool_used_twice_has_no_candidate() {
        // A -> B -> A through one pool: a fee donation, not an arbitrage.
        let edges = [ab(), EdgeId::new(P1, B, A)];
        assert!(CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).is_none());
    }

    /// §34.11 — a closed trail may not stand on its start token before the last
    /// hop. The route below would be legal at a wider bound, and it is the token
    /// rule that refuses it: the four pools are all different, so nothing else can.
    #[test]
    fn a_route_that_comes_home_early_is_two_cycles_not_one() {
        // `A -> B -> A -> C -> A`, three hops at most, so assemble's own depth cap
        // is what reports first here. The search layer refuses the same shape for
        // the token reason, and `pathfinder.rs` §34.10 tests that directly.
        let edges = [
            ab(),
            EdgeId::new(P2, B, A),
            EdgeId::new(P3, A, C),
            EdgeId::new(P4, C, A),
        ];
        assert!(CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).is_none());
    }

    #[test]
    fn a_repeated_intermediate_token_has_no_candidate() {
        // A -> B -> C -> B ends on a token the route already stood on. Three
        // distinct pools, inside the hop bound, so the token rule is the only
        // thing that can refuse it.
        let edges = [ab(), bc(), EdgeId::new(P3, C, B)];
        assert!(CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).is_none());
    }

    #[test]
    fn a_one_hop_self_loop_has_no_candidate() {
        assert!(
            CycleCandidate::assemble(CHAIN, BLOCK, &[EdgeId::new(P1, A, A)], FeeStatus::Complete)
                .is_none(),
            "a token traded against itself is not a market"
        );
    }

    #[test]
    fn four_hops_is_outside_the_bound() {
        let edges = [ab(), bc(), ca(), EdgeId::new(P1, A, B)];
        assert!(CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).is_none());
    }

    #[test]
    fn fees_are_either_all_proved_or_not() {
        let two = [ab(), EdgeId::new(P2, B, A)];
        let complete =
            CycleCandidate::assemble(CHAIN, BLOCK, &two, FeeStatus::Complete).expect("cycle");
        assert_eq!(complete.fee_status, FeeStatus::Complete);
        let incomplete =
            CycleCandidate::assemble(CHAIN, BLOCK, &two, FeeStatus::Incomplete).expect("cycle");
        assert_eq!(incomplete.fee_status, FeeStatus::Incomplete);
        assert_eq!(
            FeeStatus::Incomplete.merge(true),
            FeeStatus::Incomplete,
            "one unproved fee is enough for the whole route"
        );
    }

    #[test]
    fn a_two_hop_candidate_reads_from_its_smallest_edge() {
        let edges = [EdgeId::new(P2, B, A), ab()];
        let candidate =
            CycleCandidate::assemble(CHAIN, BLOCK, &edges, FeeStatus::Complete).expect("cycle");
        assert_eq!(
            candidate.canonical_key.edges(),
            [ab(), EdgeId::new(P2, B, A)]
        );
        assert_eq!(candidate.start_token, A);
        assert_eq!(candidate.edges.len(), 2);
        assert_eq!(candidate.hop_count, 2);
        assert_eq!(candidate.pools(), [P1, P2]);
        assert_eq!(candidate.tokens(), [A, B, A]);
        assert!(candidate.is_connected());
        assert_eq!(candidate.chain_id, CHAIN);
        assert_eq!(candidate.target_block, BLOCK);
    }
}
