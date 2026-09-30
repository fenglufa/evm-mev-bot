//! M2 real-data acceptance: a captured block of chain 91342 becomes a market
//! graph, and every number in that graph is traceable to a log in the file.
//!
//! The path under test is the whole chain of custody —
//! `captured block -> ChainLog -> ProtocolEvent -> StateUpdate -> PoolState ->
//! GraphEdge -> GraphSnapshot` — with the registry file as the only source of
//! pool identity. Nothing here asks the node for state: the graph is built from
//! logs that were already on the chain.

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address, U256};

use evm_chain::RecordedChainAdapter;
use evm_core::{BlockNumber, ChainId, LogIndex, PoolId, TokenId};
use evm_graph::{EdgeId, GraphSnapshot, MarketGraphBuilder, SkipReason, SkippedPool};
use evm_protocol::{Registry, V2Adapter};
use evm_replay::ReplayEngine;
use evm_state::{InMemoryStateStore, UpdatePosition};

const CHAIN: ChainId = ChainId(91342);

/// The block the graph binds to: both pools that trade in the captured corpus
/// sync here. `0x3978e57b..` at global log 30 and `0xcaafb95f..` at global log
/// 50 — read out of the captured file, not assumed.
const REAL_BLOCK: u64 = 37_258_093;
const BLOCK_BEFORE: u64 = 37_257_255;

/// Every block these tests replay, in chain order. Two pools are created and
/// first synced in 5455035 and 31390683, two more in 5457650 and 10544346 and
/// never traded in again, and the last two blocks carry the live trading.
const REAL_BLOCKS: [u64; 6] = [
    5_455_035,
    5_457_650,
    10_544_346,
    31_390_683,
    BLOCK_BEFORE,
    REAL_BLOCK,
];

const POOL_GIWA: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");
const POOL_NARU: Address = address!("0xcaafb95fc292c10a526f03fa480407bb438dac67");
/// Attested from its own `PairCreated`, synced once in 5457650 and never again.
const POOL_GIWA_STALE: Address = address!("0x8df9062fc2995b06de754c040f1f7eac411abbdb");
/// Attested from its own `PairCreated`, synced once in 10544346 and never again.
const POOL_GIWA_USDT: Address = address!("0x4db758ab5e494d5b81627dd39258240aa9db9d46");
const ALL_POOLS: [Address; 4] = [POOL_GIWA, POOL_GIWA_STALE, POOL_GIWA_USDT, POOL_NARU];

const TOKEN_GIWAP: Address = address!("0x304912af0ce0dd6479735634d567715107bdc0c6");
const TOKEN_HYANGNO: Address = address!("0x0274c57358c3a6b8a08b7297ba51665c303a471f");
const TOKEN_WETH: Address = address!("0x4200000000000000000000000000000000000006");
const TOKEN_USDT: Address = address!("0xfa0d1d1703b55929e262d9301a001d30e45b97a2");

/// An emitter on this chain that shares the `Sync` topic0 but is not a verified
/// pool, so it must never appear in the graph.
const LOOKALIKE: Address = address!("0xad153c844ccac3d2ea991170624200e54730be74");

/// The reserves each pool's last `Sync` states, taken from that log's data.
const GIWA_RESERVE_0: u128 = 2_849_566_765_086_289_340_844;
const GIWA_RESERVE_1: u128 = 141_242_920_674_394_454_220;
const NARU_RESERVE_0: u128 = 125_338_144_751_901_190_262;
const NARU_RESERVE_1: u128 = 175_861_646_008_610_376;
const STALE_RESERVE_0: u128 = 1_000_000_000_000_000_000_000;
const STALE_RESERVE_1: u128 = 2_000_000_000_000_000_000_000;
const USDT_RESERVE_0: u128 = 1_000_000_000;
const USDT_RESERVE_1: u128 = 2_800_000_000_000;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn registry() -> Registry {
    Registry::load_dir(&workspace_root().join("data/protocols"))
        .expect("committed registry loads and validates")
}

