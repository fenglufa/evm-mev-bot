//! M9.2 §19/§31 — building a graph at an explicitly named target block.
//!
//! These are the negative controls for the one rule M9.2 adds: a pool whose
//! authoritative `Sync` is older than the target may price it *only* when the
//! snapshot carries a scan proving nothing happened in between. Every graph here
//! is built through `InMemoryStateStore` and `StateSnapshot::at_target`, i.e. the
//! same door the live and replay paths use, so what is under test is the decision
//! and not a hand-made snapshot.
//!
//! The pre-M9.2 rule — a pool prices the graph iff its state is in the very block
//! the snapshot was applied at — is asserted here too, both with and without a
//! named target, because nothing in M2/M5 changed and this file has to fail if it
//! ever does.

use std::collections::BTreeMap;

use alloy_primitives::{address, Address, U256};

use evm_core::{BlockNumber, ChainId, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId};
use evm_graph::{MarketGraphBuilder, SkipReason};
use evm_state::{
    InMemoryStateStore, StateSnapshot, StateStore, StateUpdate, SyncScanCoverage, UpdatePosition,
};

const CHAIN: ChainId = ChainId(91342);

const T0: Address = address!("0x00000000000000000000000000000000000000a0");
const T1: Address = address!("0x00000000000000000000000000000000000000b0");
const PA: Address = address!("0x0000000000000000000000000000000000000a11");
const PB: Address = address!("0x0000000000000000000000000000000000000b22");

const TARGET: BlockNumber = BlockNumber(120);

fn pool(addr: Address) -> PoolId {
    PoolId::new(CHAIN, addr)
}

fn meta(addr: Address, t0: Address, t1: Address) -> PoolMeta {
    PoolMeta {
        id: pool(addr),
        protocol: ProtocolId::new("v2-compatible"),
        token0: TokenId::new(CHAIN, t0),
        token1: TokenId::new(CHAIN, t1),
        fee: None,
        pool_type: PoolType::ConstantProduct,
    }
}

fn position(block: u64, log_index: u64) -> UpdatePosition {
    UpdatePosition::new(BlockNumber(block), LogIndex(log_index))
}

fn coverage(from: u64, through: u64) -> SyncScanCoverage {
    SyncScanCoverage::new(BlockNumber(from), BlockNumber(through)).expect("ordered range")
}

/// A store with both pools registered, so a sync of either is admissible.
fn store() -> InMemoryStateStore {
    let mut store = InMemoryStateStore::new(CHAIN);
    store
        .apply(StateUpdate::PoolRegistered(meta(PA, T0, T1)))
        .expect("register A");
    store
}

fn sync(
    store: &mut InMemoryStateStore,
    id: PoolId,
    reserve0: u128,
    reserve1: u128,
    at: UpdatePosition,
) {
    store
        .apply(StateUpdate::PoolSynced {
            pool: id,
            reserve0: U256::from(reserve0),
            reserve1: U256::from(reserve1),
            position: at,
        })
        .expect("sync admitted");
}

/// Register B as well, so a test that only needs A is not silently registering it.
fn register_b(store: &mut InMemoryStateStore) {
    store
        .apply(StateUpdate::PoolRegistered(meta(PB, T0, T1)))
        .expect("register B");
}

fn project(
    store: &InMemoryStateStore,
    target: u64,
    coverages: BTreeMap<PoolId, SyncScanCoverage>,
) -> StateSnapshot {
    store.snapshot().at_target(BlockNumber(target), coverages)
}

fn traced(snapshot: &StateSnapshot) -> evm_graph::GraphBuild {
    MarketGraphBuilder::new()
        .build_traced(snapshot)
        .expect("the snapshot has an applied position")
}

fn skipped_pools(
    build: &evm_graph::GraphBuild,
) -> Vec<(Address, SkipReason, Option<UpdatePosition>)> {
    build
        .skipped
        .iter()
        .map(|row| (row.pool.address, row.reason, row.state_position))
        .collect()
}

// ── §16 regression guard: naming no target changes nothing ──────────────────

#[test]
fn without_a_named_target_only_the_applied_block_prices_the_graph() {
    let mut store = store();
    register_b(&mut store);
    sync(&mut store, pool(PA), 100, 200, position(100, 1));
    sync(&mut store, pool(PB), 300, 400, position(120, 2));

    let snapshot = store.snapshot();
    assert_eq!(snapshot.target_block(), None);
    let build = traced(&snapshot);
    assert_eq!(build.graph.block_number(), BlockNumber(120));
    assert_eq!(build.graph.pool_count(), 1);
    assert_eq!(
        skipped_pools(&build),
        vec![(PA, SkipReason::NotAtTargetBlock, Some(position(100, 1)))]
    );
    // A pool scanned up to the target is still not admitted here: without a named
    // target the builder reads the snapshot at its applied position, which is the
    // only contract M2/M5 ever had, and attaching evidence is a caller's choice.
    let evidenced = project(&store, 120, [(pool(PA), coverage(100, 120))].into());
    assert_eq!(evidenced.target_block(), Some(BlockNumber(120)));
    assert_eq!(traced(&evidenced).graph.pool_count(), 2);
}

