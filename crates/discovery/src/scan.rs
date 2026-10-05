//! Reading the chain's own creation log over a historical range.
//!
//! The whole mechanism is: ask for `PairCreated` topic0 in a block range, decode
//! it, keep the claims. No DEX list is consulted (M9.1 §8) — `addresses` in the
//! filter is deliberately empty, so an address nobody has ever heard of still
//! comes back. A factory allowlist here would be the same manually maintained
//! list this milestone exists to stop depending on.
//!
//! Two node limits shape the loop, both measured rather than assumed:
//! `data/evidence/m7/census-pair-created.json` scanned this chain's whole history
//! in 4,999-block chunks and found 1,030 `PairCreated` logs, so the chunk size
//! stays at that value; the per-request result ceiling below is what turns a
//! possibly-truncated answer into an error rather than a shorter census.

use alloy_primitives::{Address, Bytes, B256};
use serde::{Deserialize, Serialize};

use evm_chain::{ChainAdapter, ChainLog, LogFilter};
use evm_core::{BlockNumber, ChainId, PoolId};
use evm_protocol::{ProtocolAdapter, ProtocolError, V2Topics};

use crate::candidate::{CandidatePool, DiscoverySource};
use crate::error::{DiscoveryError, Result};

/// Blocks per `eth_getLogs` request. 4,999 is the value the M7 full-chain census
/// used successfully; changing it means re-measuring the node, not re-deciding.
pub const CHUNK_BLOCKS: u64 = 4_999;

/// The node's own per-request result ceiling.
///
/// Reaching it means the answer was cut off, so the census is incomplete — which
/// is why [`HistoricalPairCreatedSource::scan_window`] turns it into an error
/// instead of a footnote. A silent truncation is the one scan failure that looks
/// exactly like a successful empty one.
pub const NODE_LOG_LIMIT: usize = 20_000;

/// One chunk of the scan, recorded whether or not it found anything: a window
/// that returned zero logs is a fact about the chain, and an evidence table that
/// only listed productive windows would overstate how much was looked at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanWindow {
    pub from_block: BlockNumber,
    pub to_block: BlockNumber,
    pub logs_returned: usize,
}

/// A log that carried the `PairCreated` topic0 and could not be decoded.
///
/// Position + shape + the decoder's own words. Array index is nowhere in this
/// identity: the same bad log found by two runs has to land in the same row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MalformedLog {
    pub block_number: BlockNumber,
    pub tx_index: u64,
    pub log_index: u64,
    pub address: Address,
    pub topics: usize,
    pub data_len: usize,
    pub reason: String,
}

impl MalformedLog {
    fn of(log: &ChainLog, reason: &ProtocolError) -> Self {
        Self {
            block_number: log.block_number,
            tx_index: log.tx_index.0,
            log_index: log.log_index.0,
            address: log.address,
            topics: log.topics.len(),
            data_len: log.data.len(),
            reason: reason.to_string(),
        }
    }

    /// Sort key: the log's chain position, which is unique on its own.
    fn sort_key(&self) -> (u64, u64, Address) {
        (self.block_number.0, self.log_index, self.address)
    }
}

/// Everything one scan produced, in the shapes evidence is written from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    pub chain_id: ChainId,
    pub from_block: BlockNumber,
    pub to_block: BlockNumber,
    pub source: DiscoverySource,
    pub windows: Vec<ScanWindow>,
    /// Raw `PairCreated` logs exactly as the node returned them, sorted by chain
    /// position. Committed as evidence so the candidate set below can be
    /// re-derived offline — a reviewer, or a negative control, needs no node.
    pub raw_logs: Vec<ChainLog>,
    pub candidates: Vec<CandidatePool>,
    /// One entry per rejected raw log: what it was, and why it was rejected.
    pub malformed_logs: Vec<MalformedLog>,
    /// Logs seen twice because two windows covered the same block. Zero for a
    /// well-formed range, and asserted so rather than assumed.
    pub duplicate_logs: usize,
}

impl ScanReport {
    pub fn blocks_covered(&self) -> u64 {
        self.windows
            .iter()
            .map(|window| window.to_block.0 - window.from_block.0 + 1)
            .sum()
    }

    pub fn logs_returned(&self) -> usize {
        self.windows.iter().map(|window| window.logs_returned).sum()
    }

    /// Distinct pool addresses the claims name.
    ///
    /// Deliberately separate from `candidates.len()`: one address claimed twice
    /// (re-created, or by two factories) is one pool and two candidates, and a
    /// table that called either number "how many pools were found" would be wrong
    /// half the time.
    pub fn distinct_pools(&self) -> usize {
        let mut pools: Vec<PoolId> = self.candidates.iter().map(|c| c.pool).collect();
        pools.sort();
        pools.dedup();
        pools.len()
    }

