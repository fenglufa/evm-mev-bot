//! M9.3 §34 test matrix and §35 negative controls, over graphs built through the
//! state layer.
//!
//! Every fixture goes through the door the real pipeline uses — register a pool,
//! sync its reserves, project it with `MarketGraphBuilder` — because several of
//! these tests claim "the search finds nothing here". That claim is only evidence
//! if the input is a graph the builder is willing to produce; a hand-assembled
//! snapshot would let the search be graded against a market no milestone since M2
//! has ever handed it.
//!
//! Reserves are never read by this crate. They exist in the fixtures only so the
//! graph builder admits the pool at all — a pool with an empty side is refused as
//! no market (`EdgeRejection::EmptySide`), which would quietly delete a node from
//! the topology being tested.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{address, Address, U256};

use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};
use evm_graph::{GraphSnapshot, MarketGraphBuilder};
use evm_pathfinder::{
    find_cycles, CanonicalKey, CycleCandidate, FeeStatus, PathFinderConfig, PathFinderError,
};
use evm_state::{InMemoryStateStore, StateStore, StateUpdate, SyncScanCoverage, UpdatePosition};

const CHAIN: ChainId = ChainId(7);
const BLOCK: u64 = 4_000_001;

const A: Address = address!("0x000000000000000000000000000000000000000a");
const B: Address = address!("0x000000000000000000000000000000000000000b");
const C: Address = address!("0x000000000000000000000000000000000000000c");
const D: Address = address!("0x000000000000000000000000000000000000000d");

const P1: Address = address!("0x00000000000000000000000000000000000000f1");
const P2: Address = address!("0x00000000000000000000000000000000000000f2");
const P3: Address = address!("0x00000000000000000000000000000000000000f3");
const P4: Address = address!("0x00000000000000000000000000000000000000f4");
const P5: Address = address!("0x00000000000000000000000000000000000000f5");

/// 0.3%, attested. The value is irrelevant to this layer; the attestation is not.
const FEE: Option<Fee> = Some(Fee {
    numerator: 997,
    denominator: 1000,
});

/// A proved zero fee. `Some`: a market where nothing is taken.
const FREE: Option<Fee> = Some(Fee {
    numerator: 1,
    denominator: 1,
});

/// Nobody has proved this pool's fee. Not zero, and not 997/1000.
const UNKNOWN: Option<Fee> = None;

struct Spec {
    pool: Address,
    token0: Address,
    token1: Address,
    fee: Option<Fee>,
}

fn spec(pool: Address, token0: Address, token1: Address, fee: Option<Fee>) -> Spec {
    Spec {
        pool,
        token0,
        token1,
        fee,
    }
}

/// A graph over `pools`, every one of them priced at `target`.
///
/// `synced_at` and `coverage` are exposed because the difference between "this
/// pool prices the target" and "this pool was skipped" is the whole of M9.2, and
/// a search test that cannot build an empty graph cannot test an empty graph.
fn build(pools: &[Spec], synced_at: u64, target: u64) -> GraphSnapshot {
    build_covering(pools, synced_at, target, &BTreeMap::new())
}

fn build_covering(
    pools: &[Spec],
    synced_at: u64,
    target: u64,
    coverage: &BTreeMap<PoolId, SyncScanCoverage>,
) -> GraphSnapshot {
    let mut store = InMemoryStateStore::new(CHAIN);
    for pool in pools {
        store
            .apply(StateUpdate::PoolRegistered(PoolMeta {
                id: PoolId::new(CHAIN, pool.pool),
                protocol: ProtocolId::new("test-v2"),
                token0: TokenId::new(CHAIN, pool.token0),
                token1: TokenId::new(CHAIN, pool.token1),
                fee: pool.fee,
                pool_type: PoolType::ConstantProduct,
            }))
            .expect("register a fixture pool");
    }
    for (index, pool) in pools.iter().enumerate() {
        store
            .apply(StateUpdate::PoolSynced {
                pool: PoolId::new(CHAIN, pool.pool),
                reserve0: U256::from(100_000u64),
                reserve1: U256::from(50_000u64),
                position: UpdatePosition::new(BlockNumber(synced_at), LogIndex(index as u64 + 1)),
            })
            .expect("sync a fixture pool");
    }
    let snapshot = store
        .snapshot()
        .at_target(BlockNumber(target), coverage.clone());
    MarketGraphBuilder::new()
        .build(&snapshot)
        .expect("project the fixture graph")
}