// ── NC3 — an older Sync, proven unchanged, is the target-block state ─────────

#[test]
fn a_proven_unchanged_older_sync_prices_the_target_block() {
    let mut store = store();
    sync(&mut store, pool(PA), 100, 200, position(100, 1));

    let build = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 120))].into(),
    ));
    assert_eq!(build.graph.block_number(), TARGET);
    assert_eq!(build.graph.pool_count(), 1);
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);

    let edges = build.graph.pool_edges(pool(PA));
    assert_eq!(edges.len(), 2);
    for edge in &edges {
        // The graph is at 120; the edge still names the log it came from. Rewriting
        // the position to the target would have destroyed the only provenance the
        // evidence has to be audited against.
        assert_eq!(edge.state_position, position(100, 1));
        assert_eq!(edge.reserve_in.max(edge.reserve_out), U256::from(200u128));
    }
}

// ── NC6 — no proof, no entry ────────────────────────────────────────────────

#[test]
fn an_unscanned_gap_keeps_the_pool_out_of_the_target_graph() {
    let mut store = store();
    sync(&mut store, pool(PA), 100, 200, position(100, 1));

    // No scan at all.
    let build = traced(&project(&store, 120, BTreeMap::new()));
    assert!(build.graph.is_empty());
    assert_eq!(
        skipped_pools(&build),
        vec![(PA, SkipReason::NotAtTargetBlock, Some(position(100, 1)))]
    );

    // A scan that stops one block short of the target: 121 could hold a Sync.
    let short = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 119))].into(),
    ));
    assert_eq!(short.skipped.len(), 1);
    assert!(short.graph.is_empty());

    // A scan that starts after the state it vouches for leaves (100, 110] unlooked
    // at, which is the same gap wearing a wider hat.
    let gapped = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(110, 120))].into(),
    ));
    assert_eq!(
        gapped.skipped[0].reason,
        SkipReason::NotAtTargetBlock,
        "a scan that misses the interval after the state is not evidence"
    );
}

// ── NC2 — a pool nobody ever synced is unavailable, evidence or not ─────────

#[test]
fn coverage_alone_cannot_invent_a_price() {
    // PA is registered and never synced; the scan covers the whole range anyway.
    // PB carries the store's applied position, so this snapshot has a block to
    // build at and PA's absence is about PA, not about a missing block identity.
    let mut store = store();
    register_b(&mut store);
    sync(&mut store, pool(PB), 7, 8, position(120, 1));

    let build = traced(&project(&store, 120, [(pool(PA), coverage(1, 120))].into()));
    assert_eq!(build.graph.pool_count(), 1);
    assert_eq!(
        skipped_pools(&build),
        vec![(PA, SkipReason::StateUnavailable, None)]
    );
}

// ── NC1 + NC5 — a Sync after the target ─────────────────────────────────────

#[test]
fn a_sync_after_the_target_never_prices_it() {
    let mut store = store();
    sync(&mut store, pool(PA), 999, 999, position(121, 1));

    // Wide coverage, target one block before the only Sync that exists.
    let build = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 200))].into(),
    ));
    assert!(
        build.graph.is_empty(),
        "block 121's reserves cannot price block 120"
    );
    assert_eq!(
        skipped_pools(&build),
        vec![(PA, SkipReason::NotAtTargetBlock, Some(position(121, 1)))]
    );

    // Ask for 121 and the same pool is in — the rejection is about the target, not
    // about the pool.
    let later = traced(&project(
        &store,
        121,
        [(pool(PA), coverage(100, 200))].into(),
    ));
    assert_eq!(later.graph.pool_count(), 1);
    assert_eq!(later.graph.block_number(), BlockNumber(121));
}

