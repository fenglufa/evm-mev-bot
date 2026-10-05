//! M9.2 §19/§31 — target-block `Sync` reconstruction against a scripted node.
//!
//! The node here only answers `eth_getLogs`, and every other method errors, which is
//! the first assertion of the milestone: reconstructing a pool's state at block T is a
//! question about *logs*. A `getReserves()` call would answer a different question —
//! what the contract says its balance is — and if this module ever grew one, these
//! tests stop instead of quietly measuring a different RPC count than the evidence
//! claims (§5).
//!
//! The controls below are the ones §19 names, at the layer where the scan decision is
//! actually made: a `Sync` after the target (NC1), no `Sync` at all (NC2), an older
//! `Sync` that is still valid (NC3), several `Sync` logs in one block (NC4), a future
//! block leaking into a past answer (NC5), and a node that truncates or over-answers
//! (the two ways a scan could fake the freshness proof). NC6 (a mixed-block graph) and
//! NC7 (no graph entry without a `Sync`) are here too where they can be seen through
//! the real store and the real builder, and the graph layer carries the same set in
//! `crates/graph/tests/state_at_target.rs`.
//!
//! The last section adds the door M9.2 actually opens — `integrate_at_target` — so
//! that "reconstructed" is proved to mean "in the target block's graph, through the
//! registry, store and builder that were written before this milestone", which is the
//! only form of the claim §36 allows.

mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;

use evm_chain::{
    BlockContext, BlockData, CallRequest, ChainAdapter, ChainBlock, ChainError, ChainLog, LogFilter,
};
use evm_core::{BlockNumber, ChainId, LogIndex, PoolId, TxHash, TxIndex};
use evm_discovery::{
    integrate, integrate_at_target, GraphOutcome, HistoricalSyncSource, PoolStateAtTarget,
    PoolSyncAtTarget, Reconstruction, VerifiedPool, CHUNK_BLOCKS, NODE_LOG_LIMIT,
};
use evm_graph::{GraphBuild, MarketGraphBuilder, SkipReason};
use evm_protocol::Registry;
use evm_state::{
    InMemoryStateStore, StateSnapshot, StateStore, StateUpdate, SyncScanCoverage, UpdatePosition,
};

use fixtures::{
    healthy_reads_at, sync_at_block, sync_log, verified, CHAIN, FACTORY, NOWHERE, OTHER_PAIR_POOL,
    PAIR, PAIR_2, TOKEN_A, TOKEN_B, TOKEN_C,
};

fn pool(address: Address) -> PoolId {
    PoolId::new(ChainId(CHAIN), address)
}

fn block(number: u64) -> BlockNumber {
    BlockNumber(number)
}

fn coverage(from: u64, through: u64) -> SyncScanCoverage {
    SyncScanCoverage::new(block(from), block(through)).expect("ordered range")
}

/// The reserves this fixture pair publishes, big enough to price.
const R0: u64 = 1_000;
const R1: u64 = 2_000;

// ── The scripted node ───────────────────────────────────────────────────────

/// A node with a fixed `Sync` log set that answers `eth_getLogs` by range and address
/// the way a real one does.
struct SyncNode {
    logs: Vec<ChainLog>,
    /// A provider that answers every request with the whole set regardless of range.
    /// Real risk: a node that ignores `fromBlock`/`toBlock`, whose answer would put a
    /// block after the target inside a scan of blocks before it.
    ignores_ranges: bool,
    /// A provider that cuts its answer off at its own result ceiling.
    truncates: bool,
    /// A provider that answers with a log from another chain.
    foreign_chain: bool,
    /// A provider that answers a single-address request with some other address's log.
    foreign_emitter: bool,
    requests: AtomicUsize,
    asked: Mutex<Vec<LogFilter>>,
}

impl SyncNode {
    fn new(logs: Vec<ChainLog>) -> Self {
        Self {
            logs,
            ignores_ranges: false,
            truncates: false,
            foreign_chain: false,
            foreign_emitter: false,
            requests: AtomicUsize::new(0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn leaking(mut self) -> Self {
        self.ignores_ranges = true;
        self
    }

    fn truncating(mut self) -> Self {
        self.truncates = true;
        self
    }

    fn cross_chain(mut self) -> Self {
        self.foreign_chain = true;
        self
    }

    fn foreign_emitter(mut self) -> Self {
        self.foreign_emitter = true;
        self
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// `(from, to, addresses)` per request, in the order they were made.
    fn asked(&self) -> Vec<(u64, u64, Vec<Address>)> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|filter| {
                (
                    filter.from_block.0,
                    filter.to_block.0,
                    filter.addresses.clone(),
                )
            })
            .collect()
    }

    /// The highest block number any request named.
    fn highest_block_asked(&self) -> Option<u64> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|f| f.to_block.0)
            .max()
    }
}

fn answers(filter: &LogFilter, log: &ChainLog, ignores_ranges: bool) -> bool {
    if !ignores_ranges
        && (log.block_number < filter.from_block || log.block_number > filter.to_block)
    {
        return false;
    }
    if !filter.addresses.is_empty() && !filter.addresses.contains(&log.address) {
        return false;
    }
    match filter.topics.first().and_then(|slot| slot.as_ref()) {
        Some(want) => log.topics.first().is_some_and(|topic| want.contains(topic)),
        None => true,
    }
}

fn unavailable(method: &str) -> ChainError {
    ChainError::MissingData(format!("reconstruction may not ask {method}"))
}

#[async_trait]
impl ChainAdapter for SyncNode {
    fn chain_id(&self) -> ChainId {
        ChainId(CHAIN)
    }

    async fn latest_block(&self) -> std::result::Result<BlockNumber, ChainError> {
        Err(unavailable("eth_blockNumber"))
    }