/// The market every fixture below is read at: pools priced in this very block.
fn graph(pools: &[Spec]) -> GraphSnapshot {
    build(pools, BLOCK, BLOCK)
}

fn search(pools: &[Spec]) -> Vec<CycleCandidate> {
    find_cycles(&graph(pools), &PathFinderConfig::default()).expect("search a fixture graph")
}

fn search_at(pools: &[Spec], max_hops: usize) -> Vec<CycleCandidate> {
    find_cycles(&graph(pools), &PathFinderConfig::new(max_hops)).expect("search a fixture graph")
}

/// A route as a reader would write it: `A>P1>B|B>P2>A`.
fn route(candidate: &CycleCandidate) -> String {
    candidate
        .edges
        .iter()
        .map(|edge| {
            format!(
                "{}>{}>{}",
                short(edge.token_in.address),
                short(edge.pool.address),
                short(edge.token_out.address)
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn short(address: Address) -> String {
    let text = address.to_string();
    let (head, tail) = text.split_at(6);
    format!("{head}…{}", &tail[tail.len() - 4..])
}

fn routes(cycles: &[CycleCandidate]) -> Vec<String> {
    let mut list: Vec<String> = cycles.iter().map(route).collect();
    list.sort();
    list
}

fn keys(cycles: &[CycleCandidate]) -> Vec<CanonicalKey> {
    cycles
        .iter()
        .map(|candidate| candidate.canonical_key.clone())
        .collect()
}

/// The invariants that must hold for *every* candidate in *every* result: this is
/// what §29 asks a test to assert, so it runs on all of them rather than on one.
fn assert_well_formed(cycles: &[CycleCandidate], snapshot: &GraphSnapshot) {
    for candidate in cycles {
        assert!(
            candidate.hop_count >= PathFinderConfig::MIN_MAX_HOPS,
            "a one-hop cycle is a token traded against itself: {}",
            route(candidate)
        );
        assert!(
            candidate.hop_count <= PathFinderConfig::MAX_MAX_HOPS,
            "v0.1 does not walk past three hops: {}",
            route(candidate)
        );
        assert_eq!(
            candidate.hop_count,
            candidate.edges.len(),
            "the stored hop count has to agree with the route it names"
        );
        assert_eq!(candidate.chain_id, snapshot.chain_id());
        assert_eq!(
            candidate.target_block,
            snapshot.block_number(),
            "a candidate is bound to the block its graph was taken at"
        );
        assert!(
            candidate.belongs_to(snapshot),
            "every edge has to be in the graph the search was handed"
        );
        assert!(
            candidate.is_connected(),
            "consecutive edges must join at a token: {}",
            route(candidate)
        );
        assert_eq!(
            candidate.edges.first().map(|edge| edge.token_in),
            Some(candidate.start_token),
            "the route spends the token it claims to start at"
        );
        assert_eq!(
            candidate.edges.last().map(|edge| edge.token_out),
            Some(candidate.start_token),
            "and only a route that comes home is a cycle"
        );
        let pools = candidate.pools();
        assert_eq!(
            pools.iter().collect::<BTreeSet<_>>().len(),
            pools.len(),
            "one pool may not be entered twice: {}",
            route(candidate)
        );
        let entered = candidate
            .edges
            .iter()
            .map(|edge| edge.token_in)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            entered.len(),
            candidate.edges.len(),
            "a token may not be stood on twice before the route closes: {}",
            route(candidate)
        );
        assert_eq!(
            candidate.canonical_key,
            CanonicalKey::new(&candidate.edges),
            "the key has to be this route's key, whoever walked it"
        );
        assert_eq!(
            candidate.canonical_key.start_token(),
            Some(candidate.start_token),
            "the canonical rotation decides the entry token"
        );
        assert_eq!(
            candidate.fee_status,
            expected_fee_status(candidate, snapshot),
            "fee status has to follow the edges, and nothing else"
        );
    }
    let unique = keys(cycles).into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        cycles.len(),
        "one canonical key may not be emitted twice"
    );
}

/// Re-derive a candidate's fee status from the graph, the long way round: every
/// edge looked up by identity, its attestation read fresh.
fn expected_fee_status(candidate: &CycleCandidate, snapshot: &GraphSnapshot) -> FeeStatus {
    candidate
        .edges
        .iter()
        .fold(FeeStatus::Complete, |status, edge| {
            let attested = snapshot
                .edge(*edge)
                .map(|found| found.fee.is_some())
                .unwrap_or_default();
            status.merge(attested)
        })
}

// ---------------------------------------------------------------------------
// §34 Basic
// ---------------------------------------------------------------------------

/// §34.1 — a graph with nothing in it is an answer, not a failure.
#[test]
fn t01_empty_graph_yields_no_candidates() {
    // One pool, `Sync`ed in block BLOCK+1, graph taken at BLOCK: the pool prices a
    // later block, so the builder skips it and the market has no edges at all.
    let snapshot = build(&[spec(P1, A, B, FEE)], BLOCK + 1, BLOCK);
    assert_eq!(snapshot.edge_count(), 0, "the fixture is an empty graph");
    assert!(snapshot.is_empty());
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 0, "`Ok(vec![])` is the no-cycle answer (§31)");
}

/// §34.2 — one edge in the graph, so a route cannot close.
#[test]
fn t02_single_pair_yields_no_candidates() {
    let snapshot = graph(&[spec(P1, A, B, FEE)]);
    assert_eq!(snapshot.edge_count(), 2, "one pool is two directed edges");
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 0);
}

