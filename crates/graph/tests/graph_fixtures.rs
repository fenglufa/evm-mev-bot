//! M2 fixtures: the six shapes the graph has to get right, plus determinism.
//!
//! Every graph here is built from an `InMemoryStateStore` — the same store the
//! replay feeds — so these tests exercise the real path, not a hand-made
//! snapshot.

use std::collections::BTreeSet;

use alloy_primitives::{address, Address, U256};

use evm_core::{BlockNumber, ChainId, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId};
use evm_graph::{EdgeId, GraphError, MarketGraphBuilder, SkipReason, SkippedPool};
use evm_state::{InMemoryStateStore, StateStore, StateUpdate, UpdatePosition};

const CHAIN: ChainId = ChainId(7);

const A: Address = address!("0x000000000000000000000000000000000000000a");
const B: Address = address!("0x000000000000000000000000000000000000000b");
const C: Address = address!("0x000000000000000000000000000000000000000c");
const P1: Address = address!("0x00000000000000000000000000000000000000f1");
const P2: Address = address!("0x00000000000000000000000000000000000000f2");

fn token(chain: ChainId, addr: Address) -> TokenId {
    TokenId::new(chain, addr)
}

fn pool(chain: ChainId, addr: Address) -> PoolId {
    PoolId::new(chain, addr)
}

fn meta(chain: ChainId, pool_addr: Address, t0: Address, t1: Address) -> PoolMeta {
    PoolMeta {
        id: pool(chain, pool_addr),
        protocol: ProtocolId::new("test-v2"),
        token0: token(chain, t0),
        token1: token(chain, t1),
        fee: None,
        pool_type: PoolType::ConstantProduct,
    }
}

fn position(block: u64, log_index: u64) -> UpdatePosition {
    UpdatePosition::new(BlockNumber(block), LogIndex(log_index))
}

fn synced(pool_id: PoolId, reserve0: u128, reserve1: u128, at: UpdatePosition) -> StateUpdate {
    StateUpdate::PoolSynced {
        pool: pool_id,
        reserve0: U256::from(reserve0),
        reserve1: U256::from(reserve1),
        position: at,
    }
}

/// Register and sync in one go, so the block positions in these tests stay
/// monotonic — the store refuses a regression, and so should the graph.
struct Setup {
    store: InMemoryStateStore,
    next: u64,
}

fn setup(chain: ChainId) -> Setup {
    Setup {
        store: InMemoryStateStore::new(chain),
        next: 1,
    }
}

impl Setup {
    fn register(&mut self, m: PoolMeta) -> &mut Self {
        self.store
            .apply(StateUpdate::PoolRegistered(m))
            .expect("registration");
        self
    }

    fn sync(&mut self, id: PoolId, r0: u128, r1: u128) -> &mut Self {
        let at = position(100, self.next);
        self.next += 1;
        self.store
            .apply(synced(id, r0, r1, at))
            .expect("sync admitted");
        self
    }

    /// Sync at an exact position. The store refuses a position that moves
    /// backwards, so these calls stay in increasing block order.
    fn sync_at(&mut self, id: PoolId, r0: u128, r1: u128, at: UpdatePosition) -> &mut Self {
        self.store
            .apply(synced(id, r0, r1, at))
            .expect("sync admitted");
        self
    }

    fn build(&self) -> evm_graph::GraphSnapshot {
        MarketGraphBuilder::new()
            .build(&self.store.snapshot())
            .expect("snapshot has a block")
    }
}

// --- Fixture A: one pool --------------------------------------------------

#[test]
fn one_pool_is_two_nodes_and_two_edges() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .sync(pool(CHAIN, P1), 100, 200);
    let graph = s.build();

    assert_eq!(graph.chain_id(), CHAIN);
    assert_eq!(graph.block_number(), BlockNumber(100));
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 2);
    assert_eq!(graph.pool_count(), 1);
    assert_eq!(
        graph.nodes().copied().collect::<Vec<_>>(),
        vec![token(CHAIN, A), token(CHAIN, B)]
    );

    // The two directions of one pool share its PoolId and its state position.
    let edges = graph.pool_edges(pool(CHAIN, P1));
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0].pool(), edges[1].pool());
    assert_eq!(edges[0].state_position, edges[1].state_position);
    assert_eq!(
        (edges[0].reserve_in, edges[0].reserve_out),
        (U256::from(100u128), U256::from(200u128))
    );
    assert_eq!(
        (edges[1].reserve_in, edges[1].reserve_out),
        (U256::from(200u128), U256::from(100u128))
    );
}

