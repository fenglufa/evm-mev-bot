//! M1 acceptance: recorded blocks in, pool state out.
//!
//! Every test here answers one of the milestone's questions with a number:
//! which logs became events, which events became updates, which updates the
//! store admitted, and what the resulting reserves are. The synthetic cases run
//! on committed fixtures; the real case runs on a captured mainnet-state block
//! from chain 91342 and asserts its exact reserves.

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address, U256};

use evm_chain::RecordedChainAdapter;
use evm_core::{
    BlockNumber, ChainId, EvidenceRef, EvidenceSource, LogIndex, PoolId, PoolType, ProtocolId,
    TokenId,
};
use evm_protocol::{
    AttestationEvidence, PoolAttestation, ProtocolError, Registry, V2Adapter, V2Topics,
};
use evm_replay::{ChangeSource, ReplayEngine, ReplayError, ReplayReport, StateChange, Written};
use evm_state::{InMemoryStateStore, StateError, UpdatePosition};

const CHAIN: ChainId = ChainId(91342);

const POOL_A: Address = address!("0x1111111111111111111111111111111111111111");
const POOL_B: Address = address!("0x2222222222222222222222222222222222222222");
const UNATTESTED: Address = address!("0x3333333333333333333333333333333333333333");
const TOKEN_0: Address = address!("0x4444444444444444444444444444444444444444");
const TOKEN_1: Address = address!("0x5555555555555555555555555555555555555555");

/// The pool and block the real fixture was captured from, and the reserves its
/// last `Sync` states — all confirmed against the captured file and against
/// `getReserves()` at that block.
const REAL_BLOCK: u64 = 37_257_255;
const REAL_POOL: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");
const REAL_TOKEN_0: Address = address!("0x304912af0ce0dd6479735634d567715107bdc0c6");
const REAL_TOKEN_1: Address = address!("0x4200000000000000000000000000000000000006");
const REAL_RESERVE_0: u128 = 2_849_387_467_534_192_263_206;
const REAL_RESERVE_1: u128 = 141_265_148_507_902_942_209;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn fixture_dir(name: &str) -> PathBuf {
    workspace_root().join("fixtures/replay").join(name)
}

fn registry_path() -> PathBuf {
    workspace_root().join("data/protocols/v2-giwap-sepolia.json")
}

/// An attestation for a synthetic pool. The evidence points at the fixture
/// block itself, which is the honest kind of reference a test fixture can give.
fn attestation(pool: Address, block: u64) -> PoolAttestation {
    let ref_ = || EvidenceRef {
        source: EvidenceSource::RecordedCapture,
        block_number: Some(BlockNumber(block)),
        transaction_hash: None,
        log_index: None,
        signature: Some(V2Topics::default().sync.to_string()),
    };
    PoolAttestation {
        protocol: ProtocolId::new("v2-compatible"),
        pool: PoolId::new(CHAIN, pool),
        token0: TokenId::new(CHAIN, TOKEN_0),
        token1: TokenId::new(CHAIN, TOKEN_1),
        fee: None,
        pool_type: PoolType::ConstantProduct,
        evidence: AttestationEvidence {
            identity: vec![ref_()],
            tokens: vec![ref_()],
            state: vec![ref_()],
        },
    }
}

fn registry_for(pools: &[(Address, u64)]) -> Registry {
    let mut registry = Registry::default();
    for (pool, block) in pools {
        registry.attest(attestation(*pool, *block));
    }
    registry
        .validate()
        .expect("synthetic registry is evidenced");
    registry
}

type Engine = ReplayEngine<RecordedChainAdapter>;