/// The subset of the committed registry that attests only these pools. Pool
/// identity comes from the registry file alone, so this is how the test takes an
/// attestation away.
fn registry_for(pools: &[Address]) -> Registry {
    let all = registry();
    let mut chosen = Registry::default();
    for pool in pools {
        let attestation = all
            .get(PoolId::new(CHAIN, *pool))
            .unwrap_or_else(|| panic!("{pool} is not in the committed registry"));
        chosen.attest(attestation.clone());
    }
    chosen.validate().expect("subset is still evidenced");
    chosen
}

/// Replay every captured block, in chain order, against one registry.
async fn replay(pools: &[Address]) -> ReplayEngine<RecordedChainAdapter> {
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real"), CHAIN)
        .expect("real fixtures load");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry_for(pools)))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    for block in REAL_BLOCKS {
        engine
            .replay_block(BlockNumber(block), &mut report)
            .await
            .expect("real block replays");
    }
    assert_eq!(report.blocks, REAL_BLOCKS.len() as u64);
    assert_eq!(report.rejected_syncs, 0, "{:?}", report.rejections);
    engine
}

fn graph_of(engine: &ReplayEngine<RecordedChainAdapter>) -> GraphSnapshot {
    MarketGraphBuilder::new()
        .build(&engine.snapshot())
        .expect("the replay applied state, so there is a block to build at")
}

fn r(value: u128) -> U256 {
    U256::from(value)
}

fn token(addr: Address) -> TokenId {
    TokenId::new(CHAIN, addr)
}

fn pool(addr: Address) -> PoolId {
    PoolId::new(CHAIN, addr)
}

fn pos(log_index: u64) -> UpdatePosition {
    at(REAL_BLOCK, log_index)
}

fn at(block: u64, log_index: u64) -> UpdatePosition {
    UpdatePosition::new(BlockNumber(block), LogIndex(log_index))
}

/// Replay one captured block on its own, for the as-of-earlier-block cases.
async fn replay_one(block: u64, pools: &[Address]) -> ReplayEngine<RecordedChainAdapter> {
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real"), CHAIN)
        .expect("real fixtures load");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry_for(pools)))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    engine
        .replay_block(BlockNumber(block), &mut report)
        .await
        .expect("replay");
    engine
}

/// §25 in the other direction: a pool that is stale at `REAL_BLOCK` is a
/// perfectly good market at the block it actually traded in. Being kept out of
/// one graph is not the same as being thrown away.
#[tokio::test]
async fn a_pool_stale_at_the_target_block_is_a_market_at_its_own_block() {
    let engine = replay_one(5_457_650, &ALL_POOLS).await;
    let build = MarketGraphBuilder::new()
        .build_traced(&engine.snapshot())
        .expect("graph");

    assert_eq!(build.graph.block_number(), BlockNumber(5_457_650));
    assert_eq!(
        build.graph.pool_count(),
        1,
        "only pool 2 traded in this block"
    );
    assert_eq!(build.graph.edge_count(), 2);
    assert!(
        build.skipped.is_empty(),
        "the other attested pools have no record here at all: {:?}",
        build.skipped
    );
    // USDT is a node at this block and not at REAL_BLOCK — the difference is the
    // block, not the registry.
    assert_eq!(
        build.graph.nodes().copied().collect::<Vec<_>>(),
        vec![token(TOKEN_GIWAP), token(TOKEN_USDT)]
    );
    let edge = build
        .graph
        .edge(EdgeId::new(
            pool(POOL_GIWA_STALE),
            token(TOKEN_GIWAP),
            token(TOKEN_USDT),
        ))
        .expect("giwap -> usdt at its own block");
    assert_eq!(
        (edge.reserve_in, edge.reserve_out),
        (r(STALE_RESERVE_0), r(STALE_RESERVE_1))
    );
    assert_eq!(
        edge.state_position,
        at(5_457_650, 13),
        "the same log the registry cites as this pool's state evidence"
    );

    // And the same for the third pool, in its own block.
    let third = graph_of(&replay_one(10_544_346, &ALL_POOLS).await);
    assert_eq!(third.block_number(), BlockNumber(10_544_346));
    assert_eq!(third.pool_count(), 1);
    assert_eq!(
        third.neighbors(token(TOKEN_WETH)),
        [token(TOKEN_USDT)],
        "WETH — USDT is a route at this block"
    );
    assert_eq!(
        third
            .edge(EdgeId::new(
                pool(POOL_GIWA_USDT),
                token(TOKEN_WETH),
                token(TOKEN_USDT),
            ))
            .expect("weth -> usdt")
            .reserve_in,
        r(USDT_RESERVE_0)
    );
}

