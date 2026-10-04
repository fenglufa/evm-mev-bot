//! Verified pool → Registry → StateStore → StateSnapshot → Graph, and the doors a
//! rejected candidate cannot open (M9.1 §17, §20, §21).
//!
//! These tests exist because `Discovery != Trust` only means something if the trust
//! object is checked by machinery that was written before discovery existed. Every
//! assertion below is about a rule owned elsewhere: `Registry::validate()` decides
//! what an attestation must carry, `InMemoryStateStore` decides what a `Sync` must
//! look like, and `MarketGraphBuilder` decides whether a pool is a market. The
//! discovery layer is not in the witness box in any of them — it is only the party
//! handing over evidence.

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, U256};

use evm_core::{ChainId, PoolId, ProtocolId, TokenId};
use evm_discovery::{
    attestation_of, integrate, CandidateReads, DiscoveredState, DiscoveryError, GraphOutcome,
    RejectionReason,
};
use evm_graph::{GraphBuild, SkipReason, SkippedPool};
use evm_protocol::v2::PROTOCOL_NAME;
use evm_protocol::{AttestationEvidence, PoolAttestation, Registry, RegistryError};

mod fixtures;
use fixtures::{
    candidate, healthy_candidate, healthy_reads, healthy_reads_at, rejected, verified, BLOCK,
    CHAIN, FACTORY, MULTI_BLOCK, MULTI_FACTORY_1, MULTI_FACTORY_2, MULTI_POOL_1, MULTI_POOL_2,
    OTHER_PAIR_BLOCK, OTHER_PAIR_POOL, PAIR, PAIR_2, PAIR_3, TOKEN_A, TOKEN_B, TOKEN_C, TOKEN_D,
    TOKEN_E,
};

const CHAIN_ID: ChainId = ChainId(CHAIN);

fn pool_id(address: Address) -> PoolId {
    PoolId::new(CHAIN_ID, address)
}

fn token(address: Address) -> TokenId {
    TokenId::new(CHAIN_ID, address)
}