/// Load a fixture directory and the registry that makes its pools real.
fn recorded(dir_name: &str, pools: &[(Address, u64)]) -> (Engine, BlockNumber, BlockNumber) {
    let chain = RecordedChainAdapter::load(&fixture_dir(dir_name), CHAIN)
        .unwrap_or_else(|e| panic!("{}: {e}", fixture_dir(dir_name).display()));
    let blocks = chain.available_blocks();
    let from = *blocks.first().expect("fixture has blocks");
    let to = *blocks.last().expect("fixture has blocks");
    let engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry_for(pools)))],
        InMemoryStateStore::new(CHAIN),
    );
    (engine, from, to)
}

fn engine(dir_name: &str, pools: &[(Address, u64)]) -> Engine {
    recorded(dir_name, pools).0
}

/// Replay every block a fixture directory holds.
async fn replay_all(dir_name: &str, pools: &[(Address, u64)]) -> (Engine, ReplayReport) {
    let (mut engine, from, to) = recorded(dir_name, pools);
    let report = engine
        .replay_range(from, to)
        .await
        .unwrap_or_else(|e| panic!("{dir_name} replay failed: {e}"));
    (engine, report)
}

fn pool(id: Address) -> PoolId {
    PoolId::new(CHAIN, id)
}

fn position(block: u64, log_index: u64) -> UpdatePosition {
    UpdatePosition::new(BlockNumber(block), LogIndex(log_index))
}

// --- the five required fixture shapes -------------------------------------

/// No-Sync / Swap-only: flow is never a reserve.
#[tokio::test]
async fn a_swap_never_produces_reserves() {
    let (engine, report) = replay_all("swap_only", &[(POOL_A, 200)]).await;
    assert_eq!(report.logs, 3);
    assert_eq!(report.swap_events, 1);
    assert_eq!(report.sync_events, 0);
    // Attested metadata makes it a pool; the swap alone does not make it a state.
    assert_eq!(report.registrations, 1);
    assert_eq!(report.syncs_applied, 0);
    assert_eq!(report.pools, 1);
    assert_eq!(report.synced_pools, 0);
    assert!(engine.snapshot().pool_state(pool(POOL_A)).is_none());
    assert!(engine.snapshot().position.is_none());
}

#[tokio::test]
async fn one_sync_becomes_the_pools_reserves() {
    let (engine, report) = replay_all("single_sync", &[(POOL_A, 210)]).await;
    assert_eq!(report.sync_events, 1);
    assert_eq!(report.syncs_applied, 1);
    let state = engine.snapshot().pool_state(pool(POOL_A)).expect("synced");
    assert_eq!(state.reserve0, U256::from(100u128));
    assert_eq!(state.reserve1, U256::from(200u128));
    assert_eq!(state.block_number, BlockNumber(210));
    assert_eq!(state.log_index, LogIndex(5));
}

#[tokio::test]
async fn the_later_sync_is_the_state() {
    let (engine, report) = replay_all("multiple_sync", &[(POOL_A, 220)]).await;
    assert_eq!(report.blocks, 2);
    assert_eq!(report.syncs_applied, 2);
    let state = engine.snapshot().pool_state(pool(POOL_A)).expect("synced");
    assert_eq!(
        (state.reserve0, state.reserve1),
        (U256::from(120u128), U256::from(180u128))
    );
    assert_eq!(state.block_number, BlockNumber(221));
}