    /// Re-derive candidates from raw logs, with no chain involved.
    ///
    /// The same function the live scan calls, split out so the RPC loop and the
    /// decode/dedup decision can be tested and recomputed independently. Dedup is
    /// by [`CandidatePool::identity`] — chain, pool address, and the exact log the
    /// claim came from — never by list position (M9.1 §7).
    pub fn decode_claims(
        adapter: &dyn ProtocolAdapter,
        raw_logs: &[ChainLog],
    ) -> (Vec<CandidatePool>, Vec<MalformedLog>) {
        let mut candidates: Vec<CandidatePool> = Vec::new();
        let mut malformed: Vec<MalformedLog> = Vec::new();
        for log in raw_logs {
            match adapter.decode_log(log) {
                Ok(Some(event)) => {
                    // The filter asked one question; a node that answers more than
                    // that is not wrong, just broader. Any other protocol event is
                    // neither a claim nor a rejection.
                    if let evm_protocol::ProtocolEvent::PoolCreated(claim) = event {
                        let candidate = CandidatePool::from_claim(
                            &claim,
                            DiscoverySource::HistoricalPairCreated,
                        );
                        if !candidates
                            .iter()
                            .any(|seen| seen.identity() == candidate.identity())
                        {
                            candidates.push(candidate);
                        }
                    }
                }
                // Not this protocol's event shape at all — a log the topic0 filter
                // let through that the adapter does not recognize.
                Ok(None) => {}
                Err(err) => malformed.push(MalformedLog::of(log, &err)),
            }
        }
        candidates.sort();
        malformed.sort_by_key(MalformedLog::sort_key);
        (candidates, malformed)
    }
}

/// The first — and so far only — discovery source.
///
/// Not behind a trait: §5 asks for "a small abstraction" and warns against
/// premature generic frameworks, so the abstraction here is the *output type*.
/// Anything that discovers produces `Vec<CandidatePool>` tagged with a
/// [`DiscoverySource`]. A Flashblocks source is a sibling struct with its own loop
/// (§24), and nothing downstream of the candidate list has to change for it.
pub struct HistoricalPairCreatedSource {
    chunk_blocks: u64,
    topics: V2Topics,
}

impl Default for HistoricalPairCreatedSource {
    fn default() -> Self {
        Self {
            chunk_blocks: CHUNK_BLOCKS,
            topics: V2Topics::default(),
        }
    }
}

