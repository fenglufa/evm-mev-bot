//! What a verified pool's state was at an explicit target block.
//!
//! M9.1 ended with 80 verified pools and 24 edges. The other 56 were not rejected:
//! their newest `Sync` was simply from an older block than the block the graph was
//! built at, and the graph's rule — a pool prices the block its own state was
//! published in, or it prices nothing — kept them out. That rule is correct and
//! M9.2 §2 forbids relaxing it. What was missing is the evidence that lets a pool
//! whose `Sync` is older still answer for a later block, which is a question about
//! the blocks in between:
//!
//! ```text
//! the pool's latest authoritative Sync S <= T, and
//! a complete Sync scan of that pool covering (S, T] that found nothing
//! ```
//!
//! Both halves are chain reads, so this module is the half of M9.2 that talks to a
//! node. It produces [`PoolSyncAtTarget`] rows — one per pool, carrying the selected
//! `Sync` and the range that vouches for it — and nothing else: no state, no graph,
//! no verdict about whether a pool is worth trading. Projecting those rows into a
//! snapshot is `StateSnapshot::at_target`, and admitting them into a graph is the
//! builder's own predicate; neither lives here.
//!
//! Two rules shape the mechanics:
//!
//! - **`Sync` only, never `eth_call`** (§5). The whole point is the pool's own
//!   publication; `getReserves()` at the target would be a contract answer standing
//!   in for a market event. It is not merely unused here but unavailable — this
//!   module asks the chain exactly one question, `eth_getLogs` — and the state it
//!   hands over is built through [`AuthoritativeMarketState`], the only path from a
//!   `Sync` record to a `PoolState`.
//! - **A scan is a range, and the range is stated** (§4, §11). Every request is
//!   `from..=to` with `to <= target`, so no block after the target can be seen at
//!   all, and `from` is never genesis: it is the pool's own discovery block, the
//!   earliest height at which that address could have emitted anything (§8).
//!
//! # Two ways to ask, and why both exist
//!
//! [`HistoricalSyncSource::scan_pool`] asks per pool (`address = pool`,
//! `topic0 = Sync`) and walks *backwards* from the target, stopping at the first
//! chunk that answers. The early stop is not a shortcut past the evidence — it *is*
//! the evidence: the blocks between the `Sync` it found and the target are exactly
//! the ones this scan read, so no later `Sync` exists in the interval. Its cost is
//! per pool, so one dead pool whose last trade was millions of blocks back pays full
//! price for all of them.
//!
//! [`HistoricalSyncSource::census`] asks once for the whole chain (`addresses`
//! empty, `topic0 = Sync`) over one range and groups the answer per pool. One pass
//! covers every pool and every block up to the target, but it also carries every
//! other pool's logs, and the node's per-request ceiling decides how narrow its
//! chunks have to be.
//!
//! Which is cheaper is a property of the node and of this chain's activity rather
//! than of the code, so §12 settles it by running both against real data: the `Sync`
//! each strategy selects must be identical, and the request and log counts are what
//! picks between them. Correctness is not on the bidding side — the scan that does
//! not prove its interval is not cheaper, it is wrong.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use evm_chain::{ChainAdapter, ChainLog, LogFilter};
use evm_core::{BlockNumber, ChainId, PoolId, PoolState};
use evm_protocol::V2Topics;
use evm_state::{SyncScanCoverage, UpdatePosition};

use crate::error::{DiscoveryError, Result};
use crate::reads::{sync_record_of, SyncRecord};
use crate::scan::{refuse_truncated_window, sort_and_dedup_logs, ScanWindow, CHUNK_BLOCKS};
use crate::verify::AuthoritativeMarketState;