    async fn get_block(&self, _number: BlockNumber) -> std::result::Result<ChainBlock, ChainError> {
        Err(unavailable("eth_getBlockByNumber"))
    }

    async fn get_block_data(
        &self,
        _number: BlockNumber,
    ) -> std::result::Result<BlockData, ChainError> {
        Err(unavailable("eth_getBlockByNumber"))
    }

    async fn get_block_context(
        &self,
        _number: BlockNumber,
    ) -> std::result::Result<BlockContext, ChainError> {
        Err(unavailable("eth_getBlockByNumber"))
    }

    async fn get_logs(&self, filter: LogFilter) -> std::result::Result<Vec<ChainLog>, ChainError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(filter.clone());
        let mut logs = self
            .logs
            .iter()
            .filter(|log| answers(&filter, log, self.ignores_ranges))
            .cloned()
            .collect::<Vec<_>>();
        if self.foreign_chain {
            for log in &mut logs {
                log.chain_id = ChainId(CHAIN + 1);
            }
        }
        if self.foreign_emitter {
            for log in &mut logs {
                log.address = OTHER_PAIR_POOL;
            }
        }
        if self.truncates {
            // A node that filled its ceiling: the same shape of answer as a full one,
            // and the only scan failure that cannot be told from success by looking.
            while logs.len() < NODE_LOG_LIMIT {
                let block = logs.len() as u64;
                logs.push(sync_at_block(PAIR, block));
            }
        }
        Ok(logs)
    }

    async fn call(
        &self,
        _at: BlockNumber,
        _request: &CallRequest,
    ) -> std::result::Result<alloy_primitives::Bytes, ChainError> {
        Err(unavailable("eth_call"))
    }

    async fn get_code(
        &self,
        _at: BlockNumber,
        _address: Address,
    ) -> std::result::Result<alloy_primitives::Bytes, ChainError> {
        Err(unavailable("eth_getCode"))
    }

    async fn get_balance(
        &self,
        _at: BlockNumber,
        _address: Address,
    ) -> std::result::Result<U256, ChainError> {
        Err(unavailable("eth_getBalance"))
    }

    async fn get_storage_at(
        &self,
        _at: BlockNumber,
        _address: Address,
        _slot: U256,
    ) -> std::result::Result<U256, ChainError> {
        Err(unavailable("eth_getStorageAt"))
    }

    async fn get_nonce(
        &self,
        _at: BlockNumber,
        _address: Address,
    ) -> std::result::Result<u64, ChainError> {
        Err(unavailable("eth_getTransactionCount"))
    }
}

fn source(chunk_blocks: u64) -> HistoricalSyncSource {
    HistoricalSyncSource::new()
        .with_chunk_blocks(chunk_blocks)
        .expect("non-zero chunk")
}

/// Strategy A for one pool, with the chunk width the test cares about.
async fn scan(
    chain: &SyncNode,
    pool_address: Address,
    from: u64,
    to: u64,
    chunk: u64,
) -> PoolSyncAtTarget {
    source(chunk)
        .scan_pool(chain, pool(pool_address), block(from), block(to))
        .await
        .unwrap_or_else(|err| panic!("scan of {pool_address} for {from}..={to} failed: {err}"))
}

fn bounds(pools: &[(Address, u64)]) -> BTreeMap<PoolId, BlockNumber> {
    pools
        .iter()
        .map(|(address, discovery)| (pool(*address), block(*discovery)))
        .collect()
}

// ── The filters ─────────────────────────────────────────────────────────────

/// §10/§11: the canonical `Sync(uint112,uint112)` topic0, one address for the
/// per-pool strategy, none for the census — and nothing else.
#[test]
fn both_strategies_ask_the_sync_topic_and_only_that_one() {
    let source = HistoricalSyncSource::new();
    let per_pool = source.pool_filter(&pool(PAIR), block(100), block(200));
    assert_eq!(per_pool.addresses, vec![PAIR], "one pool per request");
    let census = source.census_filter(block(100), block(200));
    assert!(census.addresses.is_empty(), "a census asks the chain");
    for filter in [&per_pool, &census] {
        assert_eq!(
            filter.topics,
            vec![Some(vec![evm_protocol::V2Topics::default().sync])],
            "exactly one question, the event that publishes reserves"
        );
        assert_eq!(filter.from_block, block(100));
        assert_eq!(filter.to_block, block(200));
    }
}

/// The chunk width M9.1 measured is the default, and a zero-width chunk is refused
/// rather than allowed to loop.
#[test]
fn the_default_chunk_is_the_measured_one_and_zero_is_refused() {
    assert_eq!(
        HistoricalSyncSource::new().chunk_blocks(),
        CHUNK_BLOCKS,
        "the shared measurement, not a new guess"
    );
    assert!(HistoricalSyncSource::new().with_chunk_blocks(0).is_err());
}

// ── NC3 — an older Sync that is still valid ─────────────────────────────────

#[tokio::test]
async fn an_older_sync_with_a_covered_gap_prices_the_target() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100)]);
    let row = scan(&chain, PAIR, 100, 120, 10).await;

    assert_eq!(row.sync_block(), Some(block(100)));
    assert_eq!(row.outcome(), PoolStateAtTarget::Reconstructed);
    assert!(row.prices_target());
    // Walking backwards from 120 in tens: 111..=120, 101..=110, then 100..=100 hits.
    assert_eq!(
        row.chunks_scanned, 3,
        "the walk stops where the proof is complete, not earlier"
    );
    assert_eq!(row.search_from, block(100));
    assert_eq!(row.coverage(), Some(coverage(100, 120)));
    // §14: provenance survives — the reserves keep the block that published them.
    let state = row.state().expect("state");
    assert_eq!(state.block_number, block(100));
    assert_ne!(state.block_number, row.target);
}

