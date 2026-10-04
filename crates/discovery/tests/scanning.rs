//! The scan loop, tested against a scripted node that answers only `eth_getLogs`.
//!
//! §26 asks for unit tests that do not need a provider, and the half of discovery
//! that most needs them is the windowing: a scan that quietly drops a chunk, accepts
//! a truncated answer, or trusts a node's ordering would still produce a plausible
//! candidate list. So the node here is a fixture, and the assertions are about the
//! requests the scan makes and what it refuses to accept back.
//!
//! The §20 "malformed PairCreated data" control lives here, because malformed is a
//! statement about a log, not about a contract.

mod fixtures;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use alloy_primitives::{Address, Bytes, U256};
use async_trait::async_trait;

use evm_chain::{
    BlockContext, BlockData, CallRequest, ChainAdapter, ChainBlock, ChainError, ChainLog, LogFilter,
};
use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};
use evm_discovery::{
    hex_of, log_record, pair_created_topic0, DiscoveryError, HistoricalPairCreatedSource,
    ScanReport, CHUNK_BLOCKS, NODE_LOG_LIMIT,
};
use evm_protocol::{Registry, V2Adapter, V2Topics};

use fixtures::{
    pair_created_log, window_filter, BLOCK, CHAIN, FACTORY, OTHER_FACTORY, PAIR, PAIR_2, TOKEN_A,
    TOKEN_B,
};

/// A node with a fixed log set, which answers `eth_getLogs` by range and topic the
/// way a real one does.
///
/// Every other method errors, and that is load-bearing: the scan is supposed to ask
/// one question of the chain. If the window loop ever starts making contract reads,
/// these tests stop instead of measuring a different RPC count than the evidence
/// claims.
struct ScriptedNode {
    logs: Vec<ChainLog>,
    /// A node that answers every window with the whole set, as an overlapping or
    /// misbehaving provider does. Real risk: two windows covering one block.
    ignores_ranges: bool,
    requests: AtomicUsize,
    asked: Mutex<Vec<LogFilter>>,
}

impl ScriptedNode {
    fn new(logs: Vec<ChainLog>) -> Self {
        Self {
            logs,
            ignores_ranges: false,
            requests: AtomicUsize::new(0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn leaking(logs: Vec<ChainLog>) -> Self {
        Self {
            ignores_ranges: true,
            ..Self::new(logs)
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// `(from, to, how_many_addresses)` per request, in the order they were made.
    fn asked(&self) -> Vec<(u64, u64, usize)> {
        self.filters()
            .iter()
            .map(|filter| {
                (
                    filter.from_block.0,
                    filter.to_block.0,
                    filter.addresses.len(),
                )
            })
            .collect()
    }

    fn filters(&self) -> Vec<LogFilter> {
        self.asked.lock().unwrap().clone()
    }
}

fn answers(filter: &LogFilter, log: &ChainLog) -> bool {
    if log.block_number < filter.from_block || log.block_number > filter.to_block {
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
    ChainError::MissingData(format!("discovery may not ask {method}"))
}

#[async_trait]
impl ChainAdapter for ScriptedNode {
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
        Err(unavailable("eth_getBlockByNumber (data)"))
    }

    async fn get_block_context(
        &self,
        _number: BlockNumber,
    ) -> std::result::Result<BlockContext, ChainError> {
        Err(unavailable("eth_getBlockByNumber (context)"))
    }

    async fn get_logs(&self, filter: LogFilter) -> std::result::Result<Vec<ChainLog>, ChainError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(filter.clone());
        if self.ignores_ranges {
            return Ok(self.logs.clone());
        }
        Ok(self
            .logs
            .iter()
            .filter(|log| answers(&filter, log))
            .cloned()
            .collect())
    }

    async fn call(
        &self,
        _at: BlockNumber,
        _request: &CallRequest,
    ) -> std::result::Result<Bytes, ChainError> {
        Err(unavailable("eth_call"))
    }

    async fn get_code(
        &self,
        _at: BlockNumber,
        _address: Address,
    ) -> std::result::Result<Bytes, ChainError> {
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

fn adapter() -> V2Adapter {
    V2Adapter::new(Registry::default())
}

fn source(chunk_blocks: u64) -> HistoricalPairCreatedSource {
    HistoricalPairCreatedSource::new()
        .with_chunk_blocks(chunk_blocks)
        .unwrap()
}

/// §8: the question goes to the chain, not to a list.
#[test]
fn the_filter_names_one_topic_and_no_addresses() {
    let asked = source(CHUNK_BLOCKS).filter_for(BlockNumber(100), BlockNumber(200));
    assert_eq!(asked, window_filter(100, 200));
    assert!(
        asked.addresses.is_empty(),
        "a factory allowlist is a DEX list"
    );
    assert_eq!(
        asked.topics,
        vec![Some(vec![pair_created_topic0()])],
        "the scan asks exactly one question"
    );
}

/// The two node limits are measurements, not guesses, and the chunk has to stay
/// inside the range ceiling the node enforces.
#[test]
fn the_window_size_is_the_one_this_node_was_measured_with() {
    assert_eq!(CHUNK_BLOCKS, 4_999);
    const {
        assert!(
            CHUNK_BLOCKS <= 10_000,
            "the node refuses a 10,001-block range"
        );
    }
    assert_eq!(NODE_LOG_LIMIT, 20_000);
}

#[tokio::test]
async fn a_scan_asks_one_request_per_window_and_covers_the_whole_range() {
    let chain = ScriptedNode::new(vec![
        pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, 105, 1),
        pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR_2, 121, 2),
    ]);
    let report = source(10)
        .scan(&chain, &adapter(), BlockNumber(100), BlockNumber(124))
        .await
        .unwrap();

    assert_eq!(
        report
            .windows
            .iter()
            .map(|w| (w.from_block.0, w.to_block.0))
            .collect::<Vec<_>>(),
        vec![(100, 109), (110, 119), (120, 124)]
    );
    assert_eq!(report.blocks_covered(), 25);
    assert_eq!(report.logs_returned(), 2);
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.duplicate_logs, 0);
    assert_eq!(chain.requests(), 3);
    // Every window was asked with the same shape the filter declares — no address
    // list creeps in at the second chunk.
    assert_eq!(
        chain.asked(),
        vec![(100, 109, 0), (110, 119, 0), (120, 124, 0)]
    );
}

#[tokio::test]
async fn an_impossible_range_or_chunk_is_reported_not_clamped() {
    let chain = ScriptedNode::new(Vec::new());
    let err = source(10)
        .scan(&chain, &adapter(), BlockNumber(200), BlockNumber(100))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DiscoveryError::Configuration(_)));
    assert!(err.to_string().contains("inverted"));
    assert_eq!(
        chain.requests(),
        0,
        "an inverted range must not touch the node"
    );
    assert!(matches!(
        HistoricalPairCreatedSource::new().with_chunk_blocks(0),
        Err(DiscoveryError::Configuration(_))
    ));
}

/// The one scan failure that looks exactly like a successful empty one.
#[tokio::test]
async fn a_window_at_the_node_s_ceiling_is_an_error_not_a_shorter_census() {
    let logs: Vec<ChainLog> = (0..NODE_LOG_LIMIT)
        .map(|i| pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, i as u64))
        .collect();
    let chain = ScriptedNode::new(logs);
    let err = source(CHUNK_BLOCKS)
        .scan(&chain, &adapter(), BlockNumber(BLOCK), BlockNumber(BLOCK))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DiscoveryError::Configuration(_)));
    let message = err.to_string();
    assert!(message.contains(&NODE_LOG_LIMIT.to_string()));
    assert!(message.contains("truncated"), "{message}");
}