/// One pool's state at one target block, as the chain described it.
///
/// Everything in here is either a block range this code asked the node for or a
/// record the node returned. The verdict is not stored: it is
/// [`PoolSyncAtTarget::outcome`], computed from the two, so a row cannot state a
/// conclusion its own evidence does not support.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolSyncAtTarget {
    pub pool: PoolId,
    /// The block this row answers for.
    pub target: BlockNumber,
    /// The lower bound the scan was told to respect: the pool's discovery block, the
    /// earliest height at which the address could have emitted a `Sync` (§8). A
    /// backward scan that stops early never reaches it, so this and `search_from` are
    /// two different facts and both are kept.
    pub discovery_block: BlockNumber,
    /// Lowest block actually scanned, inclusive.
    pub search_from: BlockNumber,
    /// Highest block actually scanned, inclusive — always the target, which is what
    /// makes "no future leakage" checkable from this row alone.
    pub search_to: BlockNumber,
    /// `Sync` logs this pool emitted inside `search_from..=search_to`. Separate from
    /// the selection because "saw 4, kept the last" and "saw 1" are different
    /// statements about a pool's activity.
    pub sync_logs_seen: usize,
    /// The latest of those by chain position, if any. Carries block, tx index and log
    /// index, so §6's provenance survives without a second ordering convention (§7).
    pub selected: Option<SyncRecord>,
    /// `eth_getLogs` requests this scan made. For a row produced by a chain-wide
    /// census these describe the shared pass and are identical on every row of it —
    /// compare [`Reconstruction::chunks_scanned`], never a sum over rows.
    pub chunks_scanned: usize,
    /// Logs that came back across those requests. Same census caveat as above.
    pub logs_returned: usize,
}

impl PoolSyncAtTarget {
    /// Row identity: which pool, at which target block. Never a list position — two
    /// runs have to agree on which rows are the same row to be comparable.
    pub fn identity(&self) -> (u64, Address, u64) {
        (self.pool.chain_id.0, self.pool.address, self.target.0)
    }

    /// The scan as coverage evidence: nowhere in here did this pool's state change
    /// except at the selected record.
    pub fn coverage(&self) -> Option<SyncScanCoverage> {
        SyncScanCoverage::new(self.search_from, self.search_to)
    }

    /// The store-shaped state the selected `Sync` publishes, if there is one.
    pub fn state(&self) -> Option<PoolState> {
        self.selected
            .map(|sync| AuthoritativeMarketState { sync }.to_pool_state())
    }

    /// What the scan concluded, in the three terms §21 counts.
    ///
    /// `EmptyReserves` reuses the state layer's own predicate rather than restating
    /// it: a pool that published zero on one side has state, and that state prices
    /// nothing.
    pub fn outcome(&self) -> PoolStateAtTarget {
        match self.state() {
            None => PoolStateAtTarget::NothingPublished,
            Some(state) if !state.has_valid_reserves() => PoolStateAtTarget::EmptyReserves,
            Some(state) => match self.coverage() {
                Some(coverage)
                    if coverage.proves_valid_at(
                        UpdatePosition::new(state.block_number, state.log_index),
                        self.target,
                    ) =>
                {
                    PoolStateAtTarget::Reconstructed
                }
                _ => PoolStateAtTarget::ScanDoesNotReachTarget,
            },
        }
    }

    /// Does this row license the pool to price `target`?
    pub fn prices_target(&self) -> bool {
        self.outcome() == PoolStateAtTarget::Reconstructed
    }

    /// The block the selected `Sync` was published in — the row's own words for the
    /// evidence behind [`Self::state`], and the column §24's table names
    /// `selected_sync_block`.
    pub fn sync_block(&self) -> Option<BlockNumber> {
        self.selected.map(|sync| sync.block_number)
    }

    /// The outcome as the string the evidence tables write.
    pub fn as_str(&self) -> &'static str {
        self.outcome().as_str()
    }

    /// The parts that must agree between two strategies asking about one pool: which
    /// `Sync` was selected, and that its scan reaches the target.
    ///
    /// Deliberately narrower than the whole row — the two strategies legitimately
    /// differ in where their scans started and what they cost, and §12's correctness
    /// half is about the market state, not about the request pattern.
    pub fn selection(&self) -> (Option<SyncRecord>, bool) {
        (self.selected, self.prices_target())
    }
}