/// §11: every write the store accepted is recorded with the chain position that
/// produced it and both sides of the value, so a state claim can be audited from
/// the run's own output instead of by re-running the replay.
#[tokio::test]
async fn every_applied_write_carries_its_position_event_and_both_sides() {
    let (_engine, report) = replay_all("multiple_sync", &[(POOL_A, 220)]).await;
    let written: Vec<Written> = report
        .state_changes
        .iter()
        .map(|change| change.written)
        .collect();
    assert_eq!(
        written,
        vec![
            Written::Registration,
            Written::ReserveStatement,
            Written::ReserveStatement
        ],
        "{:#?}",
        report.state_changes
    );
    // Every write here came from a `Sync` log, including the registration: the
    // pool became known because its first reserve statement needed metadata.
    assert!(
        report
            .state_changes
            .iter()
            .all(|change| change.source == ChangeSource::Sync),
        "{:#?}",
        report.state_changes
    );
    let syncs: Vec<&StateChange> = report
        .state_changes
        .iter()
        .filter(|change| change.written == Written::ReserveStatement)
        .collect();
    assert_eq!(syncs.len(), 2);
    // The first statement of a pool has no previous value to compare against.
    assert_eq!(syncs[0].before, None);
    let first = syncs[0].after.expect("a sync states reserves");
    assert_eq!(
        (first.reserve0, first.reserve1),
        (U256::from(100u128), U256::from(200u128))
    );
    // The second carries the first as its `before`, so the change itself is the
    // record — not a snapshot someone has to remember to take.
    assert_eq!(syncs[1].before, Some(first));
    let second = syncs[1].after.expect("synced");
    assert_eq!(
        (second.reserve0, second.reserve1),
        (U256::from(120u128), U256::from(180u128))
    );
    assert_eq!(
        (
            syncs[0].block_number.0,
            syncs[0].log_index.0,
            syncs[0].tx_index.0
        ),
        (220, 5, 0),
        "the position is the log's own, in chain order"
    );
    assert_eq!(
        (
            syncs[1].block_number.0,
            syncs[1].log_index.0,
            syncs[1].tx_index.0
        ),
        (221, 3, 0)
    );
    let first_tx = format!("{:#x}", syncs[0].tx_hash.0);
    assert!(
        first_tx.starts_with("0xa1a1"),
        "the audit line points at the fixture's own transaction: {first_tx}"
    );
    assert_eq!(syncs[0].pool, pool(POOL_A));
    // The display form is what a session file prints: no field has to be inferred.
    let line = syncs[1].to_string();
    for part in [
        "block 221",
        "tx 0",
        "log 3",
        &POOL_A.to_string(),
        "Sync",
        "synced",
        "100/200 -> 120/180",
    ] {
        assert!(line.contains(part), "{line} is missing {part}");
    }
}

/// A swap is decoded, can bring a pool into existence through attested metadata,
/// and never states a reserve (§12's authority rule, checked where the writes are
/// counted rather than in a comment).
#[tokio::test]
async fn a_swap_registers_a_pool_but_never_states_reserves() {
    let (_engine, report) = replay_all("swap_only", &[(POOL_A, 200)]).await;
    assert_eq!(report.swap_events, 1);
    assert_eq!(report.state_changes.len(), 1, "{:#?}", report.state_changes);
    let change = &report.state_changes[0];
    assert_eq!(change.source, ChangeSource::Swap);
    assert_eq!(change.written, Written::Registration);
    assert_eq!(change.before, None);
    assert_eq!(
        change.after, None,
        "a registration sets identity, not reserves"
    );
    assert!(
        report
            .state_changes
            .iter()
            .all(|change| change.written != Written::ReserveStatement),
        "no swap may ever appear as a reserve authority"
    );
}

/// Same-block ordering: the store follows log index, not the order the receipts
/// happen to be stored in — which in this fixture is deliberately scrambled.
#[tokio::test]
async fn a_blocks_last_log_is_its_last_word() {
    let (engine, report) = replay_all("same_block_ordering", &[(POOL_A, 230), (POOL_B, 230)]).await;
    assert_eq!(report.logs, 5);
    assert_eq!(report.sync_events, 4);
    assert_eq!(report.swap_events, 1);
    assert_eq!(report.syncs_applied, 4);
    assert_eq!(report.registrations, 2);
    assert_eq!(report.pools, 2);
    assert_eq!(report.synced_pools, 2);

    let snapshot = engine.snapshot();
    let a = snapshot.pool_state(pool(POOL_A)).expect("pool a");
    assert_eq!(
        (a.reserve0, a.reserve1),
        (U256::from(77u128), U256::from(88u128))
    );
    assert_eq!(a.log_index, LogIndex(181));
    let b = snapshot.pool_state(pool(POOL_B)).expect("pool b");
    assert_eq!(
        (b.reserve0, b.reserve1),
        (U256::from(33u128), U256::from(44u128))
    );
    assert_eq!(snapshot.position, Some(position(230, 181)));
}

