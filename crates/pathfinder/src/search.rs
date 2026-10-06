//! The search: a bounded depth-first walk of one block's graph.
//!
//! Depth-first, not shortest-path, because the question is not "how do I get
//! there cheapest" but "which closed routes exist". Bellman-Ford and SPFA answer
//! a different question — the shortest path, and whether a negative cycle exists
//! somewhere — and neither one enumerates cycles, names them deterministically,
//! or refuses to reuse a pool. Dijkstra needs non-negative weights, and the only
//! weight this layer has is topology. So the search walks, and it stops when the
//! trail reaches `max_hops`.
//!
//! Three rules shape the walk, all of them §8–§9 of the task:
//!
//! - a pool may be entered once per cycle, because leaving and re-entering one
//!   constant-product pool is a fee donation with a name attached;
//! - a token may be stood on once before the route closes, because
//!   `A -> B -> A -> C -> A` is two cycles reported as one;
//! - only closed trails are reported, so `A -> B -> C` yields nothing until it
//!   comes back to `A`.
//!
//! The walk visits each cycle once per seat it can be entered at, and
//! [`CanonicalKey`] folds those seats together. Nothing here caps the number of
//! candidates: a search that stops after N findings reports a market it chose,
//! not the market it saw.

use std::collections::{BTreeMap, BTreeSet};

use evm_core::{PoolId, TokenId};
use evm_graph::{GraphEdge, GraphSnapshot};

use crate::candidate::{CanonicalKey, CycleCandidate, FeeStatus};
use crate::config::PathFinderConfig;
use crate::error::Result;

/// One search's answer, and how much walking it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathFinderRun {
    pub cycles: Vec<CycleCandidate>,
    /// Edge examinations the search performed, including the ones it refused.
    /// Diagnoses a search that is slow; never decides whether a cycle exists.
    pub states_visited: usize,
}

/// Every cycle candidate in one graph, under one bound.
///
/// The graph is borrowed, never cloned, and never written: a search that could
/// edit the market it reads could also invent one. Output is deterministic — the
/// same snapshot and the same config produce the same candidates in the same
/// order, because traversal follows `BTree` order through the graph and the
/// result is keyed, not appended.
pub fn find_cycles(
    graph: &GraphSnapshot,
    config: &PathFinderConfig,
) -> Result<Vec<CycleCandidate>> {
    Ok(find_cycles_traced(graph, config)?.cycles)
}

/// [`find_cycles`], plus the count of states it visited.
pub fn find_cycles_traced(
    graph: &GraphSnapshot,
    config: &PathFinderConfig,
) -> Result<PathFinderRun> {
    config.validate()?;
    let mut search = Search {
        graph,
        max_hops: config.max_hops,
        found: BTreeMap::new(),
        states: 0,
    };
    // `nodes()` yields token order, and the walk below only ever appends, so the
    // start token's order cannot leak into a candidate's identity — only into
    // which seat of a cycle it was first seen from.
    for start in graph.nodes() {
        let start = *start;
        let mut trail: Vec<GraphEdge> = Vec::with_capacity(search.max_hops);
        let mut pools: BTreeSet<PoolId> = BTreeSet::new();
        let mut tokens: BTreeSet<TokenId> = BTreeSet::new();
        tokens.insert(start);
        search.walk(start, start, &mut trail, &mut pools, &mut tokens);
    }
    Ok(PathFinderRun {
        // BTreeMap iteration is key order, and the key is the canonical rotation,
        // so this is the sort the task asks for without a second pass.
        cycles: search.found.into_values().collect(),
        states_visited: search.states,
    })
}

struct Search<'g> {
    graph: &'g GraphSnapshot,
    max_hops: usize,
    found: BTreeMap<CanonicalKey, CycleCandidate>,
    states: usize,
}

impl Search<'_> {
    /// Walk one step further from `current`, along a trail that started at
    /// `start` and has not yet closed.
    fn walk(
        &mut self,
        start: TokenId,
        current: TokenId,
        trail: &mut Vec<GraphEdge>,
        pools: &mut BTreeSet<PoolId>,
        tokens: &mut BTreeSet<TokenId>,
    ) {
        if trail.len() >= self.max_hops {
            return;
        }
        for next in self.graph.neighbors(current) {
            let next = *next;
            for edge in self.graph.routes(current, next) {
                self.states += 1;
                if pools.contains(&edge.pool()) {
                    continue;
                }
                if next == start {
                    // Closing the route. One hop from the start would be a token
                    // traded against itself, which is not a market; the depth
                    // floor is what the graph's own pair construction already
                    // guarantees, and this is where the search restates it.
                    if trail.len() + 1 >= PathFinderConfig::MIN_MAX_HOPS {
                        self.record(trail, edge);
                    }
                    continue;
                }
                if tokens.contains(&next) {
                    continue;
                }
                pools.insert(edge.pool());
                tokens.insert(next);
                trail.push(*edge);
                self.walk(start, next, trail, pools, tokens);
                trail.pop();
                tokens.remove(&next);
                pools.remove(&edge.pool());
            }
        }
    }

    /// Register a closed trail. Rotations collapse here, so a cycle found from
    /// three seats is stored once, under the key those seats agree on.
    fn record(&mut self, trail: &[GraphEdge], closing: &GraphEdge) {
        let mut edges: Vec<evm_graph::EdgeId> = Vec::with_capacity(trail.len() + 1);
        edges.extend(trail.iter().map(|edge| edge.id));
        edges.push(closing.id);
        let fee_status = trail
            .iter()
            .chain(std::iter::once(closing))
            .fold(FeeStatus::Complete, |status, edge| {
                status.merge(edge.fee.is_some())
            });
        let Some(candidate) = CycleCandidate::assemble(
            self.graph.chain_id(),
            self.graph.block_number(),
            &edges,
            fee_status,
        ) else {
            return;
        };
        self.found
            .entry(candidate.canonical_key.clone())
            .or_insert(candidate);
    }
}