/// The four states a reconstruction can leave a pool in. Serialized as a string so a
/// table can be filtered without a decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolStateAtTarget {
    /// A `Sync` at or before the target, and a scan covering every block after it up
    /// to the target that found nothing.
    Reconstructed,
    /// No `Sync` anywhere in `search_from..=search_to`. Nothing was published, which
    /// is a finding about the chain, not a failed read.
    NothingPublished,
    /// A `Sync` was found, and it states an empty side.
    EmptyReserves,
    /// The scan did not reach the target, so the interval after the state is not
    /// covered. Not producible by [`HistoricalSyncSource`] — it exists so a
    /// hand-assembled or truncated row cannot be mistaken for proof.
    ScanDoesNotReachTarget,
}

impl PoolStateAtTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reconstructed => "target_state_valid",
            Self::NothingPublished => "state_unavailable",
            Self::EmptyReserves => "state_invalid",
            Self::ScanDoesNotReachTarget => "scan_incomplete",
        }
    }
}

/// Every pool's state at one target block, from one run of one strategy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconstruction {
    pub chain_id: ChainId,
    pub target: BlockNumber,
    /// One row per pool, in `PoolId` order.
    pub rows: Vec<PoolSyncAtTarget>,
    /// Requests this run made. For a per-pool strategy this is the sum over pools;
    /// for a census it is the pass's own count, which one run pays once however many
    /// pools read it.
    pub chunks_scanned: usize,
    /// Logs returned across those requests, including logs about pools the run was
    /// not asked about.
    pub logs_returned: usize,
}

impl Reconstruction {
    /// The coverage to attach when projecting a state snapshot onto this target.
    ///
    /// A row whose scan never reached the target contributes nothing, so such a pool
    /// falls back to the graph's original equality rule rather than being trusted by
    /// default.
    pub fn coverage(&self) -> BTreeMap<PoolId, SyncScanCoverage> {
        self.rows
            .iter()
            .filter(|row| row.search_to >= row.target)
            .filter_map(|row| Some((row.pool, row.coverage()?)))
            .collect()
    }

    pub fn row(&self, pool: PoolId) -> Option<&PoolSyncAtTarget> {
        self.rows.iter().find(|row| row.pool == pool)
    }

    /// The selection map §12's correctness half compares.
    pub fn selections(&self) -> BTreeMap<PoolId, (Option<SyncRecord>, bool)> {
        self.rows
            .iter()
            .map(|row| (row.pool, row.selection()))
            .collect()
    }

    pub fn counted(&self, outcome: PoolStateAtTarget) -> usize {
        self.rows
            .iter()
            .filter(|row| row.outcome() == outcome)
            .count()
    }

    /// Blocks scanned, summed over pools — work done, not history covered, since two
    /// pools' scans overlap.
    pub fn blocks_scanned(&self) -> u64 {
        self.rows
            .iter()
            .map(|row| row.search_to.0 - row.search_from.0 + 1)
            .sum()
    }
}

/// One chain-wide `Sync` census: what a single strategy-B pass collected.
///
/// A distinct type because its counters belong to the pass, not to a pool: one
/// request answers for every pool in the range at once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncCensus {
    pub chain_id: ChainId,
    pub from_block: BlockNumber,
    pub to_block: BlockNumber,
    pub windows: Vec<ScanWindow>,
    /// Every log the pass returned, deduped and in chain order — including the pools
    /// nobody asked about, which is the volume §12 is measuring.
    pub raw_logs: Vec<ChainLog>,
    pub duplicate_logs: usize,
    /// Logs carrying the `Sync` topic0 that did not decode as one: a shape the
    /// question cannot answer, recorded instead of quietly dropped.
    pub undecodable_logs: usize,
}