#[tokio::test]
async fn empty_reserves_are_refused_and_recorded_not_applied() {
    let (engine, report) = replay_all("invalid_reserves", &[(POOL_A, 240)]).await;
    assert_eq!(report.sync_events, 2);
    assert_eq!(report.syncs_applied, 1);
    assert_eq!(report.rejected_syncs, 1);
    assert_eq!(report.rejections.len(), 1);
    assert!(report.rejections[0].contains("empty reserves"));
    assert!(report.rejections[0].contains("0 / 0"));
    // The pool keeps its last valid statement instead of the empty one.
    let state = engine
        .snapshot()
        .pool_state(pool(POOL_A))
        .expect("still synced");
    assert_eq!(
        (state.reserve0, state.reserve1),
        (U256::from(100u128), U256::from(200u128))
    );
    assert_eq!(state.log_index, LogIndex(5));
}

/// A log that claims to be a `Sync` from an attested pool but cannot be decoded
/// is not something a replay may walk past: the run stops with the reason.
#[tokio::test]
async fn a_malformed_state_event_stops_the_run() {
    let mut engine = engine("malformed_log", &[(POOL_A, 250)]);
    let err = engine
        .replay_range(BlockNumber(250), BlockNumber(250))
        .await
        .expect_err("malformed sync must not be ignored");
    match err {
        ReplayError::Protocol(ProtocolError::MalformedLog(_)) => {}
        other => panic!("expected a malformed-log rejection, got {other}"),
    }
    assert!(engine.snapshot().is_empty(), "nothing may be applied");
}

/// The trap this milestone is really about: an address nobody attested emits a
/// perfectly formed `Sync`. Identical topic0 is not identical protocol.
#[tokio::test]
async fn an_unattested_emitter_cannot_become_a_pool() {
    let (engine, report) = replay_all("unattested_emitter", &[(POOL_A, 260)]).await;
    assert_eq!(report.logs, 2);
    assert_eq!(report.sync_events, 1);
    assert_eq!(report.pools, 1);
    assert_eq!(report.synced_pools, 1);
    let snapshot = engine.snapshot();
    assert!(snapshot.pool_state(pool(UNATTESTED)).is_none());
    assert!(snapshot.pool_meta(pool(UNATTESTED)).is_none());
    let a = snapshot.pool_state(pool(POOL_A)).expect("attested pool");
    assert_eq!(
        (a.reserve0, a.reserve1),
        (U256::from(1u128), U256::from(2u128))
    );
}

// --- ordering and determinism ---------------------------------------------

/// Feeding blocks backwards must be refused, not applied.
#[tokio::test]
async fn replaying_a_earlier_block_after_a_later_one_is_refused() {
    let mut engine = engine("multiple_sync", &[(POOL_A, 220)]);
    engine
        .replay_range(BlockNumber(221), BlockNumber(221))
        .await
        .expect("block 221");
    let err = engine
        .replay_range(BlockNumber(220), BlockNumber(220))
        .await
        .expect_err("block 220 must not restate an older position");
    assert!(
        matches!(err, ReplayError::State(StateError::Regression { .. })),
        "{err}"
    );
    // The refused block changed nothing.
    let state = engine.snapshot().pool_state(pool(POOL_A)).expect("synced");
    assert_eq!(state.block_number, BlockNumber(221));
    assert_eq!(
        (state.reserve0, state.reserve1),
        (U256::from(120u128), U256::from(180u128))
    );
}