/// A provider that answers with another chain's records is offering a different
/// chain, not history (§11: one identity per candidate).
#[tokio::test]
async fn a_cross_chain_answer_stops_the_run() {
    let mut log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 1);
    log.chain_id = ChainId(31337);
    let chain = ScriptedNode::new(vec![log]);
    let err = source(CHUNK_BLOCKS)
        .scan(&chain, &adapter(), BlockNumber(BLOCK), BlockNumber(BLOCK))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DiscoveryError::Protocol(_)), "{err}");
    assert!(err.to_string().contains("91342"));
    assert!(err.to_string().contains("31337"));
}

/// Two windows covering one block is a census that counts a pool twice. The scan
/// notices, and says how many times.
#[tokio::test]
async fn a_log_the_node_returns_twice_is_counted_once() {
    let log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 1);
    let chain = ScriptedNode::leaking(vec![log]);
    let report = source(4)
        .scan(
            &chain,
            &adapter(),
            BlockNumber(BLOCK),
            BlockNumber(BLOCK + 7),
        )
        .await
        .unwrap();

    assert_eq!(report.duplicate_logs, 1, "two windows, one log");
    assert_eq!(report.raw_logs.len(), 1);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.windows.len(), 2);
}

/// One address claimed by two logs is one pool and two candidates — a table that
/// called either number "how many pools were found" would be wrong (§7).
#[tokio::test]
async fn two_claims_of_one_address_are_two_candidates_and_one_pool() {
    let chain = ScriptedNode::new(vec![
        pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 1),
        pair_created_log(OTHER_FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK + 3, 2),
    ]);
    let report = source(CHUNK_BLOCKS)
        .scan(
            &chain,
            &adapter(),
            BlockNumber(BLOCK),
            BlockNumber(BLOCK + 3),
        )
        .await
        .unwrap();

    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.distinct_pools(), 1);
    assert_ne!(report.candidates[0].factory, report.candidates[1].factory);
}