impl SyncCensus {
    pub fn rpc_calls(&self) -> usize {
        self.windows.len()
    }

    pub fn logs_returned(&self) -> usize {
        self.windows.iter().map(|window| window.logs_returned).sum()
    }

    pub fn blocks_covered(&self) -> u64 {
        self.windows
            .iter()
            .map(|window| window.to_block.0 - window.from_block.0 + 1)
            .sum()
    }

    /// How many returned logs belong to no pool in `pools` — the irrelevant volume a
    /// census pays bandwidth for.
    pub fn irrelevant_logs(&self, pools: &BTreeSet<PoolId>) -> usize {
        self.raw_logs
            .iter()
            .filter(|log| !pools.contains(&PoolId::new(self.chain_id, log.address)))
            .count()
    }

    /// Rows in the same shape strategy A produces, so the two runs can be compared.
    ///
    /// `search_from` is the pass's own start for every pool: a census cannot stop
    /// early per pool, so its coverage is uniform across the range it read.
    pub fn reconstruction(&self, lower_bounds: &BTreeMap<PoolId, BlockNumber>) -> Reconstruction {
        let mut rows = Vec::with_capacity(lower_bounds.len());
        for (pool, discovery_block) in lower_bounds {
            let mut sync_logs_seen = 0usize;
            let mut selected: Option<SyncRecord> = None;
            for log in &self.raw_logs {
                let Some(record) = sync_record_of(pool, log) else {
                    continue;
                };
                sync_logs_seen += 1;
                if selected
                    .as_ref()
                    .is_none_or(|previous| previous.identity() < record.identity())
                {
                    selected = Some(record);
                }
            }
            rows.push(PoolSyncAtTarget {
                pool: *pool,
                target: self.to_block,
                discovery_block: *discovery_block,
                search_from: self.from_block,
                search_to: self.to_block,
                sync_logs_seen,
                selected,
                chunks_scanned: self.rpc_calls(),
                logs_returned: self.logs_returned(),
            });
        }
        Reconstruction {
            chain_id: self.chain_id,
            target: self.to_block,
            rows,
            chunks_scanned: self.rpc_calls(),
            logs_returned: self.logs_returned(),
        }
    }
}

/// The historical `Sync` reader, for both query strategies.
///
/// Not a `ChainAdapter` wrapper and not a cache: it holds one number, the chunk
/// width, and issues one `eth_getLogs` per window — so an evidence table can count
/// requests and know exactly what it counted (§9: no speculative reuse of a read
/// made for another stage or another pool).
pub struct HistoricalSyncSource {
    chunk_blocks: u64,
    topics: V2Topics,
}

impl Default for HistoricalSyncSource {
    fn default() -> Self {
        Self {
            chunk_blocks: CHUNK_BLOCKS,
            topics: V2Topics::default(),
        }
    }
}