#[tokio::test]
async fn a_range_that_runs_backwards_is_an_error() {
    let mut engine = engine("multiple_sync", &[(POOL_A, 220)]);
    let err = engine
        .replay_range(BlockNumber(221), BlockNumber(220))
        .await
        .expect_err("inverted range");
    assert!(matches!(err, ReplayError::InvertedRange { .. }), "{err}");
}

#[tokio::test]
async fn a_missing_block_is_an_error_not_a_shorter_run() {
    let mut engine = engine("single_sync", &[(POOL_A, 210)]);
    let err = engine
        .replay_range(BlockNumber(209), BlockNumber(210))
        .await
        .expect_err("block 209 is not recorded");
    assert!(matches!(err, ReplayError::Chain(_)), "{err}");
}

/// Run A and run B over the same inputs have to agree exactly, field by field.
#[tokio::test]
async fn two_runs_of_the_same_input_are_identical() {
    let pools = [(POOL_A, 230), (POOL_B, 230)];
    let (engine_a, report_a) = replay_all("same_block_ordering", &pools).await;
    let (engine_b, report_b) = replay_all("same_block_ordering", &pools).await;
    assert_eq!(report_a, report_b);
    assert_eq!(engine_a.snapshot(), engine_b.snapshot());

    // And a second run agrees with the first even when the first is extended
    // block by block instead of as one range.
    let mut stepped = engine("multiple_sync", &[(POOL_A, 220)]);
    let mut accumulated = ReplayReport::default();
    for number in [220u64, 221] {
        stepped
            .replay_block(BlockNumber(number), &mut accumulated)
            .await
            .expect("stepped replay");
    }
    let (ranged, report_ranged) = replay_all("multiple_sync", &[(POOL_A, 220)]).await;
    assert_eq!(stepped.snapshot(), ranged.snapshot());
    assert_eq!(accumulated.syncs_applied, report_ranged.syncs_applied);
    assert_eq!(accumulated.logs, report_ranged.logs);
}

/// Snapshots iterate in pool-key order, so anything derived from them — a
/// future graph, a report, a hash — is reproducible.
#[tokio::test]
async fn snapshot_iteration_order_is_determined_by_identity() {
    let (engine, _) = replay_all("same_block_ordering", &[(POOL_A, 230), (POOL_B, 230)]).await;
    let order: Vec<Address> = engine
        .snapshot()
        .pools()
        .map(|(id, _)| id.address)
        .collect();
    assert_eq!(order, vec![POOL_A, POOL_B]);
    let synced: Vec<Address> = engine
        .snapshot()
        .synced_pools()
        .map(|(id, _)| id.address)
        .collect();
    assert_eq!(synced, vec![POOL_A, POOL_B]);
}

// --- the real chain --------------------------------------------------------

/// The registry file is the only thing that says this pool is a pool. The same
/// file carries the two pools M2 evidenced from their own `PairCreated` logs, so
/// the count here is the file's, not the one pool M1 started from.
#[test]
fn the_committed_registry_attests_the_real_pool_with_evidence() {
    let registry = Registry::load(&registry_path()).expect("registry loads and validates");
    assert_eq!(registry.pools.len(), 3);
    let pairs: std::collections::BTreeSet<_> = registry
        .pools
        .values()
        .map(|a| (a.token0, a.token1))
        .collect();
    assert_eq!(pairs.len(), 3, "three attestations, three distinct pairs");
    let attestation = registry
        .get(pool(REAL_POOL))
        .expect("real pool is attested");
    assert_eq!(attestation.protocol, ProtocolId::new("v2-compatible"));
    assert_eq!(attestation.token0, TokenId::new(CHAIN, REAL_TOKEN_0));
    assert_eq!(attestation.token1, TokenId::new(CHAIN, REAL_TOKEN_1));
    // Fee is deliberately unknown: nothing observed here attests it.
    assert_eq!(attestation.fee, None);
    assert!(attestation.evidence.is_complete());
    for refs in [
        &attestation.evidence.identity,
        &attestation.evidence.tokens,
        &attestation.evidence.state,
    ] {
        assert!(!refs.is_empty());
        assert!(refs.iter().all(|e| e.block_number.is_some()));
    }
    assert!(!registry.is_pool(pool(UNATTESTED)));
}