// --- the real market graph -------------------------------------------------

/// The full committed registry — four attested pools — against one block. Two of
/// them have state at that block, and those two are the graph.
#[tokio::test]
async fn the_real_block_becomes_a_three_token_graph() {
    let graph = graph_of(&replay(&ALL_POOLS).await);

    assert_eq!(graph.chain_id(), CHAIN);
    assert_eq!(graph.block_number(), BlockNumber(REAL_BLOCK));
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 4);
    assert_eq!(graph.pool_count(), 2);
    assert_eq!(
        graph.nodes().copied().collect::<Vec<_>>(),
        vec![token(TOKEN_HYANGNO), token(TOKEN_GIWAP), token(TOKEN_WETH)],
        "nodes come out in token id order"
    );
    // WETH is the side both pools share, so it is the only token with two ways
    // out — the first thing a later stage would need and cannot invent.
    assert_eq!(
        graph.neighbors(token(TOKEN_WETH)),
        [token(TOKEN_HYANGNO), token(TOKEN_GIWAP)]
    );
    assert_eq!(graph.neighbors(token(TOKEN_GIWAP)), [token(TOKEN_WETH)]);
    assert_eq!(graph.neighbors(token(TOKEN_HYANGNO)), [token(TOKEN_WETH)]);
}

/// §25: the two pools whose last state is a block (or twenty million blocks)
/// behind are named as skipped, with the position they actually came from. They
/// are never priced into a graph that claims to be at `REAL_BLOCK`, and their
/// token never appears as a node.
#[tokio::test]
async fn the_graph_refuses_to_mix_in_a_pool_from_an_earlier_block() {
    let build = MarketGraphBuilder::new()
        .build_traced(&replay(&ALL_POOLS).await.snapshot())
        .expect("graph");

    assert_eq!(build.graph.block_number(), BlockNumber(REAL_BLOCK));
    assert_eq!(build.graph.pool_count(), 2);
    assert_eq!(
        build.skipped,
        vec![
            SkippedPool {
                pool: pool(POOL_GIWA_USDT),
                reason: SkipReason::NotAtTargetBlock,
                state_position: Some(at(10_544_346, 6)),
            },
            SkippedPool {
                pool: pool(POOL_GIWA_STALE),
                reason: SkipReason::NotAtTargetBlock,
                state_position: Some(at(5_457_650, 13)),
            },
        ],
        "skips are sorted by pool and each names its own stale position"
    );

    // USDT is traded only by the two skipped pools, so it cannot be a node.
    assert!(!build.graph.nodes().any(|n| *n == token(TOKEN_USDT)));
    assert!(build.graph.neighbors(token(TOKEN_USDT)).is_empty());
    assert!(build.graph.edges().all(
        |edge| edge.id.token_in != token(TOKEN_USDT) && edge.id.token_out != token(TOKEN_USDT)
    ));

    // The skipped pools' reserves are not anywhere in the graph either.
    for (value, label) in [
        (r(STALE_RESERVE_0), "stale reserve0"),
        (r(STALE_RESERVE_1), "stale reserve1"),
        (r(USDT_RESERVE_0), "usdt reserve0"),
        (r(USDT_RESERVE_1), "usdt reserve1"),
    ] {
        assert!(
            build
                .graph
                .edges()
                .all(|edge| edge.reserve_in != value && edge.reserve_out != value),
            "{label} leaked into the graph"
        );
    }
}