/// The first request names the target as its upper bound, and no request ever names a
/// block above it — which is what makes "no future leakage" checkable from the request
/// log alone (§11, §26).
#[tokio::test]
async fn no_request_ever_names_a_block_above_the_target() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100)]);
    scan(&chain, PAIR, 95, 120, 10).await;
    assert_eq!(
        chain.highest_block_asked(),
        Some(120),
        "the target caps the walk"
    );
    for (from, to, _) in chain.asked() {
        assert!(
            from <= to && to <= 120,
            "request {from}..={to} is malformed"
        );
    }
}

// ── The backward walk and its early stop ────────────────────────────────────

#[tokio::test]
async fn the_walk_stops_at_the_first_chunk_that_answers() {
    // A Sync at 100 and a newer one at 400; the target is 500. Nothing below 301 is
    // ever asked about, and that is sound: (400, 500] is fully read.
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100), sync_at_block(PAIR, 400)]);
    let row = scan(&chain, PAIR, 100, 500, 100).await;

    assert_eq!(
        row.sync_block(),
        Some(block(400)),
        "the latest, not the first"
    );
    assert_eq!(row.chunks_scanned, 2);
    let asked = chain.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0], (401, 500, vec![PAIR]));
    assert_eq!(asked[1], (301, 400, vec![PAIR]));
    assert_eq!(row.search_from, block(301));
    assert_eq!(row.discovery_block, block(100), "the bound it was given");
    assert!(row.prices_target());
}

#[tokio::test]
async fn a_sync_exactly_on_a_chunk_edge_is_found() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 401)]);
    let row = scan(&chain, PAIR, 100, 500, 100).await;
    assert_eq!(row.sync_block(), Some(block(401)));
    assert_eq!(row.chunks_scanned, 1);
}

/// The lower bound is the pool's discovery block, never genesis (§8): a pool with no
/// `Sync` at all is scanned down to the block it was created in and stops there.
#[tokio::test]
async fn a_pool_that_published_nothing_is_scanned_to_its_creation_not_to_genesis() {
    let chain = SyncNode::new(vec![sync_at_block(OTHER_PAIR_POOL, 500)]);
    let row = scan(&chain, PAIR, 400, 500, 25).await;

    assert_eq!(row.outcome(), PoolStateAtTarget::NothingPublished);
    assert_eq!(row.as_str(), "state_unavailable");
    assert_eq!(row.selected, None);
    assert_eq!(row.sync_logs_seen, 0);
    assert_eq!(row.search_from, block(400));
    // 400..=500 in chunks of 25 is 5 requests, and exactly 5 were made: the walk is
    // complete, so "nothing published" is a read fact rather than an unstarted scan.
    assert_eq!(row.chunks_scanned, 5);
    assert_eq!(chain.requests(), 5);
    // Backwards from 500: 476..=500, 451..=475, 426..=450, 401..=425, then the last
    // chunk is clamped to the bound, so it is the single block 400.
    assert_eq!(
        chain.asked().last(),
        Some(&(400, 400, vec![PAIR])),
        "the walk ends on the bound, not below it"
    );
    // The negative scan is still coverage: it is what licenses saying "no later Sync".
    assert_eq!(row.coverage(), Some(coverage(400, 500)));
    assert!(!row.prices_target());
}

// ── NC1 — a Sync after the target ───────────────────────────────────────────

#[tokio::test]
async fn a_sync_after_the_target_is_neither_selected_nor_read() {
    let chain = SyncNode::new(vec![
        sync_at_block(PAIR, 100),
        sync_at_block(PAIR, 130),
        sync_at_block(PAIR, 400),
    ]);
    let row = scan(&chain, PAIR, 100, 120, 10).await;

    assert_eq!(row.sync_block(), Some(block(100)));
    assert_eq!(row.search_to, block(120));
    assert!(
        chain.asked().iter().all(|(_, to, _)| *to <= 120),
        "a block after the target was asked about: {:?}",
        chain.asked()
    );
}

/// The same pool read one block later admits the newer `Sync` — so the first answer
/// was a statement about the target, not a stale cache of the newest thing known.
#[tokio::test]
async fn asking_one_block_later_changes_the_answer() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100), sync_at_block(PAIR, 121)]);
    let at_120 = scan(&chain, PAIR, 100, 120, 10).await;
    let at_121 = scan(&chain, PAIR, 100, 121, 10).await;
    assert_eq!(at_120.sync_block(), Some(block(100)));
    assert_eq!(at_121.sync_block(), Some(block(121)));
    assert_eq!(at_121.search_to, block(121));
}

// ── NC4 — several Syncs in one block ────────────────────────────────────────

/// Real-looking positions: one block, three logs, block-global indexes.
#[tokio::test]
async fn the_last_sync_of_a_block_is_the_one_selected() {
    let chain = SyncNode::new(vec![
        sync_log(PAIR, U256::from(11u32), U256::from(1u32), 200, 150),
        sync_log(PAIR, U256::from(22u32), U256::from(2u32), 200, 174),
        sync_log(PAIR, U256::from(33u32), U256::from(3u32), 200, 181),
    ]);
    let row = scan(&chain, PAIR, 150, 210, 100).await;

    assert_eq!(row.sync_logs_seen, 3, "all three were read");
    let selected = row.selected.expect("a Sync");
    assert_eq!(selected.log_index, LogIndex(181));
    assert_eq!(selected.tx_index, TxIndex(1));
    assert_eq!(selected.reserve0, U256::from(33u32));
    assert_eq!(row.chunks_scanned, 1);
    // The scan began inside the state's own block, which covers every position after
    // it within that block — the §7 ordering rule, exercised rather than asserted.
    assert!(row.prices_target());
}