/// Real Data Acceptance Test: one real historical block of chain 91342, read
/// through the same adapter, decoder and store a live run would use.
#[tokio::test]
async fn the_real_block_replays_to_the_pools_real_reserves() {
    let registry = Registry::load(&registry_path()).expect("registry loads and validates");
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real"), CHAIN)
        .expect("real fixture is self-consistent");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry))],
        InMemoryStateStore::new(CHAIN),
    );
    let report = engine
        .replay_range(BlockNumber(REAL_BLOCK), BlockNumber(REAL_BLOCK))
        .await
        .expect("real block replays");

    assert_eq!(report.chain_id, Some(CHAIN));
    assert_eq!(report.blocks, 1);
    assert_eq!(report.logs, 183);
    // In this block exactly three Sync-shaped logs exist and all three come from
    // the attested pool; one Swap-shaped log, also from it.
    assert_eq!(report.sync_events, 3);
    assert_eq!(report.swap_events, 1);
    assert_eq!(report.registrations, 1);
    assert_eq!(report.syncs_applied, 3);
    assert_eq!(report.rejected_syncs, 0);
    assert_eq!(report.pools, 1);
    assert_eq!(report.synced_pools, 1);

    let snapshot = engine.snapshot();
    assert_eq!(snapshot.chain_id, CHAIN);
    assert_eq!(snapshot.position, Some(position(REAL_BLOCK, 181)));
    let meta = snapshot.pool_meta(pool(REAL_POOL)).expect("registered");
    assert_eq!(meta.token0, TokenId::new(CHAIN, REAL_TOKEN_0));
    assert_eq!(meta.token1, TokenId::new(CHAIN, REAL_TOKEN_1));
    let state = snapshot.pool_state(pool(REAL_POOL)).expect("reserves");
    assert_eq!(state.reserve0, U256::from(REAL_RESERVE_0));
    assert_eq!(state.reserve1, U256::from(REAL_RESERVE_1));
    assert_eq!(state.block_number, BlockNumber(REAL_BLOCK));
    assert_eq!(state.log_index, LogIndex(181));
    assert_eq!(state.pool, pool(REAL_POOL));
}

/// The same pool state, from the provider instead of from the fixture. Ignored
/// because it costs RPC requests; run it deliberately to prove the recorded
/// fixture and the live chain still agree.
#[tokio::test]
#[ignore = "reads the live RPC"]
async fn the_live_provider_reproduces_the_recorded_state() {
    let registry = Registry::load(&registry_path()).expect("registry loads and validates");
    let live = evm_chain::HttpChainAdapter::connect("https://sepolia-rpc.giwa.io")
        .await
        .expect("rpc reachable");
    let mut live_engine = ReplayEngine::new(
        live,
        vec![Box::new(V2Adapter::new(registry.clone()))],
        InMemoryStateStore::new(CHAIN),
    );
    let live_report = live_engine
        .replay_range(BlockNumber(REAL_BLOCK), BlockNumber(REAL_BLOCK))
        .await
        .expect("live block replays");

    let recorded = RecordedChainAdapter::load(&workspace_root().join("fixtures/real"), CHAIN)
        .expect("fixture");
    let mut recorded_engine = ReplayEngine::new(
        recorded,
        vec![Box::new(V2Adapter::new(registry))],
        InMemoryStateStore::new(CHAIN),
    );
    let recorded_report = recorded_engine
        .replay_range(BlockNumber(REAL_BLOCK), BlockNumber(REAL_BLOCK))
        .await
        .expect("recorded block replays");

    assert_eq!(live_report, recorded_report);
    assert_eq!(live_engine.snapshot(), recorded_engine.snapshot());
}