/// §34.3 — two pairs that share no token.
#[test]
fn t03_disconnected_graph_yields_no_candidates() {
    let cycles = search(&[spec(P1, A, B, FEE), spec(P2, C, D, FEE)]);
    assert_eq!(cycles.len(), 0, "nothing walks from one pair to the other");
}

/// §34.4 — a path that dead-ends is not reported as a cycle.
#[test]
fn t04_open_path_yields_no_candidates() {
    let cycles = search(&[spec(P1, A, B, FEE), spec(P2, B, C, FEE)]);
    assert_eq!(
        cycles.len(),
        0,
        "A -> B -> C has no way home, and a search that reported it would be \
         calling a half-route a finding"
    );
}

/// §34.5 — two pools on one pair close a cycle, once per direction (§14).
#[test]
fn t05_two_hop_cycle_is_found() {
    let snapshot = graph(&[spec(P1, A, B, FEE), spec(P2, A, B, FEE)]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 2, "one candidate per direction (§14)");
    for candidate in &cycles {
        assert_eq!(candidate.hop_count, 2);
        assert_eq!(candidate.pools().len(), 2);
        let pools = candidate.pools().into_iter().collect::<BTreeSet<_>>();
        assert_eq!(
            pools,
            [PoolId::new(CHAIN, P1), PoolId::new(CHAIN, P2)]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "both markets have to be walked: {}",
            route(candidate)
        );
    }
    assert_well_formed(&cycles, &snapshot);
}