impl HistoricalSyncSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chunk_blocks(&self) -> u64 {
        self.chunk_blocks
    }

    /// For a node whose measured range limit differs from the one M9.1 recorded for
    /// `PairCreated` scans. Zero is refused for the same reason as in
    /// [`crate::scan`]: a zero-width chunk either loops forever or scans nothing.
    pub fn with_chunk_blocks(mut self, chunk_blocks: u64) -> Result<Self> {
        if chunk_blocks == 0 {
            return Err(DiscoveryError::Configuration(
                "a chunk of zero blocks is not a range".to_string(),
            ));
        }
        self.chunk_blocks = chunk_blocks;
        Ok(self)
    }

    /// Strategy A's filter: one pool, one event.
    pub fn pool_filter(&self, pool: &PoolId, from: BlockNumber, to: BlockNumber) -> LogFilter {
        LogFilter {
            from_block: from,
            to_block: to,
            addresses: vec![pool.address],
            topics: vec![Some(vec![self.topics.sync])],
        }
    }

    /// Strategy B's filter: every emitter of `Sync` in the range.
    pub fn census_filter(&self, from: BlockNumber, to: BlockNumber) -> LogFilter {
        LogFilter {
            from_block: from,
            to_block: to,
            addresses: Vec::new(),
            topics: vec![Some(vec![self.topics.sync])],
        }
    }

    /// One pool's latest `Sync` at or before `target`, with the scan that proves it is
    /// still the pool's state there.
    ///
    /// Runs backwards in chunks from the target and stops at the first chunk that
    /// answers, because that stop is the proof: every block between the `Sync` found
    /// and the target was read, so the interval holds nothing later. A pool that
    /// published nothing is scanned down to `not_before` — its discovery block, never
    /// genesis (§8) — and reports `NothingPublished`, which is the chain's answer
    /// rather than a failed read.
    pub async fn scan_pool<A: ChainAdapter + ?Sized>(
        &self,
        chain: &A,
        pool: PoolId,
        not_before: BlockNumber,
        target: BlockNumber,
    ) -> Result<PoolSyncAtTarget> {
        if not_before > target {
            return Err(DiscoveryError::Configuration(format!(
                "{}: a scan starting at {} cannot answer for the earlier target {}",
                pool.address, not_before.0, target.0
            )));
        }
        let chain_id = chain.chain_id();
        if pool.chain_id != chain_id {
            return Err(DiscoveryError::Configuration(format!(
                "the adapter serves chain {}, the scan asks about a pool on chain {}",
                chain_id.0, pool.chain_id.0
            )));
        }

        let mut cursor = target.0;
        let mut search_from = not_before;
        let mut chunks_scanned = 0usize;
        let mut logs_returned = 0usize;
        let mut sync_logs_seen = 0usize;
        let mut selected: Option<SyncRecord> = None;

        while cursor >= not_before.0 {
            let low = cursor
                .saturating_sub(self.chunk_blocks - 1)
                .max(not_before.0);
            let (from, to) = (BlockNumber(low), BlockNumber(cursor));
            let logs = chain.get_logs(self.pool_filter(&pool, from, to)).await?;
            // The ceiling check runs before anything is concluded from the answer: a
            // truncated window would read as "no Sync here", which is the one failure
            // mode that fakes a freshness proof.
            refuse_truncated_window(from, to, logs.len())?;
            chunks_scanned += 1;
            logs_returned += logs.len();
            search_from = from;

            let mut latest: Option<SyncRecord> = None;
            for log in logs {
                if log.chain_id != chain_id {
                    return Err(DiscoveryError::Configuration(format!(
                        "chain {} answered with a log from chain {} at block {}",
                        chain_id.0, log.chain_id.0, log.block_number.0
                    )));
                }
                // The request named one address. An answer from another emitter is not
                // this pool's statement, and here it cannot be a mix-up the census
                // untangles by grouping — so it stops the scan instead of being
                // decoded into a record for the wrong pool.
                if log.address != pool.address {
                    return Err(DiscoveryError::Configuration(format!(
                        "a scan of {} was answered with a log emitted by {}",
                        pool.address, log.address
                    )));
                }
                // The request named a range; an answer outside it is not history of
                // this pool's state but a record from a block this scan was never
                // allowed to read. Checked rather than filtered so a node that mixes
                // ranges fails loudly instead of silently moving the target.
                if log.block_number < from || log.block_number > to {
                    return Err(DiscoveryError::Configuration(format!(
                        "{}: a request for {}..={} was answered with a log from block {}, which \
                         is outside it",
                        pool.address, from.0, to.0, log.block_number.0
                    )));
                }
                let Some(record) = sync_record_of(&pool, &log) else {
                    continue;
                };
                sync_logs_seen += 1;
                if latest
                    .as_ref()
                    .is_none_or(|previous| previous.identity() < record.identity())
                {
                    latest = Some(record);
                }
            }
            if latest.is_some() {
                selected = latest;
                break;
            }
            match low.checked_sub(1) {
                Some(next) => cursor = next,
                None => break,
            }
        }

        Ok(PoolSyncAtTarget {
            pool,
            target,
            discovery_block: not_before,
            search_from,
            search_to: target,
            sync_logs_seen,
            selected,
            chunks_scanned,
            logs_returned,
        })
    }

    /// Strategy A over a whole pool set at one target.
    ///
    /// `lower_bounds` maps each pool to the block it was discovered at, and is walked
    /// in `PoolId` order, so the row order — and any table written from it — does not
    /// depend on how the caller assembled the map (§18).
    pub async fn reconstruct<A: ChainAdapter + ?Sized>(
        &self,
        chain: &A,
        lower_bounds: &BTreeMap<PoolId, BlockNumber>,
        target: BlockNumber,
    ) -> Result<Reconstruction> {
        let mut rows = Vec::with_capacity(lower_bounds.len());
        let mut chunks_scanned = 0usize;
        let mut logs_returned = 0usize;
        for (pool, discovery_block) in lower_bounds {
            let row = self
                .scan_pool(chain, *pool, *discovery_block, target)
                .await?;
            chunks_scanned += row.chunks_scanned;
            logs_returned += row.logs_returned;
            rows.push(row);
        }
        Ok(Reconstruction {
            chain_id: chain.chain_id(),
            target,
            rows,
            chunks_scanned,
            logs_returned,
        })
    }

    /// Strategy B: one chain-wide `Sync` pass over `from..=to`, forwards, in chunks.
    ///
    /// No early stop is possible — the pass asks about every pool at once, so it has
    /// to cover the whole range it claims to. A response that reaches the node's
    /// ceiling is an error, exactly as in a `PairCreated` census, and for a sharper
    /// reason: here a truncated answer does not shorten a list, it silently turns
    /// "a later `Sync` exists and was not read" into "none was found".
    pub async fn census<A: ChainAdapter + ?Sized>(
        &self,
        chain: &A,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<SyncCensus> {
        let chain_id = chain.chain_id();
        if from > to {
            return Err(DiscoveryError::Configuration(format!(
                "census range {}..={} is inverted",
                from.0, to.0
            )));
        }

        let mut windows: Vec<ScanWindow> = Vec::new();
        let mut logs: Vec<ChainLog> = Vec::new();
        let mut cursor = from.0;
        while cursor <= to.0 {
            let last = cursor.saturating_add(self.chunk_blocks - 1).min(to.0);
            let (window_from, window_to) = (BlockNumber(cursor), BlockNumber(last));
            let returned = chain
                .get_logs(self.census_filter(window_from, window_to))
                .await?;
            refuse_truncated_window(window_from, window_to, returned.len())?;
            windows.push(ScanWindow {
                from_block: window_from,
                to_block: window_to,
                logs_returned: returned.len(),
            });
            for log in returned {
                if log.chain_id != chain_id {
                    return Err(DiscoveryError::Configuration(format!(
                        "chain {} answered with a log from chain {} at block {}",
                        chain_id.0, log.chain_id.0, log.block_number.0
                    )));
                }
                if log.block_number < window_from || log.block_number > window_to {
                    return Err(DiscoveryError::Configuration(format!(
                        "a census of {}..={} was answered with a log from block {}",
                        window_from.0, window_to.0, log.block_number.0
                    )));
                }
                logs.push(log);
            }
            cursor = last + 1;
        }

        let (raw_logs, duplicate_logs) = sort_and_dedup_logs(logs);
        let undecodable_logs = raw_logs
            .iter()
            .filter(|log| sync_record_of(&PoolId::new(chain_id, log.address), log).is_none())
            .count();
        Ok(SyncCensus {
            chain_id,
            from_block: from,
            to_block: to,
            windows,
            raw_logs,
            duplicate_logs,
            undecodable_logs,
        })
    }
}