// --- Fixture B: two pools on one pair -------------------------------------

#[test]
fn two_pools_on_one_pair_are_four_edges_and_never_overwrite_each_other() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .register(meta(CHAIN, P2, A, B))
        .sync(pool(CHAIN, P1), 100, 200)
        .sync(pool(CHAIN, P2), 300, 400);
    let graph = s.build();

    assert_eq!(graph.node_count(), 2, "one pair is still one pair");
    assert_eq!(graph.edge_count(), 4);
    assert_eq!(graph.pool_count(), 2);
    assert_eq!(graph.routes(token(CHAIN, A), token(CHAIN, B)).len(), 2);
    assert_eq!(graph.routes(token(CHAIN, B), token(CHAIN, A)).len(), 2);

    let pools: BTreeSet<PoolId> = graph
        .routes(token(CHAIN, A), token(CHAIN, B))
        .iter()
        .map(|edge| edge.id.pool)
        .collect();
    assert_eq!(
        pools,
        [pool(CHAIN, P1), pool(CHAIN, P2)].into_iter().collect()
    );

    // Each route carries its own pool's reserves — the second pool did not
    // overwrite the first, and the two are nameable apart.
    let p1 = graph
        .edge(EdgeId::new(
            pool(CHAIN, P1),
            token(CHAIN, A),
            token(CHAIN, B),
        ))
        .expect("p1 route present");
    let p2 = graph
        .edge(EdgeId::new(
            pool(CHAIN, P2),
            token(CHAIN, A),
            token(CHAIN, B),
        ))
        .expect("p2 route present");
    assert_eq!(p1.reserve_in, U256::from(100u128));
    assert_eq!(p2.reserve_in, U256::from(300u128));
}

// --- Fixture C: three token chain -----------------------------------------

#[test]
fn a_three_token_chain_reports_neighbors_in_both_directions() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .register(meta(CHAIN, P2, B, C))
        .sync(pool(CHAIN, P1), 100, 200)
        .sync(pool(CHAIN, P2), 300, 400);
    let graph = s.build();

    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 4);
    assert_eq!(
        graph.neighbors(token(CHAIN, A)),
        [token(CHAIN, B)],
        "A only reaches B"
    );
    assert_eq!(
        graph.neighbors(token(CHAIN, B)),
        [token(CHAIN, A), token(CHAIN, C)],
        "B reaches both, in id order"
    );
    assert_eq!(graph.neighbors(token(CHAIN, C)), [token(CHAIN, B)]);
    assert!(graph.neighbors(token(CHAIN, P1)).is_empty());
}

// --- Fixture D: one address on two chains ----------------------------------

#[test]
fn the_same_address_on_two_chains_is_two_graphs() {
    let mut left = setup(ChainId(1));
    left.register(meta(ChainId(1), P1, A, B))
        .sync(pool(ChainId(1), P1), 100, 200);
    let mut right = setup(ChainId(2));
    right
        .register(meta(ChainId(2), P1, A, B))
        .sync(pool(ChainId(2), P1), 900, 800);

    let left_graph = left.build();
    let right_graph = right.build();

    assert_ne!(left_graph.chain_id(), right_graph.chain_id());
    assert!(left_graph
        .nodes()
        .zip(right_graph.nodes())
        .all(|(l, r)| l != r));
    // A node from the other chain is simply not in this graph.
    assert!(left_graph.neighbors(token(ChainId(2), A)).is_empty());
    assert_eq!(
        left_graph
            .edge(EdgeId::new(
                pool(ChainId(1), P1),
                token(ChainId(1), A),
                token(ChainId(1), B)
            ))
            .expect("own chain edge")
            .reserve_in,
        U256::from(100u128)
    );
    assert!(left_graph
        .edge(EdgeId::new(
            pool(ChainId(2), P1),
            token(ChainId(2), A),
            token(ChainId(2), B)
        ))
        .is_none());
}