/// §34.6 — three pools in a ring close a three-hop cycle, once per direction.
#[test]
fn t06_three_hop_cycle_is_found() {
    let snapshot = graph(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 2, "one candidate per direction");
    for candidate in &cycles {
        assert_eq!(candidate.hop_count, 3);
        assert_eq!(candidate.tokens().len(), 4, "A B C A");
        assert_eq!(candidate.tokens().first(), candidate.tokens().last());
    }
    assert_well_formed(&cycles, &snapshot);
}

// ---------------------------------------------------------------------------
// §34 Pool Constraints
// ---------------------------------------------------------------------------

/// §34.7 — a route that would re-enter a pool is refused.
#[test]
fn t07_same_pool_reuse_is_rejected() {
    let cycles = search(&[spec(P1, A, B, FEE)]);
    assert_eq!(
        cycles.len(),
        0,
        "A -> P1 -> B -> P1 -> A is a fee donation with a route's name"
    );
}

/// §34.8 — two distinct pools on one pair are two markets, and the round trip
/// between them is a finding.
#[test]
fn t08_different_pools_on_one_pair_are_accepted() {
    let snapshot = graph(&[spec(P1, A, B, FEE), spec(P2, A, B, FEE)]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 2, "one candidate per direction (§14)");
    let expected: BTreeSet<PoolId> = [PoolId::new(CHAIN, P1), PoolId::new(CHAIN, P2)]
        .into_iter()
        .collect();
    for candidate in &cycles {
        assert_eq!(
            candidate.pools().into_iter().collect::<BTreeSet<_>>(),
            expected,
            "both markets have to be walked, not one of them twice: {}",
            route(candidate)
        );
    }
    assert_well_formed(&cycles, &snapshot);
}

/// §34.9 — every directed combination of a multi-pool market is considered.
#[test]
fn t09_multiple_pools_on_one_pair_are_all_considered() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, A, B, FEE),
        spec(P3, A, B, FEE),
    ];
    let snapshot = graph(&pools);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    // Three pools offer three unordered pairs, and each pair is walkable in two
    // directions: 6, not 3 and not 12.
    assert_eq!(cycles.len(), 6);
    assert_well_formed(&cycles, &snapshot);
    let mut pool_pairs: BTreeSet<(PoolId, PoolId)> = BTreeSet::new();
    for candidate in &cycles {
        let mut walked = candidate.pools();
        walked.sort();
        pool_pairs.insert((walked[0], walked[1]));
    }
    assert_eq!(
        pool_pairs.len(),
        3,
        "every pair of the three markets has to show up"
    );
}

// ---------------------------------------------------------------------------
// §34 Token Constraints
// ---------------------------------------------------------------------------

/// §34.10 — `A -> B -> A -> C -> A` repeats the start token mid-route: two cycles
/// walked as one, and neither the merged shape nor anything like it is emitted.
#[test]
fn t10_repeated_intermediate_token_is_rejected() {
    let snapshot = graph(&[
        spec(P1, A, B, FEE),
        spec(P2, A, B, FEE),
        spec(P3, A, C, FEE),
        spec(P4, A, C, FEE),
    ]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_well_formed(&cycles, &snapshot);
    // Two pools on A-B and two on A-C: 2 + 2 candidates, and nothing that walks
    // through A twice.
    assert_eq!(cycles.len(), 4);
    for candidate in &cycles {
        assert_eq!(
            candidate.hop_count, 2,
            "this graph has no triangle, so a longer route would have to revisit A"
        );
        let entered = candidate
            .edges
            .iter()
            .map(|edge| edge.token_in)
            .collect::<BTreeSet<_>>();
        assert_eq!(entered.len(), candidate.edges.len());
    }
}

/// §34.11 — a self-loop is not a market, and never reaches the search.
#[test]
fn t11_self_loop_is_rejected() {
    // A degenerate pool is refused by the graph layer (`EdgeRejection::SameToken`),
    // so the search is never handed a token trading against itself. The result is
    // still measured here rather than assumed.
    let snapshot = graph(&[spec(P1, A, A, FEE), spec(P2, A, B, FEE)]);
    assert_eq!(
        snapshot.pool_count(),
        1,
        "the self-pair must be the pool that did not become edges"
    );
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 0);
}