/// The registry the rest of the project already trusts — the same four attestations
/// the pipeline, simulation and execution lanes load, read from the committed files
/// rather than restated here.
fn base_registry() -> Registry {
    let dir = workspace_root().join("data/protocols");
    Registry::load_dir(&dir).unwrap_or_else(|err| panic!("{} does not load: {err}", dir.display()))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// One candidate, verified, integrated alone against an empty base registry.
fn integrate_one(reads: &CandidateReads) -> DiscoveredState {
    integrate(CHAIN_ID, &Registry::default(), &[verified(reads)]).expect("integration")
}

/// The graph this run built, or the reason there isn't one — every test that asks a
/// question about edges has to answer the "or there isn't one" branch first.
fn build(outcome: &GraphOutcome) -> &GraphBuild {
    match outcome {
        GraphOutcome::Built(build) => build,
        GraphOutcome::NoStateApplied => panic!("expected a graph, the store applied no position"),
    }
}

/// One pool's edges in the graph, in `EdgeId` order (which is how the snapshot
/// stores them: a `BTreeSet`, so this is the natural order, not a sorting step).
fn edges_of(build: &GraphBuild, pool: PoolId) -> Vec<evm_graph::GraphEdge> {
    build.graph.pool_edges(pool)
}

fn skipped_of(build: &GraphBuild, pool: PoolId) -> Option<&SkippedPool> {
    build.skipped.iter().find(|skipped| skipped.pool == pool)
}

#[test]
fn a_verified_pool_travels_the_existing_pipeline_and_prices_an_edge() {
    let reads = healthy_pool_a_reads();
    let state = integrate_one(&reads);
    let pool = pool_id(PAIR);

    // Registered, through Registry::validate(), with discovery's own attestation.
    assert_eq!(state.attested.len(), 1);
    assert!(state.registry.is_pool(pool));
    let attestation = state.registry.get(pool).expect("registered");
    assert_eq!(attestation.protocol, ProtocolId::new(PROTOCOL_NAME));
    assert_eq!(attestation.token0, token(TOKEN_A));
    assert_eq!(attestation.token1, token(TOKEN_B));
    assert!(attestation.evidence.is_complete());

    // §14 all the way through: the fee that was never proven is still unproven in
    // the meta the state layer holds and in the edge the graph prices.
    assert_eq!(attestation.fee, None);
    let meta = state
        .snapshot
        .pool_meta(pool)
        .expect("pool meta reaches the state layer");
    assert_eq!(meta.fee, None);

    let build = build(&state.graph);
    let edges = edges_of(build, pool);
    assert_eq!(edges.len(), 2, "both directions of one pool");
    for edge in &edges {
        assert_eq!(edge.fee, None, "an unattested fee must not become a number");
        assert_eq!(edge.state_position.block_number, reads.pinned_at);
    }
    // The reserves in the graph are the pool's own Sync, not getReserves().
    let forward = edges
        .iter()
        .find(|edge| edge.id.token_in == token(TOKEN_A))
        .expect("A -> B direction");
    assert_eq!(forward.reserve_in, U256::from(1_000u32));
    assert_eq!(forward.reserve_out, U256::from(2_000u32));
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
    assert!(state.duplicates.is_empty());
    assert!(state.store_rejections.is_empty());
}

/// The healthy fixture, bound so the test above reads as one line.
fn healthy_pool_a_reads() -> CandidateReads {
    healthy_reads(&healthy_candidate())
}

#[test]
fn a_rejected_candidate_has_no_path_into_the_registry() {
    // §20 control 8 — "unverified pool entering Registry". `integrate` takes
    // `&[VerifiedPool]`, so a rejection cannot be handed to it at all: the type
    // that carries an attestation is only ever produced by `verify`.
    let healthy = healthy_pool_a_reads();
    let mut starved = healthy_reads(&candidate(TOKEN_B, TOKEN_C, PAIR_3));
    starved.sync = None;
    starved.sync_logs_seen = 0;
    let rejection = rejected(&starved);
    assert_eq!(rejection.reason, RejectionReason::NoAuthoritativeState);

    // The run carries the verified pool; the rejected address is simply absent — not
    // registered, not a zero-state node, not a skipped row that hints at it.
    let state =
        integrate(CHAIN_ID, &Registry::default(), &[verified(&healthy)]).expect("integration");
    assert!(state.registry.is_pool(pool_id(PAIR)));
    assert!(!state.registry.is_pool(pool_id(PAIR_3)));
    assert!(!state.snapshot.pools().any(|(id, _)| *id == pool_id(PAIR_3)));
    assert_eq!(build(&state.graph).graph.pool_count(), 1);

    // The other half: attesting by hand, bypassing discovery entirely, still has to
    // be refused by Registry::validate() — the guard does not care who wrote the row.
    let (tampered, unblocked) = unevidenced_attestations();
    for (detail, row) in tampered {
        let mut registry = Registry::default();
        registry.attest(row);
        let error = registry
            .validate()
            .expect_err("a tampered attestation must not validate");
        assert!(
            matches!(
                error,
                RegistryError::Unevidenced(..) | RegistryError::DegeneratePair(..)
            ),
            "{detail}: expected a refusal, got {error}"
        );
    }

    // A row that is complete in count but names no block is *not* the registry's
    // check to make, and this says so rather than banking on a guard that is not
    // there. What closes it is that discovery cannot emit one: every ref
    // `attestation_of` writes carries the block the read was pinned at.
    let mut registry = Registry::default();
    registry.attest(unblocked.clone());
    assert!(unblocked.evidence.is_complete());
    registry
        .validate()
        .expect("the bucket guard passes; it does not read blocks");

    let produced = attestation_of(&verified(&healthy));
    for refs in [
        &produced.evidence.identity,
        &produced.evidence.tokens,
        &produced.evidence.state,
    ] {
        for reference in refs {
            assert_eq!(
                reference.block_number,
                Some(healthy.pinned_at),
                "every ref discovery files names the block it was read at (§22)"
            );
        }
    }
}

/// Attestations a caller could fabricate: the ones `Registry::validate()` refuses, plus
/// the one it cannot see (refs with no block behind them).
fn unevidenced_attestations() -> (Vec<(String, PoolAttestation)>, PoolAttestation) {
    let mut row = base_registry()
        .get(pool_id(PAIR))
        .expect("the committed registry attests the fixture pair")
        .clone();
    let complete = row.evidence.clone();

    row.evidence = AttestationEvidence {
        identity: Vec::new(),
        tokens: Vec::new(),
        state: Vec::new(),
    };
    let empty = row.clone();

    // Complete in count, empty in substance: refs that name no block are not
    // evidence of anything, which is why `is_complete()` alone is not enough here.
    row.evidence = complete.clone();
    for refs in [
        &mut row.evidence.identity,
        &mut row.evidence.tokens,
        &mut row.evidence.state,
    ] {
        for reference in refs.iter_mut() {
            reference.block_number = None;
        }
    }
    let unblocked = row.clone();
    // `is_complete()` cannot see this one: the registry's guard is about the three
    // buckets, while the block-pinning rule lives in `verify` (§22). Returned
    // separately so the test states which layer refuses what, instead of pretending
    // one guard covers both.

    row = base_registry()
        .get(pool_id(PAIR))
        .expect("committed")
        .clone();
    row.token1 = row.token0;
    let degenerate = row.clone();

    row.token1 = TokenId::new(ChainId(31337), TOKEN_B);
    let cross_chain = row;

    let refused = vec![
        ("no evidence buckets".to_string(), empty),
        ("one token on both sides".to_string(), degenerate),
        ("a token from another chain".to_string(), cross_chain),
    ];
    (refused, unblocked)
}

#[test]
fn two_claims_of_one_address_attest_once_and_report_the_other() {
    // Two `PairCreated` logs for one pair address: the registry keys on `PoolId`, so
    // there is one slot and therefore one attestation — but the claim that filled it
    // is a decision that has to be visible, not a HashMap collision resolved in
    // whichever order the windows ran (§30: `Duplicate != Reusable`).
    let early = healthy_reads_at(FACTORY, TOKEN_A, TOKEN_B, PAIR_2, BLOCK, 4);
    let late = healthy_reads_at(FACTORY, TOKEN_A, TOKEN_B, PAIR_2, BLOCK + 500, 9);
    assert_eq!(early.pinned_at.0 + 500, late.pinned_at.0);

    // In either arrival order: the earlier claim wins, so the result is an input
    // order, not a race.
    let forwards = integrate(
        CHAIN_ID,
        &Registry::default(),
        &[verified(&early), verified(&late)],
    )
    .expect("integration");
    let reversed = integrate(
        CHAIN_ID,
        &Registry::default(),
        &[verified(&late), verified(&early)],
    )
    .expect("integration");

    for state in [&forwards, &reversed] {
        assert_eq!(state.attested.len(), 1);
        assert_eq!(state.duplicates.len(), 1);
        let duplicate = &state.duplicates[0];
        assert_eq!(duplicate.pool, pool_id(PAIR_2));
        assert_eq!(duplicate.kept_block, early.pinned_at);
        assert_eq!(duplicate.dropped_block, late.pinned_at);
        assert_eq!(state.registry.pools.len(), 1);
    }
    // The one place the dedup is order-dependent is which `Sync` priced the pool:
    // the kept claim's block is the block the evidence was pinned at, and `verify`
    // already refused any other pairing.
    assert_eq!(forwards, reversed);
}

#[test]
fn two_pools_on_one_pair_reach_the_graph_as_two_markets() {
    // The M7 census's real multi-venue pair: two factories, one token pair, two pool
    // addresses. Registry keys on address, so both survive; the graph keys on
    // (token_in, token_out) -> *plural* edges, so M3 can compare them. One market
    // hidden inside another would be an arithmetic error disguised as a route.
    let left = healthy_reads_at(
        MULTI_FACTORY_1,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_1,
        MULTI_BLOCK,
        0,
    );
    let right = healthy_reads_at(
        MULTI_FACTORY_2,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_2,
        MULTI_BLOCK,
        1,
    );
    let state = integrate(
        CHAIN_ID,
        &Registry::default(),
        &[verified(&right), verified(&left)],
    )
    .expect("integration");

    assert_eq!(state.attested.len(), 2);
    assert_eq!(state.registry.pools.len(), 2);
    // `attested` is in `PoolId` order regardless of arrival order (§21).
    assert_eq!(state.attested[0].pool, pool_id(MULTI_POOL_1));
    assert_eq!(state.attested[1].pool, pool_id(MULTI_POOL_2));
    // Both `Sync` records applied: two pools at two log indexes in one block is the
    // ordering the store accepts, and the only one it accepts.
    assert!(state.store_rejections.is_empty());
    assert_eq!(
        state.snapshot.position.expect("position").block_number.0,
        MULTI_BLOCK
    );

    let build = build(&state.graph);
    let routes = build.graph.routes(token(TOKEN_D), token(TOKEN_B));
    assert_eq!(routes.len(), 2, "two pools must be two markets");
    let pools: Vec<PoolId> = routes.iter().map(|edge| edge.id.pool).collect();
    assert_eq!(
        pools,
        vec![pool_id(MULTI_POOL_1), pool_id(MULTI_POOL_2)],
        "each market names its own pool"
    );
    assert!(routes.iter().all(|edge| edge.fee.is_none()));
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
}

#[test]
fn a_pool_the_store_refuses_is_still_registered_and_skipped_by_the_graph() {
    // A `Sync` that publishes an empty side is a real pool making a real (useless)
    // statement: discovery verified it, so it is attested and registered. It is the
    // state layer that refuses to price it, and the graph that reports the refusal —
    // three different answers from three different layers, recorded separately
    // rather than collapsed into "rejected".
    let priced = healthy_reads_at(
        MULTI_FACTORY_1,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_1,
        MULTI_BLOCK,
        0,
    );
    let mut empty = healthy_reads_at(
        MULTI_FACTORY_2,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_2,
        MULTI_BLOCK,
        1,
    );
    let state_before_empty = verified(&empty);
    assert_eq!(
        state_before_empty.market_state.sync.reserve0,
        U256::from(1_000u32)
    );
    empty.sync.as_mut().expect("sync").reserve0 = U256::ZERO;

    let state = integrate(
        CHAIN_ID,
        &Registry::default(),
        &[verified(&priced), verified(&empty)],
    )
    .expect("integration");
    let refusal = &state.store_rejections[0];
    assert_eq!(refusal.pool, pool_id(MULTI_POOL_2));
    assert_eq!(refusal.rule, "empty_reserves");
    assert!(
        refusal.message.contains("empty reserves (0 /"),
        "{}",
        refusal.message
    );

    // Registered: the attestation stands on its own evidence.
    assert!(state.registry.is_pool(pool_id(MULTI_POOL_2)));
    assert_eq!(
        state
            .snapshot
            .get(pool_id(MULTI_POOL_2))
            .expect("record")
            .state,
        None,
        "the refused sync left no state behind"
    );
    // Unpriced, and said out loud by the graph rather than quietly absent from it.
    let build = build(&state.graph);
    assert_eq!(
        skipped_of(build, pool_id(MULTI_POOL_2))
            .expect("skip row")
            .reason,
        SkipReason::StateUnavailable
    );
    assert_eq!(build.graph.routes(token(TOKEN_D), token(TOKEN_B)).len(), 1);
    assert!(edges_of(build, pool_id(MULTI_POOL_1)).len() == 2);
}

#[test]
fn a_second_attestation_of_one_committed_address_is_a_conflict_not_an_update() {
    // §16 "bypass nothing": the base registry is the project's own record of what it
    // trusts, and discovery does not get to overwrite it because it found fresh
    // evidence. The fixture pair is attested in `data/protocols/v2-giwap-sepolia.json`
    // at a different block with different refs, so merging must refuse.
    let base = base_registry();
    let committed = base
        .get(pool_id(PAIR))
        .expect("committed attestation")
        .clone();
    let reads = healthy_pool_a_reads();

    let error = integrate(CHAIN_ID, &base, &[verified(&reads)])
        .expect_err("a conflicting attestation must stop the run");
    match error {
        DiscoveryError::Integration {
            layer,
            pool,
            message,
        } => {
            assert_eq!(layer, "Registry::merge");
            assert!(pool.contains(&format!("{PAIR:?}")), "{pool}");
            assert!(message.contains("attested twice"), "{message}");
            // The refusal names both sides, so an auditor can see which row won.
            assert!(message.contains(&format!("{:?}", TOKEN_A)), "{message}");
        }
        other => panic!("expected an integration error, got {other}"),
    }

    // And nothing was written: the base registry is still exactly the files on disk.
    let again = base_registry();
    assert_eq!(base, again);
    assert_eq!(
        base.get(pool_id(PAIR)).expect("still committed"),
        &committed
    );
    assert_eq!(committed.fee, None);
}

#[test]
fn an_empty_run_changes_nothing() {
    // Zero candidates is a legitimate census result. It must not be reported as a
    // crash, and it must not fabricate a graph at a block nobody observed.
    let base = base_registry();
    let state = integrate(CHAIN_ID, &base, &[]).expect("an empty census integrates");
    assert!(state.attested.is_empty());
    assert!(state.duplicates.is_empty());
    assert!(state.store_rejections.is_empty());
    assert_eq!(state.registry, base);
    assert_eq!(state.registry.pools.len(), 4);
    assert!(state.snapshot.is_empty());
    assert_eq!(state.snapshot.position, None);
    assert_eq!(state.graph, GraphOutcome::NoStateApplied);
    assert!(state.graph.build().is_none(), "no position, so no graph");
}

#[test]
fn discovery_adds_markets_on_top_of_the_committed_registry_without_touching_it() {
    let base = base_registry();
    let before = base.pools.len();
    let left = healthy_reads_at(
        MULTI_FACTORY_1,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_1,
        MULTI_BLOCK,
        0,
    );
    let right = healthy_reads_at(
        MULTI_FACTORY_2,
        TOKEN_D,
        TOKEN_B,
        MULTI_POOL_2,
        MULTI_BLOCK,
        1,
    );
    let state =
        integrate(CHAIN_ID, &base, &[verified(&left), verified(&right)]).expect("integration");

    assert_eq!(state.registry.pools.len(), before + 2);
    for (id, attestation) in base.pools.iter() {
        assert_eq!(
            state.registry.get(*id).expect("committed row survives"),
            attestation,
            "discovery did not rewrite {id:?}"
        );
    }
    state
        .registry
        .validate()
        .expect("merged registry validates");
    // The store is scoped to this run: it applies the pools discovery attested, and
    // does not re-apply the committed four. That is deliberate — rediscovering the
    // pipeline's own registry-to-store wiring here would be the parallel
    // implementation §17 forbids. What the run must prove is that nothing about the
    // base registry breaks the path, and the graph below shows exactly the two
    // pools this census priced, with no phantom rows for the other four.
    assert_eq!(state.snapshot.len(), 2);
    assert_eq!(state.snapshot.synced_pools().count(), 2);
    let build = build(&state.graph);
    assert_eq!(build.graph.pool_count(), 2);
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
    assert_eq!(build.graph.routes(token(TOKEN_D), token(TOKEN_B)).len(), 2);
}

#[test]
fn two_runs_of_one_input_are_equal_down_to_the_serialized_bytes() {
    // §21. The value comparison is necessary but not sufficient: `Registry` is a
    // `HashMap`, so two equal registries can still serialize in different orders,
    // which is exactly how a report ends up non-reproducible while its tests pass.
    let inputs = [
        healthy_reads_at(
            MULTI_FACTORY_1,
            TOKEN_D,
            TOKEN_B,
            MULTI_POOL_1,
            MULTI_BLOCK,
            0,
        ),
        healthy_reads_at(
            MULTI_FACTORY_2,
            TOKEN_D,
            TOKEN_B,
            MULTI_POOL_2,
            MULTI_BLOCK,
            1,
        ),
        healthy_reads_at(
            FACTORY,
            TOKEN_E,
            TOKEN_B,
            OTHER_PAIR_POOL,
            OTHER_PAIR_BLOCK,
            5,
        ),
    ];
    let verified_all: Vec<_> = inputs.iter().map(verified).collect();
    let base = base_registry();

    let mut shuffled = verified_all.clone();
    shuffled.rotate_right(2);
    let a = integrate(CHAIN_ID, &base, &verified_all).expect("run A");
    let b = integrate(CHAIN_ID, &base, &shuffled).expect("run B");

    assert_eq!(a, b, "the whole run result");
    assert_eq!(a.registry, b.registry);
    assert_eq!(a.attested, b.attested);
    assert_eq!(a.duplicates, b.duplicates);
    assert_eq!(a.store_rejections, b.store_rejections);
    for pool in a.registry.pools.keys().copied() {
        // Graph-relevant meta: token sides, fee, pool type, protocol.
        assert_eq!(
            a.snapshot.pool_meta(pool),
            b.snapshot.pool_meta(pool),
            "{pool:?} meta differs"
        );
    }
    // The artifact layer, byte for byte.
    let dump = |state: &DiscoveredState| {
        serde_json::to_string_pretty(&state.attested).expect("attestations serialize")
    };
    assert_eq!(dump(&a), dump(&b), "serialized evidence differs");
    let json = dump(&a);
    assert!(
        json.contains("\"fee\": null"),
        "the fee stays unattested in the bytes an auditor reads"
    );
    assert!(!json.contains("997"), "{json}");
}