/// Feed order is not selection order: a node that returns logs newest-first must not
/// change the answer.
#[tokio::test]
async fn a_node_that_answers_in_reverse_order_gives_the_same_selection() {
    let forwards = vec![
        sync_log(PAIR, U256::from(11u32), U256::from(1u32), 100, 1),
        sync_log(PAIR, U256::from(22u32), U256::from(2u32), 120, 4),
    ];
    let mut backwards = forwards.clone();
    backwards.reverse();
    let a = scan(&SyncNode::new(forwards), PAIR, 100, 120, 100).await;
    let b = scan(&SyncNode::new(backwards), PAIR, 100, 120, 100).await;
    assert_eq!(a, b);
    assert_eq!(a.sync_block(), Some(block(120)));
}

// ── The two ways a scan could fake a freshness proof ────────────────────────

#[tokio::test]
async fn a_window_at_the_node_ceiling_is_refused_not_believed() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100)]).truncating();
    let err = source(100)
        .scan_pool(&chain, pool(PAIR), block(100), block(120))
        .await
        .expect_err("a truncated window must not become a scan");
    let message = err.to_string();
    assert!(
        message.contains("per-request ceiling"),
        "the error should name the rule: {message}"
    );
    assert!(
        message.contains("truncated census is not a census"),
        "and the reason: {message}"
    );
}

/// A provider that ignores `fromBlock`/`toBlock` would hand this scan a block after
/// the target. The scan checks the range it asked for instead of trusting the answer,
/// so NC5 fails loudly rather than pricing block 130's reserves at block 120.
#[tokio::test]
async fn a_node_that_answers_outside_the_requested_range_is_an_error() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100), sync_at_block(PAIR, 130)]).leaking();
    let err = source(100)
        .scan_pool(&chain, pool(PAIR), block(100), block(120))
        .await
        .expect_err("an out-of-range answer is not history");
    assert!(
        err.to_string().contains("outside"),
        "the error should name the range rule: {err}"
    );
}

#[tokio::test]
async fn a_cross_chain_answer_stops_the_run() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100)]).cross_chain();
    let err = source(100)
        .scan_pool(&chain, pool(PAIR), block(100), block(120))
        .await
        .expect_err("another chain's log is not this chain's state");
    assert!(err.to_string().contains("chain"), "{err}");
}

/// A single-address request answered with another pool's `Sync` is the quietest way a
/// scan could lie: the reserves are real, the block is in range, and the emitter is the
/// only thing wrong. It stops the run rather than becoming a record for the asked pool.
#[tokio::test]
async fn a_scan_never_becomes_another_pools_statement() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 100)]).foreign_emitter();
    let err = source(100)
        .scan_pool(&chain, pool(PAIR), block(100), block(120))
        .await
        .expect_err("some other pool traded; this one says nothing");
    assert!(
        err.to_string().contains("answered with a log emitted by"),
        "the error should name the emitter rule: {err}"
    );
}

/// The same fact, seen from the strategy that *does* expect other emitters: a census
/// groups by address, so one pool's `Sync` never becomes another pool's state.
#[tokio::test]
async fn a_census_attributes_each_log_to_the_address_that_emitted_it() {
    let chain = SyncNode::new(vec![sync_at_block(PAIR, 120), sync_at_block(PAIR_2, 121)]);
    let census = source(100)
        .census(&chain, block(100), block(130))
        .await
        .expect("census");
    let rows = census.reconstruction(&bounds(&[(PAIR, 100), (PAIR_2, 100)]));
    assert_eq!(
        rows.row(pool(PAIR)).expect("row").sync_block(),
        Some(block(120)),
        "PAIR must not inherit the newer log"
    );
    assert_eq!(
        rows.row(pool(PAIR_2)).expect("row").sync_block(),
        Some(block(121))
    );
    for row in &rows.rows {
        let selected = row.selected.expect("a Sync");
        assert_eq!(selected.pool, row.pool, "a record belongs to its emitter");
    }
}

/// Asking about a pool on a chain the adapter does not serve is a configuration
/// error, not an empty scan: an empty scan would look like "this pool never traded".
#[tokio::test]
async fn a_pool_from_another_chain_is_refused_before_the_first_request() {
    let chain = SyncNode::new(vec![]);
    let foreign = PoolId::new(ChainId(CHAIN + 1), PAIR);
    let err = source(10)
        .scan_pool(&chain, foreign, block(100), block(120))
        .await
        .expect_err("a chain mismatch is not a negative finding");
    assert!(err.to_string().contains("chain"), "{err}");
    assert_eq!(
        chain.requests(),
        0,
        "no request was spent on a wrong question"
    );
}

#[tokio::test]
async fn a_target_below_the_discovery_block_is_refused() {
    let chain = SyncNode::new(vec![]);
    let err = source(10)
        .scan_pool(&chain, pool(PAIR), block(120), block(100))
        .await
        .expect_err("the range would be empty");
    assert!(err.to_string().contains("cannot answer for"), "{err}");
    assert_eq!(chain.requests(), 0);
}

// ── NC2-adjacent: state that exists and prices nothing ──────────────────────

#[tokio::test]
async fn a_sync_that_publishes_an_empty_side_is_state_invalid_not_absent_state() {
    let chain = SyncNode::new(vec![sync_log(PAIR, U256::from(R0), U256::ZERO, 120, 120)]);
    let row = scan(&chain, PAIR, 100, 120, 100).await;

    assert_eq!(row.outcome(), PoolStateAtTarget::EmptyReserves);
    assert_eq!(row.as_str(), "state_invalid");
    assert!(row.selected.is_some(), "the pool did publish");
    assert!(!row.prices_target(), "and it prices nothing");
}