/// §34.12 — a simple cycle is accepted.
#[test]
fn t12_simple_cycle_is_accepted() {
    let cycles = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    assert_eq!(cycles.len(), 2);
    assert!(cycles
        .iter()
        .all(|candidate| candidate.tokens().first() == candidate.tokens().last()));
}

// ---------------------------------------------------------------------------
// §34 Canonicalization
// ---------------------------------------------------------------------------

/// §34.13 — the three seats of one cycle are one candidate.
#[test]
fn t13_rotations_are_deduplicated() {
    let snapshot = graph(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    // The walk reaches this ring from A, from B and from C — six findings before
    // folding — and reports two, one per direction.
    assert_eq!(cycles.len(), 2);
    for candidate in &cycles {
        let edges = &candidate.edges;
        for shift in 0..edges.len() {
            let rotation: Vec<_> = (shift..shift + edges.len())
                .map(|index| edges[index % edges.len()])
                .collect();
            assert_eq!(
                CanonicalKey::new(edges),
                CanonicalKey::new(&rotation),
                "entering the same route from another seat may not change its key"
            );
        }
    }
}

/// §34.14 — reversal is not a rotation, and must survive.
#[test]
fn t14_reversal_is_preserved() {
    let cycles = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    assert_eq!(
        cycles.len(),
        2,
        "A -> B -> C -> A and A -> C -> B -> A are two markets read in two directions"
    );
    let mut walked = cycles.iter();
    let there = walked.next().expect("a candidate");
    let back = walked.next().expect("a second candidate");
    for edge in &there.edges {
        let flipped = evm_graph::EdgeId::new(edge.pool, edge.token_out, edge.token_in);
        assert!(
            back.edges.contains(&flipped),
            "the other direction has to trade every pool in reverse: {} vs {}",
            route(there),
            route(back)
        );
    }
    assert_ne!(there.canonical_key, back.canonical_key);
}

/// §34.15 — the key is a property of the route, so it is the same in every run and
/// for every rotation, and it never depends on direction.
#[test]
fn t15_canonical_key_is_deterministic() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
        spec(P4, A, B, FEE),
    ];
    let first = search(&pools);
    let second = search(&pools);
    assert_eq!(keys(&first), keys(&second));
    for candidate in &first {
        let key = CanonicalKey::new(&candidate.edges);
        // The key is the least rotation of the route's own edges.
        let mut rotations: Vec<Vec<evm_graph::EdgeId>> = Vec::new();
        for shift in 0..candidate.edges.len() {
            rotations.push(
                (shift..shift + candidate.edges.len())
                    .map(|index| candidate.edges[index % candidate.edges.len()])
                    .collect(),
            );
        }
        let least = rotations
            .iter()
            .min()
            .expect("a route has at least one rotation");
        assert_eq!(key.edges(), least.as_slice(), "{}", route(candidate));
    }
}

/// §34.16 — no key appears twice in one output, on a market dense enough to make
/// that a real risk.
#[test]
fn t16_duplicate_canonical_keys_are_never_emitted() {
    let cycles = search(&[
        spec(P1, A, B, FEE),
        spec(P2, A, B, FEE),
        spec(P3, A, B, FEE),
        spec(P4, B, C, FEE),
        spec(P5, C, A, FEE),
    ]);
    assert!(cycles.len() > 4, "the fixture has to be a busy market");
    let list = keys(&cycles);
    let unique: BTreeSet<CanonicalKey> = list.iter().cloned().collect();
    assert_eq!(list.len(), unique.len());
    assert_eq!(
        list,
        {
            let mut sorted = list.clone();
            sorted.sort();
            sorted
        },
        "and the output already arrives in key order"
    );
}

// ---------------------------------------------------------------------------
// §34 Fee
// ---------------------------------------------------------------------------