/// `Real Pool -> PoolMeta -> PoolState -> GraphEdge`, field by field.
#[tokio::test]
async fn every_edge_matches_the_log_it_came_from() {
    let graph = graph_of(&replay(&ALL_POOLS).await);

    let giwa_in = graph
        .edge(EdgeId::new(
            pool(POOL_GIWA),
            token(TOKEN_GIWAP),
            token(TOKEN_WETH),
        ))
        .expect("giwap -> weth");
    assert_eq!(
        (giwa_in.reserve_in, giwa_in.reserve_out),
        (r(GIWA_RESERVE_0), r(GIWA_RESERVE_1))
    );
    assert_eq!(giwa_in.state_position, pos(30));
    // Nobody proved this pool's fee, so the edge carries no fee rather than an
    // assumed 0.3%.
    assert_eq!(giwa_in.fee, None);

    let giwa_out = graph
        .edge(EdgeId::new(
            pool(POOL_GIWA),
            token(TOKEN_WETH),
            token(TOKEN_GIWAP),
        ))
        .expect("weth -> giwap");
    assert_eq!(
        (giwa_out.reserve_in, giwa_out.reserve_out),
        (r(GIWA_RESERVE_1), r(GIWA_RESERVE_0))
    );
    assert_eq!(giwa_out.state_position, pos(30));

    let naru_in = graph
        .edge(EdgeId::new(
            pool(POOL_NARU),
            token(TOKEN_HYANGNO),
            token(TOKEN_WETH),
        ))
        .expect("hyangno -> weth");
    assert_eq!(
        (naru_in.reserve_in, naru_in.reserve_out),
        (r(NARU_RESERVE_0), r(NARU_RESERVE_1))
    );
    assert_eq!(naru_in.state_position, pos(50));
    assert_eq!(naru_in.fee, None);

    let naru_out = graph
        .edge(EdgeId::new(
            pool(POOL_NARU),
            token(TOKEN_WETH),
            token(TOKEN_HYANGNO),
        ))
        .expect("weth -> hyangno");
    assert_eq!(
        (naru_out.reserve_in, naru_out.reserve_out),
        (r(NARU_RESERVE_1), r(NARU_RESERVE_0))
    );
    assert_eq!(naru_out.state_position, pos(50));

    // Both directions of one pool price the same pair the registry attests.
    for (id, tokens) in [
        (POOL_GIWA, [TOKEN_GIWAP, TOKEN_WETH]),
        (POOL_NARU, [TOKEN_HYANGNO, TOKEN_WETH]),
    ] {
        let edges = graph.pool_edges(pool(id));
        assert_eq!(edges.len(), 2, "{id} is in the graph once");
        assert!(edges.iter().all(|edge| [edge.token_in(), edge.token_out()]
            == [token(tokens[0]), token(tokens[1])]
            || [edge.token_in(), edge.token_out()] == [token(tokens[1]), token(tokens[0])]),);
    }

    // No edge points at an unattested emitter.
    assert!(graph.edges().all(|edge| edge.pool() != pool(LOOKALIKE)));
    assert!(graph.neighbors(token(LOOKALIKE)).is_empty());
}