// --- Fixture E: a registered pool with no state ----------------------------

#[test]
fn a_pool_without_state_is_skipped_not_priced_at_zero() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .register(meta(CHAIN, P2, B, C))
        .sync(pool(CHAIN, P1), 100, 200);
    // P2 is registered but has never emitted an authoritative state event.

    let build = MarketGraphBuilder::new()
        .build_traced(&s.store.snapshot())
        .expect("snapshot has a block");

    assert_eq!(
        build.graph.edge_count(),
        2,
        "only the synced pool is a market"
    );
    assert_eq!(build.graph.node_count(), 2);
    assert!(build.graph.pool_edges(pool(CHAIN, P2)).is_empty());
    assert_eq!(
        build.skipped,
        vec![SkippedPool {
            pool: pool(CHAIN, P2),
            reason: SkipReason::StateUnavailable,
            state_position: None,
        }],
        "a missing state is reported, not invented"
    );
    assert!(build
        .graph
        .edges()
        .all(|edge| edge.reserve_in != U256::ZERO && edge.reserve_out != U256::ZERO));
}

// --- §25: one graph, one block ---------------------------------------------

/// A pool whose newest state is older than the target block is not a market at
/// that block. Mixing it in would report a graph that never existed.
#[test]
fn a_pool_stale_at_the_target_block_is_skipped_not_mixed_in() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P2, A, C))
        .sync_at(pool(CHAIN, P2), 500, 600, position(90, 1))
        .register(meta(CHAIN, P1, A, B))
        .sync(pool(CHAIN, P1), 100, 200);
    // The snapshot is therefore bound to block 100, while P2's last state is a
    // block behind.

    let build = MarketGraphBuilder::new()
        .build_traced(&s.store.snapshot())
        .expect("snapshot has a block");

    assert_eq!(build.graph.block_number(), BlockNumber(100));
    assert_eq!(
        build.graph.pool_count(),
        1,
        "only the in-block pool is priced"
    );
    assert_eq!(build.graph.edge_count(), 2);
    assert_eq!(build.graph.node_count(), 2);
    assert!(
        !build.graph.nodes().any(|n| *n == token(CHAIN, C)),
        "a token that only the stale pool trades is not a node"
    );
    assert_eq!(
        build.skipped,
        vec![SkippedPool {
            pool: pool(CHAIN, P2),
            reason: SkipReason::NotAtTargetBlock,
            state_position: Some(position(90, 1)),
        }],
        "and the skip names the block its state really came from"
    );
}

/// The stale pool's last sync is still recorded, so a graph bound to that block
/// does hold it — the pool is not lost, only kept out of the wrong block.
#[test]
fn the_stale_pool_has_its_own_graph_at_its_own_block() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P2, A, C))
        .sync_at(pool(CHAIN, P2), 500, 600, position(90, 1));
    let graph = s.build();
    assert_eq!(graph.block_number(), BlockNumber(90));
    assert_eq!(graph.pool_count(), 1);
    assert_eq!(graph.neighbors(token(CHAIN, A)), [token(CHAIN, C)]);
}

// --- Fixture F: a pool whose state cannot price a pair ---------------------

/// The store itself refuses empty reserves, so the graph can only ever be handed
/// a state that passed that check. This fixture is the second line of that
/// defence: a pair the store will accept metadata for, but which cannot be a
/// route.
#[test]
fn a_degenerate_pair_is_rejected_and_reported() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, A))
        .sync(pool(CHAIN, P1), 100, 200);

    let build = MarketGraphBuilder::new()
        .build_traced(&s.store.snapshot())
        .expect("snapshot has a block");
    assert!(build.graph.is_empty());
    assert_eq!(build.graph.node_count(), 0);
    assert_eq!(
        build.skipped,
        vec![SkippedPool {
            pool: pool(CHAIN, P1),
            reason: SkipReason::Rejected(evm_graph::EdgeRejection::SameToken),
            state_position: Some(position(100, 1)),
        }]
    );
}