/// §34.17 — every fee proved, so a later stage has everything it needs.
#[test]
fn t17_all_some_is_complete() {
    let cycles = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FREE),
        spec(P3, C, A, FEE),
        spec(P4, A, B, FREE),
    ]);
    assert!(!cycles.is_empty());
    assert!(
        cycles
            .iter()
            .all(|candidate| candidate.fee_status == FeeStatus::Complete),
        "every pool here has an attested fee, in any of its two shapes"
    );
}

/// §34.18 — one unproved fee is enough for the whole route.
#[test]
fn t18_one_none_is_incomplete() {
    let snapshot = graph(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, UNKNOWN),
        spec(P3, C, A, FEE),
    ]);
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 2, "the shapes are still in the graph");
    assert!(
        cycles
            .iter()
            .all(|candidate| candidate.fee_status == FeeStatus::Incomplete),
        "P2 is on every route this fixture offers"
    );
    assert_well_formed(&cycles, &snapshot);
}

/// §34.19 — several unproved fees are still one `Incomplete`, not a count of gaps.
#[test]
fn t19_multiple_none_is_incomplete() {
    let cycles = search(&[
        spec(P1, A, B, UNKNOWN),
        spec(P2, B, C, UNKNOWN),
        spec(P3, C, A, FEE),
    ]);
    assert_eq!(cycles.len(), 2);
    assert!(cycles
        .iter()
        .all(|candidate| candidate.fee_status == FeeStatus::Incomplete));
}

/// §34.20 — `None` is not read as zero: a proved zero fee and an unproved fee
/// cannot report the same status.
#[test]
fn t20_none_is_never_zero() {
    let proved_zero = search(&[
        spec(P1, A, B, FREE),
        spec(P2, B, C, FREE),
        spec(P3, C, A, FREE),
    ]);
    let unknown = search(&[
        spec(P1, A, B, UNKNOWN),
        spec(P2, B, C, UNKNOWN),
        spec(P3, C, A, UNKNOWN),
    ]);
    assert_eq!(
        routes(&proved_zero),
        routes(&unknown),
        "the same market, the same shapes"
    );
    assert!(proved_zero
        .iter()
        .all(|candidate| candidate.fee_status == FeeStatus::Complete));
    assert!(unknown
        .iter()
        .all(|candidate| candidate.fee_status == FeeStatus::Incomplete));
}

/// §34.21 — `None` is not read as 997/1000 either, however many neighbours prove it.
#[test]
fn t21_none_is_never_the_usual_rate() {
    let gap = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, UNKNOWN),
    ]);
    assert!(gap
        .iter()
        .all(|candidate| candidate.fee_status == FeeStatus::Incomplete));
    let closed = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    assert_eq!(
        routes(&gap),
        routes(&closed),
        "closing the gap changes no shape"
    );
    assert!(closed
        .iter()
        .all(|candidate| candidate.fee_status == FeeStatus::Complete));
    // And the search resolves nothing on its own: there is no fee value in a
    // candidate to be defaulted.
    let json = serde_json::to_string(&closed[0]).expect("a candidate serializes");
    assert!(
        !json.contains("997") && !json.contains("1000"),
        "a candidate cannot carry a fee, proved or invented: {json}"
    );
}