// ── Strategy B, the census ──────────────────────────────────────────────────

#[tokio::test]
async fn a_census_covers_the_range_and_separates_the_pools_it_was_not_asked_about() {
    let chain = SyncNode::new(vec![
        sync_at_block(PAIR, 120),
        sync_at_block(PAIR_2, 121),
        sync_at_block(OTHER_PAIR_POOL, 118),
    ]);
    let census = source(10)
        .census(&chain, block(100), block(130))
        .await
        .expect("census");

    assert_eq!(census.rpc_calls(), 4, "100..=130 in chunks of 10");
    assert_eq!(census.blocks_covered(), 31);
    assert_eq!(census.raw_logs.len(), 3);
    assert_eq!(census.undecodable_logs, 0);

    let watched: BTreeSet<PoolId> = [pool(PAIR), pool(PAIR_2)].into_iter().collect();
    assert_eq!(
        census.irrelevant_logs(&watched),
        1,
        "the third pool is the volume a census pays for"
    );

    let rows = census.reconstruction(&bounds(&[(PAIR, 100), (PAIR_2, 100), (NOWHERE, 100)]));
    assert_eq!(
        rows.row(pool(PAIR)).expect("row").sync_block(),
        Some(block(120))
    );
    assert_eq!(
        rows.row(pool(NOWHERE)).expect("row").outcome(),
        PoolStateAtTarget::NothingPublished
    );
    // A census cannot stop per pool, so every row's scan starts where the pass did.
    for row in &rows.rows {
        assert_eq!(row.search_from, block(100));
        assert_eq!(row.search_to, block(130));
        assert_eq!(row.chunks_scanned, 4, "the shared pass, not 4 per pool");
    }
    assert_eq!(rows.chunks_scanned, 4);
    assert_eq!(rows.logs_returned, 3);
}

#[tokio::test]
async fn a_census_range_that_overlaps_itself_counts_the_repeat() {
    // Two windows covering block 120, the way an overlapping or misbehaving provider
    // produces: the log is one fact and the duplicate is recorded, not summed twice.
    let once = sync_at_block(PAIR, 120);
    let mut twice = once.clone();
    twice.tx_hash = TxHash(B256::left_padding_from(&[7u8]));
    let chain = SyncNode::new(vec![once, twice]);
    let census = source(100)
        .census(&chain, block(100), block(130))
        .await
        .expect("census");
    assert_eq!(census.duplicate_logs, 1);
    assert_eq!(census.raw_logs.len(), 1);
}

#[tokio::test]
async fn a_census_of_an_inverted_range_is_refused() {
    let chain = SyncNode::new(vec![]);
    assert!(source(10)
        .census(&chain, block(120), block(100))
        .await
        .is_err());
    assert_eq!(chain.requests(), 0);
}

/// §12's correctness half, offline: the two strategies ask different questions of the
/// node and must give the same market answer.
#[tokio::test]
async fn both_strategies_select_the_same_sync_for_the_same_pools() {
    let logs = vec![
        sync_at_block(PAIR, 105),
        sync_at_block(PAIR, 125),
        sync_at_block(PAIR_2, 110),
        sync_at_block(OTHER_PAIR_POOL, 99),
        sync_at_block(OTHER_PAIR_POOL, 140),
    ];
    let pools = [
        (PAIR, 100u64),
        (PAIR_2, 100),
        (OTHER_PAIR_POOL, 100),
        (NOWHERE, 100),
    ];
    let per_pool = source(25)
        .reconstruct(&SyncNode::new(logs.clone()), &bounds(&pools), block(130))
        .await
        .expect("strategy a");
    let census = source(25)
        .census(&SyncNode::new(logs), block(100), block(130))
        .await
        .expect("strategy b");
    let wide = census.reconstruction(&bounds(&pools));

    assert_eq!(
        per_pool.selections(),
        wide.selections(),
        "the two query shapes disagree about the market state"
    );
    // What they legitimately differ on is the cost, which is the half §12 measures.
    assert!(
        per_pool.chunks_scanned > 0 && wide.chunks_scanned > 0,
        "both runs did work"
    );
    assert_eq!(
        per_pool.counted(PoolStateAtTarget::Reconstructed),
        wide.counted(PoolStateAtTarget::Reconstructed)
    );
    assert_eq!(
        per_pool.counted(PoolStateAtTarget::NothingPublished),
        wide.counted(PoolStateAtTarget::NothingPublished)
    );
    // Per-row counters mean different things per strategy, which is why §12 compares
    // the run totals: four backward pool scans (2 + 1 + 1 + 2 chunks) against one
    // chain-wide pass over the same range (2 chunks).
    assert_eq!(per_pool.chunks_scanned, 6);
    assert_eq!(wide.chunks_scanned, 2);
}

// ── The run over a pool set ─────────────────────────────────────────────────