/// The graph prices each pool's last `Sync` up to the target block — not the
/// first one it ever saw.
#[tokio::test]
async fn the_graph_prices_the_latest_sync_not_the_first() {
    let graph = graph_of(&replay(&[POOL_GIWA, POOL_NARU]).await);
    let edge = graph
        .edge(EdgeId::new(
            pool(POOL_GIWA),
            token(TOKEN_GIWAP),
            token(TOKEN_WETH),
        ))
        .expect("edge");
    // Pool 1 synced three times in `BLOCK_BEFORE` (logs 150, 174, 181) and once
    // here at log 30; the reserves asserted above are this block's.
    assert_eq!(edge.state_position, pos(30));

    // Replaying only the earlier block gives a different, self-consistent graph.
    let older_graph = graph_of(&replay_one(BLOCK_BEFORE, &[POOL_GIWA, POOL_NARU]).await);
    assert_eq!(older_graph.block_number(), BlockNumber(BLOCK_BEFORE));
    assert_eq!(older_graph.pool_count(), 1, "only pool 1 syncs there");
    assert_eq!(older_graph.edge_count(), 2);
    assert_eq!(older_graph.node_count(), 2);
    let edge = older_graph.edges().next().copied().expect("an edge");
    assert_eq!(
        edge.state_position,
        UpdatePosition::new(BlockNumber(BLOCK_BEFORE), LogIndex(181)),
        "the last of three syncs in that block"
    );
    assert_ne!(edge.reserve_in, r(GIWA_RESERVE_0));
}

/// A pool the registry does not attest is not a market, even when its `Sync`
/// lands in the same block as an attested pool's.
#[tokio::test]
async fn withdrawing_one_attestation_removes_its_edges() {
    let graph = graph_of(&replay(&[POOL_GIWA]).await);
    assert_eq!(graph.edge_count(), 2);
    assert_eq!(graph.pool_count(), 1);
    assert_eq!(graph.node_count(), 2);
    assert!(!graph.nodes().any(|n| *n == token(TOKEN_HYANGNO)));
    assert!(!graph.edges().any(|edge| edge.pool() == pool(POOL_NARU)),);
}

/// Nothing the chain offers is left out of the graph without a stated reason,
/// and nothing that was never observed is quietly priced in.
#[tokio::test]
async fn nothing_attested_is_left_out_without_a_reason() {
    let engine = replay(&ALL_POOLS).await;
    let snapshot = engine.snapshot();
    let build = MarketGraphBuilder::new()
        .build_traced(&snapshot)
        .expect("graph");

    // Every pool the replay actually holds state for is accounted for: in the
    // graph, or skipped with a reason and the position it came from.
    let in_graph: std::collections::BTreeSet<_> =
        build.graph.edges().map(|edge| edge.id.pool).collect();
    let skipped: std::collections::BTreeSet<_> = build.skipped.iter().map(|s| s.pool).collect();
    let stored: std::collections::BTreeSet<_> = snapshot.pools().map(|(id, _)| *id).collect();
    assert_eq!(
        in_graph.union(&skipped).copied().collect::<Vec<_>>(),
        stored.iter().copied().collect::<Vec<_>>(),
        "graph + skips covers every stored pool, exactly once, in pool order"
    );
    assert_eq!(stored.len(), 4, "four attested pools were observed");
    assert!(build
        .skipped
        .iter()
        .all(|s| s.state_position.is_some() && s.reason == SkipReason::NotAtTargetBlock));

    // With only the two in-block pools attested, the same block yields no skips.
    let build = MarketGraphBuilder::new()
        .build_traced(&replay(&[POOL_GIWA, POOL_NARU]).await.snapshot())
        .expect("graph");
    assert!(
        build.skipped.is_empty(),
        "both attested pools synced in this block: {:?}",
        build.skipped
    );

    // One block earlier only pool 1 trades at all. The pools created later are
    // absent from the snapshot — not registered, not skipped, not zeroed.
    let older = replay_one(BLOCK_BEFORE, &ALL_POOLS).await;
    let build = MarketGraphBuilder::new()
        .build_traced(&older.snapshot())
        .expect("graph");
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
    assert_eq!(build.graph.pool_count(), 1);
    assert_eq!(older.snapshot().pools().count(), 1);
}

