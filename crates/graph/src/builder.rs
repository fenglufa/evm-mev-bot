use std::collections::BTreeSet;

use serde::Serialize;

use evm_core::{BlockNumber, ChainId, PoolId};
use evm_state::{StateSnapshot, UpdatePosition};

use crate::edge::{EdgeRejection, GraphEdge};
use crate::snapshot::GraphSnapshot;

/// Why a pool the store knows about did not become an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SkipReason {
    /// The pool is registered but no authoritative state event has priced it
    /// yet. Inventing a reserve here — zero, or a fresh `eth_call` — would put a
    /// number in the graph that nobody observed.
    StateUnavailable,
    /// The pool has state, but it is not from the block this graph is taken at.
    /// A snapshot of block N holding one pool's price from N-100 and another's
    /// from N+1 is not a market; it is a comparison of three moments, and a
    /// route found across it would be an artifact of mixing them.
    NotAtTargetBlock,
    /// The pool has state, but it cannot price a pair.
    Rejected(EdgeRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SkippedPool {
    pub pool: PoolId,
    pub reason: SkipReason,
    /// What the store's own record says, kept so the reason can be audited
    /// without re-reading the store.
    pub state_position: Option<UpdatePosition>,
}

/// The graph plus everything the state layer offered and the graph declined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphBuild {
    pub graph: GraphSnapshot,
    pub skipped: Vec<SkippedPool>,
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error(
        "state snapshot carries no applied position, so there is no block to build a graph at"
    )]
    NoBlockIdentity,
}

/// Projects a `StateSnapshot` onto tokens and pools.
///
/// The builder reads no chain: it is a pure consumer of the state layer, so a
/// graph can never disagree with the reserves the replay produced, and the same
/// snapshot always yields the same graph.
#[derive(Clone, Copy, Debug, Default)]
pub struct MarketGraphBuilder;

impl MarketGraphBuilder {
    pub fn new() -> Self {
        Self
    }

    pub fn build(&self, snapshot: &StateSnapshot) -> Result<GraphSnapshot, GraphError> {
        Ok(self.build_traced(snapshot)?.graph)
    }

    /// `build`, but also reporting which registered pools were left out and why.
    ///
    /// The target block is the snapshot's own applied position, and every edge
    /// has to come from that exact block: a graph mixing one pool's price from
    /// block N-100 with another's from N would describe no market that ever
    /// existed. A pool whose latest state is older than the target is skipped
    /// with the position it actually holds, so the gap is auditable.
    pub fn build_traced(&self, snapshot: &StateSnapshot) -> Result<GraphBuild, GraphError> {
        let position = snapshot.position.ok_or(GraphError::NoBlockIdentity)?;
        let block_number: BlockNumber = position.block_number;
        let chain_id: ChainId = snapshot.chain_id;
        let mut edges = BTreeSet::new();
        let mut skipped = Vec::new();
        for (id, record) in snapshot.pools() {
            match record.state {
                Some(state) => {
                    let state_position = UpdatePosition::new(state.block_number, state.log_index);
                    if state.block_number != block_number {
                        skipped.push(SkippedPool {
                            pool: *id,
                            reason: SkipReason::NotAtTargetBlock,
                            state_position: Some(state_position),
                        });
                        continue;
                    }
                    match GraphEdge::pair(&record.meta, &state) {
                        Ok(pair) => {
                            edges.extend(pair);
                        }
                        Err(rejection) => skipped.push(SkippedPool {
                            pool: *id,
                            reason: SkipReason::Rejected(rejection),
                            state_position: Some(state_position),
                        }),
                    }
                }
                None => skipped.push(SkippedPool {
                    pool: *id,
                    reason: SkipReason::StateUnavailable,
                    state_position: None,
                }),
            }
        }
        skipped.sort_by_key(|skipped| skipped.pool);
        Ok(GraphBuild {
            graph: GraphSnapshot::assemble(chain_id, block_number, edges),
            skipped,
        })
    }
}