/// §18/§27: the result is a function of (chain, target, pool set) alone. A map built
/// in a different order produces byte-identical rows, and a second run produces the
/// same value.
#[tokio::test]
async fn the_same_inputs_produce_the_same_rows_in_the_same_order() {
    let logs = vec![sync_at_block(PAIR, 120), sync_at_block(PAIR_2, 100)];
    let chain = SyncNode::new(logs);

    let mut forwards = BTreeMap::new();
    forwards.insert(pool(PAIR), block(100));
    forwards.insert(pool(PAIR_2), block(100));
    let mut backwards = BTreeMap::new();
    backwards.insert(pool(PAIR_2), block(100));
    backwards.insert(pool(PAIR), block(100));

    let first = source(10)
        .reconstruct(&chain, &forwards, block(125))
        .await
        .expect("run");
    let second = source(10)
        .reconstruct(&chain, &backwards, block(125))
        .await
        .expect("re-run");
    assert_eq!(first, second);
    assert_eq!(
        first.rows.iter().map(|r| r.pool).collect::<Vec<_>>(),
        vec![pool(PAIR), pool(PAIR_2)],
        "rows are in PoolId order, not map-insertion order"
    );
    assert_eq!(
        first
            .rows
            .iter()
            .map(|r| r.sync_logs_seen)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
}

#[tokio::test]
async fn a_run_counts_its_own_work_and_names_which_pools_it_could_not_price() {
    let logs = vec![sync_at_block(PAIR, 124), sync_at_block(PAIR_2, 100)];
    let chain = SyncNode::new(logs);
    let run = source(10)
        .reconstruct(
            &chain,
            &bounds(&[(PAIR, 100), (PAIR_2, 100), (OTHER_PAIR_POOL, 115)]),
            block(124),
        )
        .await
        .expect("run");

    assert_eq!(run.target, block(124));
    assert_eq!(run.rows.len(), 3);
    assert_eq!(run.counted(PoolStateAtTarget::Reconstructed), 2);
    assert_eq!(run.counted(PoolStateAtTarget::NothingPublished), 1);
    assert_eq!(run.counted(PoolStateAtTarget::EmptyReserves), 0);
    // PAIR hits in its first chunk; PAIR_2 needs three (115..=124, 105..=114,
    // 100..=104); the third pool's bound is 115, one chunk, and it hits nothing.
    assert_eq!(run.chunks_scanned, 1 + 3 + 1);
    assert_eq!(run.logs_returned, 2);
    assert_eq!(run.blocks_scanned(), 10 + 25 + 10);
    assert_eq!(
        run.coverage().keys().copied().collect::<Vec<_>>(),
        vec![pool(PAIR), pool(OTHER_PAIR_POOL), pool(PAIR_2)],
        "a completed scan is coverage even when it found nothing"
    );
}

// ── The projection, at the layer that owns it ───────────────────────────────

/// The two facts §13 asks to keep auditable side by side, in the store's own shape.
#[tokio::test]
async fn a_reconstruction_projects_a_snapshot_without_moving_its_observations() {
    let chain = SyncNode::new(vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 100, 3)]);
    let run = source(10)
        .reconstruct(&chain, &bounds(&[(PAIR, 100)]), block(120))
        .await
        .expect("run");

    let snapshot = store_with(&[(PAIR, TOKEN_A, TOKEN_B)], &[(PAIR, 100, 3, R0, R1)]).snapshot();
    let projected = snapshot.at_target(block(120), run.coverage());

    assert_eq!(projected.target_block(), Some(block(120)));
    assert_eq!(
        projected.pool_coverage(pool(PAIR)),
        Some(coverage(100, 120))
    );
    // The observation itself is untouched: block 100 still says where it came from.
    assert_eq!(
        projected.block_number(),
        Some(block(100)),
        "the target must not overwrite the evidence"
    );
    let state = projected.pool_state(pool(PAIR)).expect("state");
    assert_eq!(state.block_number, block(100));
    assert_eq!(state.log_index, LogIndex(3));
}

/// The milestone's actual point, end to end through the real doors: a pool whose
/// `Sync` is twenty blocks old joins the target block's graph because a scan covers
/// the gap — and one nobody scanned still does not.
#[tokio::test]
async fn a_reconstructed_pool_joins_the_target_graph_and_an_unscanned_one_does_not() {
    let chain = SyncNode::new(vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 100, 3)]);
    let run = source(10)
        .reconstruct(&chain, &bounds(&[(PAIR, 100)]), block(120))
        .await
        .expect("run");

    // Two pools in one store: PAIR reconstructed, PAIR_2 observed at 110 and never
    // scanned. On different pairs, so each would have edges of its own to lose.
    let snapshot = store_with(
        &[(PAIR, TOKEN_A, TOKEN_B), (PAIR_2, TOKEN_C, TOKEN_B)],
        &[(PAIR, 100, 3, R0, R1), (PAIR_2, 110, 4, R0, R1)],
    )
    .snapshot();
    let build = traced(&snapshot.at_target(block(120), run.coverage()));

    assert_eq!(build.graph.block_number(), block(120));
    assert_eq!(build.graph.pool_count(), 1, "only the reconstructed pool");
    let edges = build.graph.pool_edges(pool(PAIR));
    assert_eq!(edges.len(), 2, "one pair, both directions");
    for edge in &edges {
        assert_eq!(
            edge.state_position,
            UpdatePosition::new(block(100), LogIndex(3))
        );
    }
    assert_eq!(
        skipped_reasons(&build),
        vec![(pool(PAIR_2), SkipReason::NotAtTargetBlock)]
    );
}

/// NC7 at the discovery layer: coverage without state is nothing. A pool whose
/// contract reads were flawless and whose `Sync` history is empty cannot be priced at
/// the target by any amount of scanning — the `getReserves()` path stays structurally
/// unavailable, and the graph says why.
#[tokio::test]
async fn coverage_alone_never_grants_graph_eligibility() {
    let chain = SyncNode::new(vec![]);
    let run = source(10)
        .reconstruct(&chain, &bounds(&[(PAIR, 100)]), block(120))
        .await
        .expect("run");
    assert_eq!(
        run.row(pool(PAIR)).expect("row").outcome(),
        PoolStateAtTarget::NothingPublished
    );

    // The store knows PAIR and has never seen a `Sync` from it. PAIR_2 is synced at
    // the target block, so the snapshot has a block to build at and PAIR's absence is
    // about PAIR, not about a missing block identity.
    let snapshot = store_with(
        &[(PAIR, TOKEN_A, TOKEN_B), (PAIR_2, TOKEN_C, TOKEN_B)],
        &[(PAIR_2, 120, 5, R0, R1)],
    )
    .snapshot();
    let build = traced(&snapshot.at_target(block(120), run.coverage()));
    assert_eq!(build.graph.pool_count(), 1);
    assert!(
        build.graph.pool_edges(pool(PAIR)).is_empty(),
        "a verified pool with no published reserves has nothing to price with"
    );
    assert_eq!(
        skipped_reasons(&build),
        vec![(pool(PAIR), SkipReason::StateUnavailable)]
    );
}