/// §31/§32: same input, same graph, same bytes.
#[tokio::test]
async fn the_real_graph_is_deterministic() {
    let first = graph_of(&replay(&ALL_POOLS).await);
    let second = graph_of(&replay(&ALL_POOLS).await);
    assert_eq!(first, second, "two runs of one block");

    let json_a = serde_json::to_string(&first).expect("serialize");
    let json_b = serde_json::to_string(&second).expect("serialize");
    assert_eq!(json_a, json_b, "identical inputs, identical JSON");

    let value: serde_json::Value = serde_json::from_str(&json_a).expect("json");
    assert_eq!(value["chain_id"], 91342);
    assert_eq!(value["block_number"], REAL_BLOCK);
    assert_eq!(value["nodes"].as_array().expect("nodes").len(), 3);
    assert_eq!(value["edges"].as_array().expect("edges").len(), 4);
    let edges: Vec<String> = value["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| serde_json::to_string(e).expect("edge"))
        .collect();
    let mut sorted = edges.clone();
    sorted.sort();
    assert_eq!(edges, sorted, "edge order is canonical, not incidental");
}

/// §30: every attested pool survives the replay as itself — one record, the
/// tokens it was attested with, never duplicated, overwritten, or merged into
/// another address.
#[tokio::test]
async fn the_registry_behind_the_graph_has_no_duplicates_or_merges() {
    let registry = registry();
    assert_eq!(registry.pools.len(), 4);
    let keys: Vec<PoolId> = registry.pools.keys().copied().collect();
    let unique: std::collections::BTreeSet<PoolId> = keys.iter().copied().collect();
    assert_eq!(keys.len(), unique.len(), "one PoolId, one attestation");

    // Four distinct pairs: no two attestations collapsed into one edge set.
    let pairs: std::collections::BTreeSet<(TokenId, TokenId)> = registry
        .pools
        .values()
        .map(|a| (a.token0, a.token1))
        .collect();
    assert_eq!(pairs.len(), 4);

    let engine = replay(&ALL_POOLS).await;
    let snapshot = engine.snapshot();
    assert_eq!(
        snapshot.pools().count(),
        registry.pools.len(),
        "each attestation produced exactly one stored pool"
    );
    let graph = graph_of(&engine);

    for attestation in registry.pools.values() {
        let record = snapshot
            .pools()
            .find(|(id, _)| **id == attestation.pool)
            .expect("attested pool is stored")
            .1;
        assert_eq!(
            record.meta,
            attestation.to_meta(),
            "{} keeps its own pair",
            attestation.pool.address
        );
        assert_eq!(record.meta.id, attestation.pool);

        let edges = graph.pool_edges(attestation.pool);
        let ids: std::collections::BTreeSet<EdgeId> = edges.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), edges.len(), "no repeated route");
        if edges.is_empty() {
            assert!(
                record.state.unwrap().block_number.0 != snapshot.position.unwrap().block_number.0,
                "a pool with no edges must be stale, not silently dropped"
            );
            continue;
        }
        assert_eq!(
            edges.len(),
            2,
            "{} is exactly two directions",
            attestation.pool.address
        );
        assert_eq!(
            ids.iter()
                .map(|id| id.token_in)
                .collect::<std::collections::BTreeSet<_>>(),
            [attestation.token0, attestation.token1]
                .into_iter()
                .collect(),
            "the route's tokens are the attested ones"
        );
        for id in ids {
            assert_eq!(id.pool, attestation.pool);
            assert_eq!(id.token_in.chain_id, CHAIN);
            assert_eq!(id.token_out.chain_id, CHAIN);
        }
    }

    // The graph holds the two pools that traded at the target block; the other
    // two are reported, not merged into these.
    assert_eq!(graph.pool_count(), 2);
    assert!(graph
        .pool_edges(pool(POOL_GIWA))
        .iter()
        .all(|e| e.state_position == pos(30)));
    assert!(graph
        .pool_edges(pool(POOL_NARU))
        .iter()
        .all(|e| e.state_position == pos(50)));
}