impl HistoricalPairCreatedSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn source(&self) -> DiscoverySource {
        DiscoverySource::HistoricalPairCreated
    }

    pub fn chunk_blocks(&self) -> u64 {
        self.chunk_blocks
    }

    /// For a node whose measured limit differs from this one's. Zero is refused:
    /// a zero-width chunk either loops forever or scans nothing, and neither is
    /// worth discovering in the middle of a live run.
    pub fn with_chunk_blocks(mut self, chunk_blocks: u64) -> Result<Self> {
        if chunk_blocks == 0 {
            return Err(DiscoveryError::Configuration(
                "a chunk of zero blocks is not a range".to_string(),
            ));
        }
        self.chunk_blocks = chunk_blocks;
        Ok(self)
    }

    /// The filter this source asks with: one topic, no addresses.
    pub fn filter_for(&self, from: BlockNumber, to: BlockNumber) -> LogFilter {
        LogFilter {
            from_block: from,
            to_block: to,
            addresses: Vec::new(),
            topics: vec![Some(vec![self.topics.pair_created])],
        }
    }

    /// Scan `from..=to` for `PairCreated` and turn what it finds into candidates.
    ///
    /// The `adapter` is the one the replay and live pipelines already use, so
    /// discovery cannot grow a decoder production does not have: a log this scan
    /// rejects, the pipeline rejects, and the other way round.
    ///
    /// A node error stops the run. A malformed log does not: it is a fact about
    /// one log on a chain nobody controls, and dropping a whole census because one
    /// record was unreadable would lose the logs that were readable.
    pub async fn scan<A: ChainAdapter + ?Sized>(
        &self,
        chain: &A,
        adapter: &dyn ProtocolAdapter,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<ScanReport> {
        let chain_id = chain.chain_id();
        if from > to {
            return Err(DiscoveryError::Configuration(format!(
                "range {}..={} is inverted",
                from.0, to.0
            )));
        }

        let mut windows: Vec<ScanWindow> = Vec::new();
        let mut raw_logs: Vec<ChainLog> = Vec::new();
        let mut cursor = from.0;
        while cursor <= to.0 {
            let last = cursor.saturating_add(self.chunk_blocks - 1).min(to.0);
            let logs = self
                .scan_window(chain, BlockNumber(cursor), BlockNumber(last))
                .await?;
            windows.push(ScanWindow {
                from_block: BlockNumber(cursor),
                to_block: BlockNumber(last),
                logs_returned: logs.len(),
            });
            for log in logs {
                if log.chain_id != chain_id {
                    // A provider that answers with another chain's records is not
                    // offering history, it is offering a different chain. Mixing
                    // them would put two chains' identities in one candidate list,
                    // which is what TokenId and PoolId exist to prevent (§11).
                    return Err(DiscoveryError::Protocol(ProtocolError::MalformedLog(
                        format!(
                            "chain {} answered with a log from chain {} at block {}",
                            chain_id.0, log.chain_id.0, log.block_number.0
                        ),
                    )));
                }
                raw_logs.push(log);
            }
            cursor = last + 1;
        }

        let (deduped, duplicate_logs) = sort_and_dedup_logs(raw_logs);
        let (candidates, malformed_logs) = ScanReport::decode_claims(adapter, &deduped);
        Ok(ScanReport {
            chain_id,
            from_block: from,
            to_block: to,
            source: self.source(),
            windows,
            raw_logs: deduped,
            candidates,
            malformed_logs,
            duplicate_logs,
        })
    }

    async fn scan_window<A: ChainAdapter + ?Sized>(
        &self,
        chain: &A,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<ChainLog>> {
        let logs = chain.get_logs(self.filter_for(from, to)).await?;
        refuse_truncated_window(from, to, logs.len())?;
        Ok(logs)
    }
}

/// Turn an answer that filled the node's result ceiling into an error.
///
/// Shared with [`crate::reconstruct`], which asks a different question of the same
/// node and has the same failure available to it: a truncated `Sync` scan reads
/// exactly like a scan that found nothing, and "no later `Sync`" is the evidence a
/// target-block projection rests on.
pub(crate) fn refuse_truncated_window(
    from: BlockNumber,
    to: BlockNumber,
    returned: usize,
) -> Result<()> {
    if returned >= NODE_LOG_LIMIT {
        return Err(DiscoveryError::Configuration(format!(
            "blocks {}..={} returned {} logs, which is the node's per-request ceiling: the \
             range has to be narrowed, because a truncated census is not a census",
            from.0, to.0, returned
        )));
    }
    Ok(())
}

/// Sort logs by chain position and drop repeats, counting what was dropped.
///
/// Two windows covering one block (an overlapping range, or a node that answers a
/// narrower question with a wider answer) make a log appear twice. Position is the
/// identity that decides, so the count is exact rather than approximate.
pub(crate) fn sort_and_dedup_logs(logs: Vec<ChainLog>) -> (Vec<ChainLog>, usize) {
    let mut sorted = logs;
    sorted.sort();
    let mut deduped: Vec<ChainLog> = Vec::with_capacity(sorted.len());
    let mut duplicate_logs = 0usize;
    for log in sorted {
        let repeat = deduped.last().is_some_and(|last| {
            last.block_number == log.block_number && last.log_index == log.log_index
        });
        if repeat {
            duplicate_logs += 1;
        } else {
            deduped.push(log);
        }
    }
    (deduped, duplicate_logs)
}

/// The bytes of a raw log in the shape the evidence file writes them.
///
/// A function rather than a `Serialize` impl on `ChainLog`, because the two forms
/// have different jobs: the working form is compared, sorted and decoded, the
/// evidence form has to read correctly for someone who is not running this
/// binary.
pub fn log_record(log: &ChainLog) -> LogRecord {
    LogRecord {
        chain_id: log.chain_id.0,
        block_number: log.block_number.0,
        tx_hash: log.tx_hash.to_string(),
        tx_index: log.tx_index.0,
        log_index: log.log_index.0,
        address: log.address.to_string(),
        topics: log.topics.iter().map(|topic| topic.to_string()).collect(),
        data: hex_of(&log.data),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub chain_id: u64,
    pub block_number: u64,
    pub tx_hash: String,
    pub tx_index: u64,
    pub log_index: u64,
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
}

/// Lowercase `0x`-prefixed hex, written by hand rather than pulled from a
/// dependency: this project already has `hex` in the workspace, and using it here
/// would mean a discovery crate depending on something it does not otherwise need.
pub fn hex_of(bytes: &Bytes) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for byte in bytes.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The topic0 a scan filters on — exposed so evidence can name the question that
/// was asked instead of leaving a reader to re-derive a hash to check one.
pub fn pair_created_topic0() -> B256 {
    V2Topics::default().pair_created
}