// ── The door M9.2 opens in the existing pipeline ────────────────────────────
//
// The tests above ask the scan and the snapshot questions. These ask the one
// question the milestone is judged on: does a reconstructed pool reach a target
// block's graph through the doors that already exist — `Registry::validate()`,
// `InMemoryStateStore`'s position rule, `MarketGraphBuilder` — rather than through
// a reconstruction-shaped shortcut next to them. `integrate_at_target` is the whole
// of discovery's new surface here, and every assertion below is about what the
// layers it hands evidence to then decided.

/// One verified pool at one chain position, in the shape `integrate` takes.
fn verified_at(
    token0: Address,
    token1: Address,
    pair: Address,
    block_number: u64,
    log_index: u64,
) -> VerifiedPool {
    verified(&healthy_reads_at(
        FACTORY,
        token0,
        token1,
        pair,
        block_number,
        log_index,
    ))
}

/// The same scan the offline tests above run, handed to the real pipeline.
async fn reconstruct(
    logs: Vec<ChainLog>,
    bounds_input: &[(Address, u64)],
    target: u64,
    chunk: u64,
) -> Reconstruction {
    let chain = SyncNode::new(logs);
    source(chunk)
        .reconstruct(&chain, &bounds(bounds_input), block(target))
        .await
        .expect("reconstruction")
}

/// NC3 at the milestone's real boundary: a pool whose last `Sync` is fifteen blocks
/// old joins the target block's graph, with its price still recorded at the block
/// that published it (§14 — the target never overwrites the evidence).
#[tokio::test]
async fn a_reconstructed_pool_reaches_the_graph_through_the_real_door() {
    let verified_pool = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let run = reconstruct(
        vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 105, 9)],
        &[(PAIR, 100)],
        120,
        1_000,
    )
    .await;
    let state = integrate_at_target(ChainId(CHAIN), &Registry::default(), &[verified_pool], &run)
        .expect("integration");

    assert_eq!(state.attested.len(), 1);
    // Two block identities in one snapshot, and they are different on purpose.
    assert_eq!(state.snapshot.target_block(), Some(block(120)));
    assert_eq!(state.snapshot.block_number(), Some(block(105)));
    let build = match &state.graph {
        GraphOutcome::Built(build) => build,
        GraphOutcome::NoStateApplied => panic!("a pool with reserves should price"),
    };
    assert_eq!(
        build.graph.block_number(),
        block(120),
        "the graph is the target's"
    );
    assert_eq!(build.graph.pool_count(), 1);
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
    let edges = build.graph.pool_edges(pool(PAIR));
    assert_eq!(edges.len(), 2, "both directions of one pair");
    for edge in &edges {
        assert_eq!(
            edge.state_position,
            UpdatePosition::new(block(105), LogIndex(9)),
            "the edge names the Sync that published it, not the block it is priced at"
        );
        assert_eq!(edge.reserve_in + edge.reserve_out, U256::from(R0 + R1));
    }
}

/// §22's "one reason per skipped pool", through the door: a pool the run attested but
/// the reconstruction never covered is registered and skipped, never silently absent.
#[tokio::test]
async fn a_pool_the_reconstruction_did_not_cover_is_registered_and_skipped() {
    let covered = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let uncovered = verified_at(TOKEN_C, TOKEN_B, PAIR_2, 100, 5);
    let run = reconstruct(
        vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 105, 9)],
        &[(PAIR, 100)],
        120,
        1_000,
    )
    .await;
    let state = integrate_at_target(
        ChainId(CHAIN),
        &Registry::default(),
        &[covered, uncovered],
        &run,
    )
    .expect("integration");

    assert_eq!(state.attested.len(), 2, "both are markets");
    assert!(state
        .snapshot
        .pool_meta(pool(PAIR_2))
        .is_some_and(|meta| meta.fee.is_none()));
    // …and neither is synced: only the covered pool got a position.
    assert_eq!(
        state
            .snapshot
            .pool_state(pool(PAIR_2))
            .map(|state| state.block_number),
        None,
        "a pool with no row has no state, which is not the same as no pool"
    );
    let build = match &state.graph {
        GraphOutcome::Built(build) => build,
        GraphOutcome::NoStateApplied => panic!("the covered pool applied a position"),
    };
    assert_eq!(build.graph.pool_count(), 1);
    assert_eq!(
        skipped_reasons(build),
        vec![(pool(PAIR_2), SkipReason::StateUnavailable)]
    );
}

/// NC1 through the door, and §10's block pinning with it: a pool whose only `Sync`
/// is one block after the target cannot be priced at it, and no request of the scan
/// ever named that block.
#[tokio::test]
async fn a_sync_after_the_target_keeps_the_pool_out_of_the_graph_through_the_door() {
    let verified_pool = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let run = reconstruct(
        vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 121, 9)],
        &[(PAIR, 100)],
        120,
        10,
    )
    .await;
    assert_eq!(
        run.row(pool(PAIR)).expect("row").outcome(),
        PoolStateAtTarget::NothingPublished,
        "the scan's own answer is that nothing was published at or before the target"
    );

    let state = integrate_at_target(ChainId(CHAIN), &Registry::default(), &[verified_pool], &run)
        .expect("integration");
    assert!(state.store_rejections.is_empty());
    assert_eq!(state.snapshot.block_number(), None);
    assert!(
        matches!(state.graph, GraphOutcome::NoStateApplied),
        "a pool whose only Sync is in the future prices nothing at the target"
    );
}

