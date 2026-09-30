use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use evm_core::{BlockNumber, ChainId, PoolId, TokenId};

use crate::edge::{EdgeId, GraphEdge};

/// The market as one exact block left it.
///
/// There is no such thing as an un-blocked "current graph": the same two tokens
/// can price completely differently one block later, so a graph carries the
/// block it was built at and every edge carries the position of the state event
/// behind it. A later state produces a new snapshot, never an edit of this one —
/// which is why every collection here is private and every accessor is shared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphSnapshot {
    chain_id: ChainId,
    block_number: BlockNumber,
    nodes: BTreeSet<TokenId>,
    edges: BTreeSet<GraphEdge>,
    /// `token_in -> token_out -> every pool offering that route`. Plural on
    /// purpose: two pools on one pair are two markets, and M3 has to see both.
    ///
    /// Derived from `edges`, so it is not serialized; the edge set is the graph.
    #[serde(skip)]
    routes: BTreeMap<(TokenId, TokenId), Vec<GraphEdge>>,
    #[serde(skip)]
    peers: BTreeMap<TokenId, Vec<TokenId>>,
}

impl GraphSnapshot {
    /// Indexes the edge set. `edges` arrives ordered by edge identity, so every
    /// derived list is ordered too and nothing here depends on hash order.
    pub(crate) fn assemble(
        chain_id: ChainId,
        block_number: BlockNumber,
        edges: BTreeSet<GraphEdge>,
    ) -> Self {
        let mut nodes = BTreeSet::new();
        let mut routes: BTreeMap<(TokenId, TokenId), Vec<GraphEdge>> = BTreeMap::new();
        for edge in &edges {
            nodes.insert(edge.token_in());
            nodes.insert(edge.token_out());
            routes
                .entry((edge.token_in(), edge.token_out()))
                .or_default()
                .push(*edge);
        }
        let mut peers: BTreeMap<TokenId, Vec<TokenId>> = BTreeMap::new();
        for (token_in, token_out) in routes.keys() {
            let list = peers.entry(*token_in).or_default();
            if list.last() != Some(token_out) {
                list.push(*token_out);
            }
        }
        Self {
            chain_id,
            block_number,
            nodes,
            edges,
            routes,
            peers,
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// The block this view is taken at: the state snapshot's applied position.
    /// Every edge here was priced in exactly this block — a pool whose newest
    /// state is older is skipped and reported, never carried along.
    pub fn block_number(&self) -> BlockNumber {
        self.block_number
    }

    pub fn nodes(&self) -> impl Iterator<Item = &TokenId> {
        self.nodes.iter()
    }

    pub fn edges(&self) -> impl Iterator<Item = &GraphEdge> {
        self.edges.iter()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn pool_count(&self) -> usize {
        let pools: BTreeSet<PoolId> = self.edges.iter().map(GraphEdge::pool).collect();
        pools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Everything a token can be spent into, sorted and deterministic.
    pub fn neighbors(&self, token: TokenId) -> &[TokenId] {
        self.peers.get(&token).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every pool that routes `token_in -> token_out`, in edge-identity order.
    pub fn routes(&self, token_in: TokenId, token_out: TokenId) -> &[GraphEdge] {
        self.routes
            .get(&(token_in, token_out))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Both directions of one pool, if the pool is in this graph.
    pub fn pool_edges(&self, pool: PoolId) -> Vec<GraphEdge> {
        self.edges
            .iter()
            .filter(|edge| edge.pool() == pool)
            .copied()
            .collect()
    }

    pub fn edge(&self, id: EdgeId) -> Option<GraphEdge> {
        self.edges.iter().find(|edge| edge.id == id).copied()
    }
}