/// §34.22 — an incomplete candidate stays a topological finding: it has no field
/// anything downstream could read as a quote, and no promotion path in this crate.
#[test]
fn t22_incomplete_cannot_be_promoted() {
    let cycles = search(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, UNKNOWN),
        spec(P3, C, A, FEE),
    ]);
    let incomplete = cycles
        .iter()
        .find(|candidate| candidate.fee_status == FeeStatus::Incomplete)
        .expect("NC6's candidate");
    let value: serde_json::Value = serde_json::to_value(incomplete).expect("serializes");
    let mut names: Vec<String> = value
        .as_object()
        .expect("a candidate is an object")
        .keys()
        .cloned()
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "canonical_key",
            "chain_id",
            "edges",
            "fee_status",
            "hop_count",
            "start_token",
            "target_block",
        ],
        "topology, block identity and fee status — nothing else"
    );
    let text = serde_json::to_string(incomplete).expect("serializes");
    for forbidden in [
        "amount",
        "profit",
        "gas",
        "price_impact",
        "simulation",
        "execution",
        "reserve",
    ] {
        assert!(
            !text.contains(forbidden),
            "the search layer must not grow a field about {forbidden}: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// §34 Determinism
// ---------------------------------------------------------------------------

/// §34.23 — the same graph twice gives byte-identical output.
#[test]
fn t23_same_graph_twice_identical_bytes() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, A, B, UNKNOWN),
        spec(P3, B, C, FEE),
        spec(P4, C, A, FEE),
        spec(P5, A, C, FEE),
    ];
    let first = search(&pools);
    let second = search(&pools);
    assert_eq!(
        serde_json::to_string(&first).expect("serializes"),
        serde_json::to_string(&second).expect("serializes"),
        "byte for byte, not merely the same set"
    );
    assert_eq!(first, second);
}

/// §34.24 — the order the state layer was fed does not reach the answer.
#[test]
fn t24_different_insertion_order_same_result() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
        spec(P4, A, B, UNKNOWN),
    ];
    let forwards = find_cycles(&graph(&pools), &PathFinderConfig::default()).expect("search");
    let mut backwards: Vec<Spec> = Vec::new();
    for entry in pools.iter().rev() {
        backwards.push(spec(entry.pool, entry.token0, entry.token1, entry.fee));
    }
    let reversed = find_cycles(&graph(&backwards), &PathFinderConfig::default()).expect("search");
    // The log positions differ between the two runs, so the edges differ as values;
    // an `EdgeId` does not carry a position, and the answer must not either.
    assert_eq!(
        serde_json::to_string(&forwards).expect("serializes"),
        serde_json::to_string(&reversed).expect("serializes"),
    );
}

/// §34.25 — repeated runs order the output the same way, in key order rather than
/// discovery order.
#[test]
fn t25_repeated_run_same_ordering() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, A, B, FEE),
        spec(P3, B, C, FEE),
        spec(P4, C, A, FEE),
        spec(P5, C, D, FEE),
    ];
    let snapshot = graph(&pools);
    let mut previous: Option<Vec<CanonicalKey>> = None;
    for _ in 0..5 {
        let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
        let list = keys(&cycles);
        if let Some(before) = &previous {
            assert_eq!(before, &list, "the order has to repeat, not just the count");
        }
        let mut sorted = list.clone();
        sorted.sort();
        assert_eq!(list, sorted, "output arrives sorted by canonical key");
        previous = Some(list);
    }
    assert_well_formed(
        &find_cycles(&snapshot, &PathFinderConfig::default()).expect("search"),
        &snapshot,
    );
}

// ---------------------------------------------------------------------------
// §35 Negative controls
// ---------------------------------------------------------------------------

/// NC7 — a candidate is bound to the graph's target block, and to nothing else.
#[test]
fn nc7_candidates_carry_the_graphs_target_block() {
    let target = 37_224_031u64;
    let snapshot = build(
        &[
            spec(P1, A, B, FEE),
            spec(P2, B, C, FEE),
            spec(P3, C, A, FEE),
        ],
        target,
        target,
    );
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert_eq!(cycles.len(), 2, "the fixture has to produce candidates");
    for candidate in &cycles {
        assert_eq!(candidate.target_block, BlockNumber(target));
        assert_eq!(candidate.target_block, snapshot.block_number());
        assert_ne!(candidate.target_block, BlockNumber(target - 1), "not 99");
        assert_ne!(candidate.target_block, BlockNumber(target + 1), "not 101");
    }
}