#[test]
fn a_later_sync_does_not_move_an_earlier_graph() {
    // The §26 shape: one store, two targets. The pool whose state predates both is
    // in only where the scan proves it, and the pool synced at 121 is invisible at
    // 120 no matter how the store grew.
    let mut store = store();
    register_b(&mut store);
    sync(&mut store, pool(PA), 100, 200, position(100, 1));
    sync(&mut store, pool(PB), 500, 600, position(121, 4));

    let at_120 = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 120))].into(),
    ));
    assert_eq!(at_120.graph.pool_count(), 1);
    assert_eq!(at_120.graph.block_number(), TARGET);
    for edge in at_120.graph.edges() {
        assert!(edge.state_position.block_number <= TARGET);
        assert_ne!(edge.pool(), pool(PB), "block 121 leaked into block 120");
    }

    // Evidence is read against the target asked for, never carried forward by
    // habit: PA's scan stops at 120, so one block later PA is unproven again.
    let at_121 = traced(&project(
        &store,
        121,
        [
            (pool(PA), coverage(100, 120)),
            (pool(PB), coverage(121, 121)),
        ]
        .into(),
    ));
    assert_eq!(at_121.graph.pool_count(), 1);
    assert_eq!(at_121.graph.block_number(), BlockNumber(121));
    assert_eq!(
        skipped_pools(&at_121),
        vec![(PA, SkipReason::NotAtTargetBlock, Some(position(100, 1)))]
    );

    // Extend PA's own scan to the new target and it is proven there too — the same
    // store, the same state log, a wider look at the blocks after it.
    let both = traced(&project(
        &store,
        121,
        [
            (pool(PA), coverage(100, 121)),
            (pool(PB), coverage(121, 121)),
        ]
        .into(),
    ));
    assert_eq!(both.graph.pool_count(), 2);
    assert!(both.skipped.is_empty(), "{:?}", both.skipped);
}

// ── NC4 — several Syncs in one block ────────────────────────────────────────

#[test]
fn the_final_sync_of_a_block_is_the_one_that_prices_it() {
    let mut store = store();
    sync(&mut store, pool(PA), 11, 21, position(100, 150));
    sync(&mut store, pool(PA), 12, 22, position(100, 174));
    sync(&mut store, pool(PA), 13, 23, position(100, 181));

    let build = traced(&project(
        &store,
        100,
        [(pool(PA), coverage(100, 100))].into(),
    ));
    let edges = build.graph.pool_edges(pool(PA));
    assert_eq!(edges.len(), 2);
    assert_eq!(edges[0].state_position, position(100, 181));
    assert_eq!(
        edges[0].reserve_in,
        U256::from(13u128),
        "an earlier log in the same block beat the pool's last statement"
    );

    // Projected forward, the same rule holds: the last word in block 100 is the
    // state carried to 120, provided the scan reaches 120.
    let forward = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 120))].into(),
    ));
    let forward_edges = forward.graph.pool_edges(pool(PA));
    assert_eq!(forward_edges[0].state_position, position(100, 181));
    assert_eq!(forward.graph.block_number(), TARGET);
}

// ── §15 / §27 — one graph, one block, no dependence on order ────────────────

#[test]
fn a_target_graph_never_mixes_unproven_moments() {
    // A at 100 proven through 120; B at 110 with no scan; C at 120 observed there.
    let mut store = InMemoryStateStore::new(CHAIN);
    let pc = address!("0x0000000000000000000000000000000000000c33");
    for addr in [PA, PB, pc] {
        store
            .apply(StateUpdate::PoolRegistered(meta(addr, T0, T1)))
            .expect("register");
    }
    sync(&mut store, pool(PA), 1, 2, position(100, 1));
    sync(&mut store, pool(PB), 3, 4, position(110, 2));
    sync(&mut store, pool(pc), 5, 6, position(120, 3));

    let build = traced(&project(
        &store,
        120,
        [(pool(PA), coverage(100, 120))].into(),
    ));
    let mut pools_in_graph: Vec<Address> = build
        .graph
        .edges()
        .map(|edge| edge.pool().address)
        .collect::<Vec<_>>();
    pools_in_graph.sort();
    pools_in_graph.dedup();
    assert_eq!(
        pools_in_graph,
        vec![PA, pc],
        "B has a state at 110 and no scan; it may not stand in for block 120"
    );
    assert_eq!(
        skipped_pools(&build),
        vec![(PB, SkipReason::NotAtTargetBlock, Some(position(110, 2)))]
    );
}

#[test]
fn the_same_inputs_build_the_same_graph_and_the_same_accounting() {
    let mut store = store();
    register_b(&mut store);
    sync(&mut store, pool(PA), 100, 200, position(100, 1));
    sync(&mut store, pool(PB), 300, 400, position(110, 2));
    let coverages: BTreeMap<PoolId, SyncScanCoverage> = [
        (pool(PA), coverage(100, 120)),
        (pool(PB), coverage(110, 119)),
    ]
    .into();

    let first = traced(&project(&store, 120, coverages.clone()));
    let second = traced(&project(&store, 120, coverages));
    assert_eq!(first, second);
    // Skips come out ordered by pool identity, so the accounting table two runs
    // write is the same table, not the same set in a different order.
    let order: Vec<Address> = first.skipped.iter().map(|row| row.pool.address).collect();
    assert_eq!(order, vec![PB]);
    assert_eq!(first.graph.block_number(), TARGET);
}