/// §20 "malformed PairCreated data". A log that carries the topic0 and cannot be
/// decoded is a finding about that log; it is never a candidate, and it never
/// reaches a registry.
#[test]
fn malformed_pair_created_data_produces_a_row_and_no_candidate() {
    let adapter = adapter();
    let shapes = [
        // The inline encoding this chain emits, with the declared encoding's data.
        (3usize, 32usize),
        // The declared encoding with this chain's data.
        (4, 64),
        // A log too short to name both tokens.
        (2, 64),
    ];
    let mut logs = Vec::new();
    for (index, (topics, data_len)) in shapes.iter().enumerate() {
        let mut log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, index as u64);
        log.topics.truncate(*topics);
        if *topics == 4 {
            // The declared encoding's fourth topic, so the shape under test is the
            // data length and not a missing topic.
            log.topics.push(pair_created_topic0());
        }
        log.data = Bytes::from(vec![7u8; *data_len]);
        logs.push(log);
    }
    // One well-formed log among them, to prove the malformed rows are not a
    // blanket rejection of the batch.
    logs.push(pair_created_log(
        FACTORY, TOKEN_A, TOKEN_B, PAIR_2, BLOCK, 9,
    ));

    let (candidates, malformed) = ScanReport::decode_claims(&adapter, &logs);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].pool.address, PAIR_2);
    assert_eq!(malformed.len(), 3);
    for (row, (topics, data_len)) in malformed.iter().zip(shapes.iter()) {
        assert_eq!(row.topics, *topics);
        assert_eq!(row.data_len, *data_len);
        assert!(!row.reason.is_empty(), "a rejection with no reason");
    }
    let indexes: Vec<u64> = malformed.iter().map(|row| row.log_index).collect();
    assert_eq!(indexes, vec![0, 1, 2], "rows stay in chain order");
}

/// A log the filter let through that is simply another protocol event is neither a
/// claim nor a rejection.
#[test]
fn an_event_from_another_part_of_the_protocol_is_neither_claim_nor_rejection() {
    let mut log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 1);
    log.topics[0] = V2Topics::default().sync;
    log.data = Bytes::from(vec![0u8; 64]);

    let (candidates, malformed) = ScanReport::decode_claims(&adapter(), std::slice::from_ref(&log));
    assert!(candidates.is_empty());
    assert!(
        malformed.is_empty(),
        "a log that is not this event is not a malformed one: {malformed:?}"
    );
}

/// §21, offline half: the same raw logs in any order produce byte-identical rows.
#[test]
fn decoding_the_same_logs_in_any_order_gives_the_same_rows() {
    let logs = vec![
        pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK + 4, 1),
        pair_created_log(OTHER_FACTORY, TOKEN_A, TOKEN_B, PAIR_2, BLOCK, 2),
        pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 3),
    ];
    let mut reversed = logs.clone();
    reversed.reverse();

    let (ours, _) = ScanReport::decode_claims(&adapter(), &logs);
    let (theirs, _) = ScanReport::decode_claims(&adapter(), &reversed);
    assert_eq!(ours, theirs);
    assert_eq!(
        serde_json::to_string(&ours).unwrap(),
        serde_json::to_string(&theirs).unwrap()
    );
    // And the order is claim identity, not arrival order: pool address first, so a
    // second run on a different node ordering lands on the same rows.
    let mut by_identity = ours.clone();
    by_identity.sort_by_key(|c| c.identity());
    assert_eq!(ours, by_identity);
    assert_eq!(ours.first().unwrap().pool.address, PAIR);
}

/// A candidate is the decoded claim, so identity is available before verification.
#[test]
fn a_decoded_claim_carries_its_own_provenance() {
    let log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 6);
    let (candidates, _) = ScanReport::decode_claims(&adapter(), std::slice::from_ref(&log));
    let candidate = candidates.first().unwrap();

    assert_eq!(candidate.pool.address, PAIR);
    assert_eq!(candidate.factory, FACTORY);
    assert_eq!(candidate.discovered_at.log_index, LogIndex(6));
    assert_eq!(candidate.discovered_at.tx_index, TxIndex(1));
    assert_eq!(candidate.pair_index, U256::from(41u32));
    assert_eq!(candidate.identity(), candidate.sort_key());
    // The claim's tokens are the factory's words, not a read answer.
    assert_eq!(candidate.claimed_token0.address, TOKEN_A);
    assert_eq!(candidate.discovered_at.tx_hash, TxHash(log.tx_hash.0));
}

/// The evidence row form: raw bytes as hex, chain position as numbers, and the
/// topic0 the filter asked for written out.
#[test]
fn a_log_record_writes_the_bytes_an_auditor_needs() {
    let log = pair_created_log(FACTORY, TOKEN_A, TOKEN_B, PAIR, BLOCK, 6);
    let record = log_record(&log);

    assert_eq!(record.chain_id, CHAIN);
    assert_eq!(record.block_number, BLOCK);
    assert_eq!(record.log_index, 6);
    assert_eq!(record.address, format!("{FACTORY}"));
    assert_eq!(record.topics[0], pair_created_topic0().to_string());
    assert_eq!(record.topics.len(), 3);
    assert_eq!(record.data, hex_of(&log.data));
    assert_eq!(record.data.len(), 2 + 64 * 2);
}