/// §17's "the store's refusals are data" at the new door: a reconstructed `Sync` that
/// publishes an empty side is refused by the state layer, recorded, and stays out of
/// the graph — while a second pool in the same run still prices.
#[tokio::test]
async fn a_reconstructed_empty_side_is_recorded_not_silently_dropped() {
    let empty = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let priced = verified_at(TOKEN_C, TOKEN_B, PAIR_2, 100, 5);
    let run = reconstruct(
        vec![
            sync_log(PAIR, U256::ZERO, U256::from(R1), 105, 7),
            sync_log(PAIR_2, U256::from(R0), U256::from(R1), 106, 3),
        ],
        &[(PAIR, 100), (PAIR_2, 100)],
        120,
        1_000,
    )
    .await;
    assert_eq!(run.counted(PoolStateAtTarget::EmptyReserves), 1);

    let state = integrate_at_target(ChainId(CHAIN), &Registry::default(), &[empty, priced], &run)
        .expect("integration");
    assert_eq!(state.store_rejections.len(), 1);
    assert_eq!(state.store_rejections[0].pool, pool(PAIR));
    assert_eq!(state.store_rejections[0].rule, "empty_reserves");
    let build = match &state.graph {
        GraphOutcome::Built(build) => build,
        GraphOutcome::NoStateApplied => panic!("the valid pool applied a position"),
    };
    assert_eq!(build.graph.pool_count(), 1);
    assert_eq!(
        skipped_reasons(build),
        vec![(pool(PAIR), SkipReason::StateUnavailable)]
    );
}

/// §33's regression half for this file: the door M9.1 built still behaves exactly as
/// M9.1 published. It hands the store the candidate's own `Sync`, sets no target, and
/// the graph is that block's — which is why M9.1 read 24 of 80.
#[test]
fn the_original_door_still_builds_the_graph_at_its_own_sync_block() {
    let verified_pool = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let state =
        integrate(ChainId(CHAIN), &Registry::default(), &[verified_pool]).expect("integration");

    assert_eq!(state.snapshot.target_block(), None);
    assert_eq!(state.snapshot.block_number(), Some(block(100)));
    let build = match &state.graph {
        GraphOutcome::Built(build) => build,
        GraphOutcome::NoStateApplied => panic!("the candidate synced at its claim"),
    };
    assert_eq!(build.graph.block_number(), block(100));
    assert_eq!(build.graph.pool_count(), 1);
    assert!(build.skipped.is_empty(), "{:?}", build.skipped);
}

/// §27 at the door, one step further than the scan: two calls on the same inputs
// return values that compare equal, including the graph and the projection.
#[tokio::test]
async fn two_door_calls_on_the_same_inputs_agree_entirely() {
    let verified_pool = verified_at(TOKEN_A, TOKEN_B, PAIR, 100, 4);
    let run = reconstruct(
        vec![sync_log(PAIR, U256::from(R0), U256::from(R1), 105, 9)],
        &[(PAIR, 100)],
        120,
        1_000,
    )
    .await;
    let first = integrate_at_target(
        ChainId(CHAIN),
        &Registry::default(),
        std::slice::from_ref(&verified_pool),
        &run,
    )
    .expect("run A");
    let second = integrate_at_target(ChainId(CHAIN), &Registry::default(), &[verified_pool], &run)
        .expect("run B");

    assert_eq!(first, second, "the whole of a run, not just its row count");
}

// ── Fixtures for the state layer ────────────────────────────────────────────

/// A store with the given pools registered — `(pool, token X, token Y)`, sorted into
/// `token0`/`token1` the way the registry does it — and the given `Sync` records
/// applied as `(pool, block, log index, reserve0, reserve1)`, fed in ascending chain
/// position the way `integrate` feeds them.
fn store_with(
    registrations: &[(Address, Address, Address)],
    syncs: &[(Address, u64, u64, u64, u64)],
) -> InMemoryStateStore {
    use evm_core::{PoolMeta, PoolType, ProtocolId, TokenId};
    let chain = ChainId(CHAIN);
    let mut store = InMemoryStateStore::new(chain);
    for (pair, left, right) in registrations {
        store
            .apply(StateUpdate::PoolRegistered(PoolMeta {
                id: pool(*pair),
                protocol: ProtocolId::new("v2-compatible"),
                token0: TokenId::new(chain, (*left).min(*right)),
                token1: TokenId::new(chain, (*left).max(*right)),
                fee: None,
                pool_type: PoolType::ConstantProduct,
            }))
            .expect("registered");
    }
    let mut order: Vec<usize> = (0..syncs.len()).collect();
    order.sort_by_key(|index| (syncs[*index].1, syncs[*index].2));
    for index in order {
        let (pair, block_number, log_index, reserve0, reserve1) = syncs[index];
        store
            .apply(StateUpdate::PoolSynced {
                pool: pool(pair),
                reserve0: U256::from(reserve0),
                reserve1: U256::from(reserve1),
                position: UpdatePosition::new(block(block_number), LogIndex(log_index)),
            })
            .expect("applied");
    }
    store
}

fn traced(snapshot: &StateSnapshot) -> GraphBuild {
    MarketGraphBuilder::new()
        .build_traced(snapshot)
        .expect("the snapshot has an applied position")
}

fn skipped_reasons(build: &GraphBuild) -> Vec<(PoolId, SkipReason)> {
    build
        .skipped
        .iter()
        .map(|row| (row.pool, row.reason))
        .collect()
}