/// NC8 — the same route under two target blocks is two distinct target identities.
#[test]
fn nc8_snapshots_at_different_blocks_are_distinct() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ];
    let early =
        find_cycles(&build(&pools, 100, 100), &PathFinderConfig::default()).expect("search");
    let late = find_cycles(&build(&pools, 101, 101), &PathFinderConfig::default()).expect("search");
    assert_eq!(early.len(), late.len());
    assert_eq!(
        keys(&early),
        keys(&late),
        "one market at two blocks is still the same set of routes"
    );
    for (left, right) in early.iter().zip(late.iter()) {
        assert_ne!(left.target_block, right.target_block);
        // The route is the same road in both runs: same edges, same key, same start
        // token. If `target_block` were not part of a candidate's identity, these two
        // would be one candidate reported twice.
        assert_eq!(left.canonical_key, right.canonical_key);
        assert_eq!(left.edges, right.edges);
        assert_eq!(left.start_token, right.start_token);
        assert_ne!(left, right, "so the block is what makes them two");
    }
}

/// A depth this layer does not walk is a config error, not a truncated search
/// (§31–§33), and the bound is what decides reach — never a candidate cap.
#[test]
fn nc_config_bounds_are_enforced_and_never_silently_widened() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, A, B, FEE),
        spec(P3, B, C, FEE),
        spec(P4, C, A, FEE),
    ];
    let two = search_at(&pools, 2);
    let three = search_at(&pools, 3);
    assert!(two.iter().all(|candidate| candidate.hop_count == 2));
    assert!(three.iter().any(|candidate| candidate.hop_count == 3));
    let two_keys: BTreeSet<CanonicalKey> = keys(&two).into_iter().collect();
    let three_keys: BTreeSet<CanonicalKey> = keys(&three).into_iter().collect();
    for key in &two_keys {
        assert!(
            three_keys.contains(key),
            "a wider bound loses nothing: {key}"
        );
    }
    for max_hops in [0usize, 1, 4, 9] {
        assert_eq!(
            find_cycles(&graph(&pools), &PathFinderConfig::new(max_hops)),
            Err(PathFinderError::MaxHopsOutOfRange { actual: max_hops }),
            "{max_hops} hops is not a search v0.1 runs"
        );
    }
}

/// The search's own account of how far it walked, kept as a diagnosis so a slow
/// benchmark can be read without changing what the search returns.
#[test]
fn search_reports_the_states_it_visited() {
    let snapshot = graph(&[
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
    ]);
    let traced =
        evm_pathfinder::find_cycles_traced(&snapshot, &PathFinderConfig::default()).expect("trace");
    assert_eq!(traced.cycles.len(), 2);
    assert!(
        traced.states_visited > 0,
        "a search that visited nothing found its cycles some other way"
    );
    let empty = evm_pathfinder::find_cycles_traced(
        &build(&[spec(P1, A, B, FEE)], BLOCK + 1, BLOCK),
        &PathFinderConfig::default(),
    )
    .expect("trace");
    assert!(empty.cycles.is_empty());
    assert_eq!(
        empty.states_visited, 0,
        "no edges means no states visited, which is what makes the count a diagnosis \
         rather than a second answer"
    );
}

/// §3 — the search borrows the market and leaves it exactly as it found it.
#[test]
fn the_graph_is_read_and_never_written() {
    let pools = [
        spec(P1, A, B, FEE),
        spec(P2, B, C, FEE),
        spec(P3, C, A, FEE),
        spec(P4, A, B, UNKNOWN),
    ];
    let snapshot = graph(&pools);
    let before = serde_json::to_string(&snapshot).expect("a graph serializes");
    let cycles = find_cycles(&snapshot, &PathFinderConfig::default()).expect("search");
    assert!(!cycles.is_empty());
    assert_eq!(
        serde_json::to_string(&snapshot).expect("a graph serializes"),
        before,
        "the search cannot have edited the market it read"
    );
}