#[test]
fn empty_reserves_never_reach_the_graph() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .sync(pool(CHAIN, P1), 100, 200);
    // The store refuses a zero-reserve sync outright...
    let rejected = s
        .store
        .apply(synced(pool(CHAIN, P1), 0, 200, position(100, 9)));
    assert!(rejected.is_err(), "the state layer admits no empty side");
    // ...so the graph keeps the last real reserves instead of dropping to zero.
    let graph = s.build();
    assert_eq!(
        graph
            .edge(EdgeId::new(
                pool(CHAIN, P1),
                token(CHAIN, A),
                token(CHAIN, B)
            ))
            .expect("edge")
            .reserve_in,
        U256::from(100u128)
    );
}

// --- A graph needs a block ------------------------------------------------

#[test]
fn a_snapshot_with_no_position_has_no_graph() {
    let store = InMemoryStateStore::new(CHAIN);
    let err = MarketGraphBuilder::new()
        .build(&store.snapshot())
        .expect_err("nothing has been applied");
    assert!(matches!(err, GraphError::NoBlockIdentity), "{err:?}");
}

#[test]
fn a_registered_but_never_synced_registry_builds_no_graph() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B));
    // Metadata alone gives no block position: there is nothing observed to bind.
    let err = MarketGraphBuilder::new()
        .build(&s.store.snapshot())
        .expect_err("no applied position");
    assert!(matches!(err, GraphError::NoBlockIdentity));
    let err = MarketGraphBuilder::new()
        .build_traced(&s.store.snapshot())
        .expect_err("metadata alone gives no block to bind");
    assert!(matches!(err, GraphError::NoBlockIdentity));
}

// --- Determinism and serialization ----------------------------------------

#[test]
fn the_same_state_snapshot_always_builds_the_same_graph() {
    let mut first = setup(CHAIN);
    first
        .register(meta(CHAIN, P1, A, B))
        .register(meta(CHAIN, P2, B, C));
    let mut second = setup(CHAIN);
    // Registration order reversed on purpose: the graph must not inherit it.
    second
        .register(meta(CHAIN, P2, B, C))
        .register(meta(CHAIN, P1, A, B));
    for s in [&mut first, &mut second] {
        s.sync(pool(CHAIN, P1), 100, 200);
        s.sync(pool(CHAIN, P2), 300, 400);
    }

    let a = first.build();
    let b = second.build();
    assert_eq!(a, b);
    assert_eq!(
        MarketGraphBuilder::new()
            .build(&first.store.snapshot())
            .unwrap(),
        a,
        "building twice from one snapshot gives one graph"
    );

    let json_a = serde_json::to_string(&a).expect("serialize");
    let json_b = serde_json::to_string(&b).expect("serialize");
    assert_eq!(json_a, json_b, "identical inputs, identical JSON");

    let value: serde_json::Value = serde_json::from_str(&json_a).expect("round trip");
    assert_eq!(value["chain_id"], 7);
    assert_eq!(value["block_number"], 100);
    assert_eq!(value["nodes"].as_array().expect("nodes").len(), 3);
    assert_eq!(value["edges"].as_array().expect("edges").len(), 4);
    // The serialized edge order is the canonical one, not an artifact.
    let ids: Vec<String> = value["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| serde_json::to_string(e).expect("edge json"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn edge_order_is_canonical_so_routes_and_neighbors_are_reproducible() {
    let mut s = setup(CHAIN);
    s.register(meta(CHAIN, P1, A, B))
        .register(meta(CHAIN, P2, A, B))
        .sync(pool(CHAIN, P1), 100, 200)
        .sync(pool(CHAIN, P2), 300, 400);
    let graph = s.build();
    let a_b = graph.routes(token(CHAIN, A), token(CHAIN, B));
    assert!(
        a_b.windows(2).all(|w| w[0].id < w[1].id),
        "routes come out in edge-id order"
    );
    assert_eq!(a_b[0].pool(), pool(CHAIN, P1));
    assert_eq!(a_b[1].pool(), pool(CHAIN, P2));
}
