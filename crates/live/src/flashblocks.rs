//! The §27–31 source: observe the `pending` block, never trust it as state.
//!
//! A candidate is a block still being built. This file exists to answer four
//! questions about it with measurements rather than with assumptions:
//!
//! 1. is the sequence moving (§ [`FlashblockStats`]'s counts),
//! 2. can its state be read (the probe in [`FlashblockSource::probe`] asks the
//!    provider and keeps the provider's own words),
//! 3. how does it map onto the canonical block it becomes
//!    ([`FlashblockSource::note_canonical`]), and
//! 4. what happens when it never does ([`FlashblockConfig`]'s expiry budget).
//!
//! What it cannot do is reach the state engine. [`SourceKind::is_canonical`] is
//! false for this source, so the pipeline it feeds has no code path that applies
//! a candidate as a reserve, and §27's "permanent overwrite of canonical state by
//! flashblock state is forbidden" is enforced by the event type rather than by a
//! promise in a comment. The canonical path — the polling or WebSocket source and
//! the replay engine — runs unchanged whether or not this source ever answers,
//! which is §30's fallback requirement in the only form that can be tested.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use evm_chain::{chain_block_from_value, HeadReader};
use evm_core::{BlockNumber, ChainId};

use crate::error::{LiveError, LiveResult};
use crate::event::{BlockAnnouncement, FlashblockCandidate, MarketEvent, SourceKind, SourceStatus};
use crate::source::{EndedBy, MarketDataSource, RunReport};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashblockConfig {
    /// Candidate reads are cheap and the endpoint refreshes sub-second, so this
    /// polls faster than the canonical source — and still changes nothing about
    /// state, because a candidate cannot advance it.
    pub poll_interval_ms: u64,
    /// How many distinct hashes one block number may accumulate before the list
    /// stops growing. A bound on memory, not on the observation: refreshes past
    /// the window are still counted.
    pub max_hashes_per_number: usize,
    /// How many polls a candidate may be held before its number is called expired
    /// — the §30 "does this candidate have an expiry" question, answered locally
    /// because no field on the payload answers it.
    pub expiry_cycles: u32,
    /// How many block numbers may be outstanding at once.
    pub max_numbers_held: usize,
}

impl Default for FlashblockConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 250,
            max_hashes_per_number: 16,
            expiry_cycles: 12,
            max_numbers_held: 8,
        }
    }
}

/// Everything the candidate stream showed, in counters the report can print.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FlashblockStats {
    /// `pending` reads attempted.
    pub reads: u64,
    pub read_failures: u64,
    /// Reads that returned a parsable header.
    pub observations: u64,
    /// Distinct (number, hash) pairs — i.e. distinct partial states.
    pub distinct_candidates: u64,
    /// Distinct block numbers that were ever pending.
    pub numbers_seen: u64,
    /// Times one number came back under a new hash: the direct evidence that a
    /// partial state was delivered rather than a head notification replayed.
    pub hash_refreshes: u64,
    /// Transactions the candidates collectively showed.
    pub transactions_observed: u64,
    pub gas_used_max: u64,
    /// A pending number moved backwards, or jumped more than one ahead.
    pub sequence_anomalies: u64,
    pub resolved: u64,
    /// Resolutions where a candidate hash *was* the sealed block's hash.
    pub matched: u64,
    pub expired: u64,
    /// Canonical blocks this source was told about.
    pub canonical_seen: u64,
    /// …of which had at least one candidate observation before they sealed.
    pub canonical_covered: u64,
    pub highest_pending: u64,
    pub highest_canonical: u64,
    /// Milliseconds between the first and last candidate read, so a refresh rate
    /// can be divided out of the counters instead of being asserted.
    pub span_ms: u64,
}

struct Record {
    hashes: Vec<B256>,
    hash_window_full: bool,
    observations: u64,
    first: FlashblockCandidate,
    last: FlashblockCandidate,
    cycles_held: u32,
}

/// The candidate source. See the module comment for what it may and may not do.
pub struct FlashblockSource<R> {
    reader: R,
    chain_id: ChainId,
    /// Where the reads go. Passed in by config/CLI (§44: no endpoint is baked in
    /// here, and nothing in this file branches on a chain id).
    endpoint: String,
    config: FlashblockConfig,
    records: BTreeMap<BlockNumber, Record>,
    seen_numbers: BTreeSet<BlockNumber>,
    last_number: Option<BlockNumber>,
    started_at_ms: Option<u64>,
    stats: FlashblockStats,
    capability: Value,
    last_report: Option<RunReport>,
}

fn hex_u64(value: Option<&Value>) -> Option<u64> {
    let text = value?.as_str()?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).ok()
}

fn transaction_count(raw: &Value) -> Option<usize> {
    raw.get("transactions")
        .and_then(Value::as_array)
        .map(Vec::len)
}

/// A pending header, normalized to the one candidate shape the pipeline carries.
///
/// `hash` and `number` come from the same parser canonical ingestion uses, so a
/// candidate and a block cannot disagree about what a field means; `gasUsed` is
/// read here because a sealed block's normalized form has no use for it and a
/// candidate's whole claim is that it is growing.
pub fn candidate_from_value(
    chain_id: ChainId,
    raw: &Value,
    observed_at_unix_ms: u64,
) -> Result<FlashblockCandidate, LiveError> {
    let block =
        chain_block_from_value(chain_id, raw).map_err(|e| LiveError::Decode(e.to_string()))?;
    let gas_used = hex_u64(raw.get("gasUsed")).ok_or_else(|| {
        LiveError::Decode("pending header carries no `gasUsed` field".to_string())
    })?;
    let transaction_count = transaction_count(raw).ok_or_else(|| {
        LiveError::Decode("pending header carries no `transactions` array".to_string())
    })?;
    Ok(FlashblockCandidate {
        chain_id,
        number: block.number,
        hash: block.hash,
        parent_hash: Some(block.parent_hash),
        chain_timestamp_secs: block.timestamp,
        transaction_count: transaction_count as u64,
        gas_used,
        observed_at_unix_ms,
    })
}

impl<R: HeadReader> FlashblockSource<R> {
    pub fn new(reader: R, chain_id: ChainId, endpoint: String, config: FlashblockConfig) -> Self {
        Self {
            reader,
            chain_id,
            endpoint,
            config,
            records: BTreeMap::new(),
            seen_numbers: BTreeSet::new(),
            last_number: None,
            started_at_ms: None,
            stats: FlashblockStats::default(),
            capability: json!({"probed": false}),
            last_report: None,
        }
    }

    pub const fn stats(&self) -> FlashblockStats {
        self.stats
    }

    pub const fn config(&self) -> FlashblockConfig {
        self.config
    }

    /// The rows §73 and the supplement demand, filled by asking the endpoint
    /// rather than by asserting them.
    ///
    /// Runs once, before the loop: it reads `pending`, then asks the provider for
    /// that exact hash back. Whatever the provider says is stored verbatim, and
    /// `state_integration` is only `PASS` if the candidate turns out to be
    /// addressable — which is the precondition for ever taking state from one.
    pub async fn probe(&mut self) -> Value {
        let mut rows = json!({
            "transport": self.reader.transport(),
            "probed": true,
            "endpoint_configured_from": "cli/env",
        });
        let raw = match self.reader.pending_raw().await {
            Ok(Some(raw)) => {
                rows["connect"] = json!({"status": "PASS", "detail": "eth_getBlockByNumber(\"pending\") answered a header"});
                raw
            }
            Ok(None) => {
                rows["connect"] = json!({"status": "FAIL", "detail": "the provider answered null for \"pending\""});
                self.capability = rows;
                return self.capability.clone();
            }
            Err(error) => {
                rows["connect"] = json!({
                    "status": "FAIL",
                    "detail": format!("pending read failed: {error}"),
                });
                self.capability = rows;
                return self.capability.clone();
            }
        };
        let candidate = match candidate_from_value(self.chain_id, &raw, crate::event::now_unix_ms())
        {
            Ok(candidate) => {
                rows["decode"] = json!({
                    "status": "PASS",
                    "fields": ["number", "hash", "parentHash", "timestamp", "transactions", "gasUsed"],
                    "number": candidate.number.0,
                    "hash": format!("{:#x}", candidate.hash),
                    "transaction_count": candidate.transaction_count,
                    "gas_used": candidate.gas_used,
                });
                candidate
            }
            Err(error) => {
                rows["decode"] = json!({"status": "FAIL", "detail": format!("{error}")});
                self.capability = rows;
                return self.capability.clone();
            }
        };
        match self.reader.candidate_read(candidate.hash).await {
            Ok(Value::Null) => {
                rows["state_integration"] = json!({
                    "status": "BLOCKED",
                    "probe_method": "eth_getBlockByHash",
                    "provider_answer": "null",
                    "detail": format!(
                        "block {} is pending and its own hash {:#x} resolves to nothing, so no state can be read at a candidate",
                        candidate.number.0, candidate.hash
                    ),
                });
            }
            Ok(answer) => {
                rows["state_integration"] = json!({
                    "status": "CANDIDATE_ADDRESSABLE",
                    "probe_method": "eth_getBlockByHash",
                    "provider_answer": answer,
                    "detail": "the provider resolves a pending hash; reading *state* at it is still unproven, so no candidate is applied to the store",
                });
            }
            Err(error) => {
                rows["state_integration"] = json!({
                    "status": "BLOCKED",
                    "probe_method": "eth_getBlockByHash",
                    "provider_answer": format!("{error}"),
                    "detail": "the provider refused to address a candidate hash",
                });
            }
        }
        rows["fallback"] = json!({
            "status": "PASS",
            "detail": "canonical ingestion runs on the polling/WebSocket source regardless of this source; a candidate cannot advance state, so there is nothing for it to overwrite",
        });
        rows["subscription"] = json!({
            "status": "NOT_PROBED_HERE",
            "detail": "subscription is probed by the WebSocket source, whose capability record carries the provider's own error string",
        });
        self.capability = rows;
        self.capability.clone()
    }

    /// One `pending` read, as a candidate observation.
    ///
    /// Same number and same hash is not a new candidate — it is the same partial
    /// state seen again, so it is counted, not re-emitted. Same number under a new
    /// hash is the interesting case, and it is what a real partial-state delivery
    /// looks like on this endpoint.
    pub fn observe(&mut self, candidate: FlashblockCandidate) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        self.stats.observations += 1;
        self.stats.transactions_observed += candidate.transaction_count;
        self.stats.gas_used_max = self.stats.gas_used_max.max(candidate.gas_used);
        self.stats.highest_pending = self.stats.highest_pending.max(candidate.number.0);
        if let Some(previous) = self.last_number {
            if candidate.number < previous {
                self.stats.sequence_anomalies += 1;
                events.push(MarketEvent::Unknown {
                    detail: format!(
                        "`pending` moved backwards: {} after {} — a candidate source that rewinds cannot be reconciled, so the number is recorded and ignored",
                        candidate.number.0, previous.0
                    ),
                });
                return events;
            }
            if candidate.number.0 > previous.0 + 1 {
                let skipped = candidate.number.0 - previous.0 - 1;
                self.stats.sequence_anomalies += 1;
                events.push(MarketEvent::Unknown {
                    detail: format!(
                        "`pending` jumped {} -> {}: {skipped} number(s) were never observed as pending",
                        previous.0, candidate.number.0
                    ),
                });
            }
        }
        self.last_number = Some(candidate.number);
        match self.records.get_mut(&candidate.number) {
            Some(record) => {
                record.observations += 1;
                record.last = candidate;
                if record.hashes.contains(&candidate.hash) {
                    return events;
                }
                self.stats.hash_refreshes += 1;
                self.stats.distinct_candidates += 1;
                if record.hashes.len() < self.config.max_hashes_per_number {
                    record.hashes.push(candidate.hash);
                } else {
                    record.hash_window_full = true;
                }
                events.push(MarketEvent::Candidate(candidate));
            }
            None => {
                self.stats.distinct_candidates += 1;
                if self.seen_numbers.insert(candidate.number) {
                    self.stats.numbers_seen += 1;
                }
                self.records.insert(
                    candidate.number,
                    Record {
                        hashes: vec![candidate.hash],
                        hash_window_full: false,
                        observations: 1,
                        first: candidate,
                        last: candidate,
                        cycles_held: 0,
                    },
                );
                events.push(MarketEvent::Candidate(candidate));
            }
        }
        events
    }

    /// Age every outstanding candidate by one poll and expire what aged out.
    ///
    /// Expiry is a report, never a silent forget (§51): a candidate that will not
    /// seal is a fact about the endpoint, and it is stated with its number and
    /// hash.
    pub fn tick(&mut self) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        for record in self.records.values_mut() {
            record.cycles_held += 1;
        }
        loop {
            let stale = self
                .records
                .iter()
                .find(|(number, record)| {
                    record.cycles_held > self.config.expiry_cycles
                        || number.0 < self.stats.highest_canonical
                        || self.records.len() > self.config.max_numbers_held
                })
                .map(|(number, _)| *number);
            let Some(number) = stale else { break };
            let record = self.records.remove(&number).expect("just found");
            self.stats.expired += 1;
            events.push(MarketEvent::CandidateExpired {
                number: number.0,
                hash: format!("{:#x}", record.last.hash),
                observations: record.observations,
            });
        }
        events
    }

    /// Feed the canonical block that just sealed, and close the candidate window
    /// for it (§28's mapping question).
    ///
    /// This is the only way the candidate source learns what the chain actually
    /// became: the canonical path tells it, after state has already been applied.
    /// The direction matters — a candidate is reconciled *to* canonical, never the
    /// other way round.
    pub fn note_canonical(&mut self, block: BlockAnnouncement) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        self.stats.canonical_seen += 1;
        self.stats.highest_canonical = self.stats.highest_canonical.max(block.number.0);
        let resolved: Vec<BlockNumber> = self
            .records
            .range(..=block.number)
            .map(|(number, _)| *number)
            .collect();
        for number in resolved {
            let Some(record) = self.records.remove(&number) else {
                continue;
            };
            self.stats.resolved += 1;
            let matched = record.hashes.contains(&block.hash);
            if matched {
                self.stats.matched += 1;
            }
            self.stats.canonical_covered += 1;
            events.push(MarketEvent::CandidateResolved {
                number: number.0,
                observations: record.observations,
                candidate_hashes: record.hashes.len() as u64 + u64::from(record.hash_window_full),
                matched_hash: matched,
                canonical_hash: format!("{:#x}", block.hash),
                transaction_count_delta: record.last.transaction_count as i64
                    - block.transaction_count as i64,
                gas_used_growth: record.last.gas_used as i64 - record.first.gas_used as i64,
            });
        }
        events
    }

    async fn cycle_async(&mut self) -> LiveResult<Vec<MarketEvent>> {
        self.stats.reads += 1;
        let raw = match self.reader.pending_raw().await {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                self.stats.read_failures += 1;
                return Ok(vec![MarketEvent::Status(SourceStatus::Disconnected {
                    source: SourceKind::Flashblock,
                    reason: "the provider answered null for `pending`".to_string(),
                })]);
            }
            Err(error) => {
                self.stats.read_failures += 1;
                return Ok(vec![MarketEvent::Status(SourceStatus::Disconnected {
                    source: SourceKind::Flashblock,
                    reason: format!("pending read failed: {error}"),
                })]);
            }
        };
        let candidate = candidate_from_value(self.chain_id, &raw, crate::event::now_unix_ms())?;
        let mut events = self.observe(candidate);
        events.extend(self.tick());
        Ok(events)
    }
}

impl<R: HeadReader + Send> FlashblockSource<R> {
    /// The candidate loop, with the canonical path's announcements handed to it so
    /// §28's mapping question is answered by production code and not only by a test:
    /// every block that seals either resolves a candidate window in this session or
    /// it does not, and [`FlashblockSource::capability`] says which.
    ///
    /// The drain comes before the stop check so a shutdown never loses the
    /// reconciliation of a block that sealed while the loop was finishing (§49).
    ///
    /// [`MarketDataSource::run`] calls this with a closed channel: a candidate source
    /// wired without a canonical path must report NOT_OBSERVED rather than assume a
    /// mapping it was never shown.
    pub async fn run_reconciling(
        &mut self,
        sink: mpsc::Sender<MarketEvent>,
        stop: Arc<AtomicBool>,
        canonical: &mut mpsc::Receiver<BlockAnnouncement>,
    ) -> LiveResult<RunReport> {
        self.probe().await;
        let mut report = RunReport {
            source: SourceKind::Flashblock,
            cycles: 0,
            read_failures: 0,
            events_sent: 0,
            ended_by: None,
            tracker: None,
            detail: json!({}),
        };
        self.started_at_ms = Some(crate::event::now_unix_ms());
        loop {
            while let Ok(announcement) = canonical.try_recv() {
                for event in self.note_canonical(announcement) {
                    sink.send(event).await.map_err(|_| LiveError::QueueClosed)?;
                    report.events_sent += 1;
                }
            }
            if stop.load(Ordering::Relaxed) {
                report.ended_by = Some(EndedBy::StopRequested);
                break;
            }
            report.cycles += 1;
            let events = match self.cycle_async().await {
                Ok(events) => events,
                Err(error) => {
                    report.read_failures = self.stats.read_failures;
                    report.detail = json!(self.stats);
                    self.last_report = Some(report.clone());
                    return Err(error);
                }
            };
            for event in events {
                sink.send(event).await.map_err(|_| LiveError::QueueClosed)?;
                report.events_sent += 1;
            }
            tokio::time::sleep(Duration::from_millis(self.config.poll_interval_ms)).await;
        }
        if let Some(started) = self.started_at_ms {
            self.stats.span_ms = crate::event::now_unix_ms().saturating_sub(started);
        }
        report.read_failures = self.stats.read_failures;
        report.detail = json!(self.stats);
        self.last_report = Some(report.clone());
        Ok(report)
    }
}

#[async_trait]
impl<R: HeadReader + Send> MarketDataSource for FlashblockSource<R> {
    fn kind(&self) -> SourceKind {
        SourceKind::Flashblock
    }

    async fn head(&mut self) -> LiveResult<BlockNumber> {
        self.reader
            .head()
            .await
            .map_err(|e| LiveError::Head(e.to_string()))
    }

    async fn run(
        &mut self,
        _start_after: BlockNumber,
        sink: mpsc::Sender<MarketEvent>,
        stop: Arc<AtomicBool>,
    ) -> LiveResult<RunReport> {
        // No canonical path is wired: the candidate window is never told what
        // sealed, so §28's mapping question answers NOT_OBSERVED rather than
        // being silently assumed.
        let (closed, mut receiver) = mpsc::channel(1);
        drop(closed);
        self.run_reconciling(sink, stop, &mut receiver).await
    }

    fn capability(&self) -> Value {
        let mut rows = self.capability.clone();
        rows["endpoint"] = Value::String(self.endpoint.clone());
        let reconciliation = if self.stats.canonical_seen == 0 {
            "NOT_OBSERVED"
        } else if self.stats.resolved > 0 {
            "PASS"
        } else {
            "FAIL"
        };
        rows["sequence"] = json!({
            "status": if self.stats.distinct_candidates > 0 { "PASS" } else { "FAIL" },
            "numbers_seen": self.stats.numbers_seen,
            "distinct_candidates": self.stats.distinct_candidates,
            "hash_refreshes": self.stats.hash_refreshes,
            "sequence_anomalies": self.stats.sequence_anomalies,
            "resolved": self.stats.resolved,
            "expired": self.stats.expired,
            "span_ms": self.stats.span_ms,
        });
        rows["newheads_reconciliation"] = json!({
            "status": reconciliation,
            "canonical_seen": self.stats.canonical_seen,
            "canonical_covered": self.stats.canonical_covered,
            "matched_candidate_hash": self.stats.matched,
            "detail": "candidates reconciled against canonical announcements from the same pipeline",
        });
        rows["end_to_end"] = json!({
            "status": if self.stats.resolved > 0 { "PARTIAL" } else { "BLOCKED" },
            "detail": if self.stats.resolved > 0 {
                "a candidate stream was carried through the unified pipeline and reconciled against canonical blocks in the same session; no state was ever taken from a candidate"
            } else {
                "no candidate sealed into a canonical block during this session, so the mapping is unproven here"
            },
            "note": "M5 does not broadcast, does not sign and does not execute (§26)",
        });
        rows
    }

    fn report(&self) -> Option<RunReport> {
        self.last_report.clone()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use async_trait::async_trait;
    use evm_chain::{ChainBlock, ChainError, HeadReader, Result as ChainResult};
    use evm_core::BlockNumber;

    use super::*;
    use crate::event::SourceKind;

    const CHAIN: ChainId = ChainId(1);

    /// A `pending` header in the shape the provider gives it, so the parse path is
    /// exercised too and not only the accounting.
    fn pending_raw(number: u64, txs: usize, gas: u64) -> Value {
        let parent = number - 1;
        json!({
            "number": format!("0x{number:x}"),
            "hash": format!("0x{number:0>64x}"),
            "parentHash": format!("0x{parent:0>64x}"),
            "timestamp": "0x6553f000",
            "gasUsed": format!("0x{gas:x}"),
            "transactions": (0..txs).map(|i| format!("0x{i:0>64x}")).collect::<Vec<_>>(),
            "baseFeePerGas": "0x7",
        })
    }

    fn candidate(number: u64, seed: u8, txs: u64, gas: u64) -> FlashblockCandidate {
        FlashblockCandidate {
            chain_id: CHAIN,
            number: BlockNumber(number),
            hash: B256::repeat_byte(seed),
            parent_hash: Some(B256::repeat_byte(seed.wrapping_sub(1))),
            chain_timestamp_secs: 1_700_000_000 + number,
            transaction_count: txs,
            gas_used: gas,
            observed_at_unix_ms: 0,
        }
    }

    fn canonical(number: u64, seed: u8, txs: u64) -> BlockAnnouncement {
        BlockAnnouncement {
            chain_id: CHAIN,
            number: BlockNumber(number),
            hash: B256::repeat_byte(seed),
            parent_hash: B256::repeat_byte(seed.wrapping_sub(1)),
            chain_timestamp_secs: 1_700_000_000 + number,
            transaction_count: txs,
            observed_at_unix_ms: 0,
            source: SourceKind::HttpPoll,
        }
    }

    fn source(config: FlashblockConfig) -> FlashblockSource<UnusedReader> {
        FlashblockSource::new(
            UnusedReader,
            CHAIN,
            "http://candidate.test".to_string(),
            config,
        )
    }

    /// The accounting under test is pure, so the transport is only present to
    /// satisfy the type. Its answers are asserted where they matter, in the
    /// provider-facing tests, not here.
    struct UnusedReader;

    #[async_trait]
    impl HeadReader for UnusedReader {
        fn transport(&self) -> &'static str {
            "test"
        }
        async fn head(&mut self) -> ChainResult<BlockNumber> {
            Err(ChainError::MissingData("unused".to_string()))
        }
        async fn block_at(&mut self, _number: BlockNumber) -> ChainResult<Option<ChainBlock>> {
            Ok(None)
        }
    }

    #[test]
    fn a_pending_header_parses_into_a_candidate_with_its_growth_fields() {
        let raw = pending_raw(100, 4, 200_000);
        let got = candidate_from_value(CHAIN, &raw, 7).expect("parses");
        assert_eq!(got.number, BlockNumber(100));
        assert_eq!(got.transaction_count, 4);
        assert_eq!(got.gas_used, 200_000);
        assert_eq!(got.observed_at_unix_ms, 7);
        assert_eq!(got.chain_timestamp_secs, 0x6553_f000);
    }

    #[test]
    fn a_header_without_gas_used_is_a_decode_failure_not_a_zero() {
        let mut raw = pending_raw(100, 4, 200_000);
        raw.as_object_mut().expect("object").remove("gasUsed");
        let err = candidate_from_value(CHAIN, &raw, 0).unwrap_err();
        assert!(
            matches!(err, LiveError::Decode(_)),
            "a missing field must not become a candidate claiming zero gas: {err}"
        );
    }

    #[test]
    fn the_same_candidate_twice_is_counted_but_not_re_emitted() {
        let mut src = source(FlashblockConfig::default());
        let first = src.observe(candidate(10, 10, 1, 1_000));
        let again = src.observe(candidate(10, 10, 1, 1_000));
        assert!(matches!(first.as_slice(), [MarketEvent::Candidate(_)]));
        assert!(again.is_empty());
        assert_eq!(src.stats().observations, 2);
        assert_eq!(src.stats().distinct_candidates, 1);
        assert_eq!(src.stats().hash_refreshes, 0);
    }

    #[test]
    fn a_new_hash_for_one_number_is_a_refresh_and_is_emitted() {
        // What was measured on the flashblocks endpoint: the same number coming
        // back with a different hash and more transactions. One partial state per
        // hash, so one event per hash.
        let mut src = source(FlashblockConfig::default());
        src.observe(candidate(10, 10, 1, 1_000));
        let events = src.observe(candidate(10, 0xAA, 5, 90_000));
        assert!(
            matches!(events.as_slice(), [MarketEvent::Candidate(c)] if c.transaction_count == 5)
        );
        assert_eq!(src.stats().hash_refreshes, 1);
        assert_eq!(src.stats().numbers_seen, 1);
        assert_eq!(src.stats().distinct_candidates, 2);
    }

    #[test]
    fn resolution_reports_whether_the_candidate_ever_became_the_block() {
        let mut src = source(FlashblockConfig::default());
        src.observe(candidate(10, 10, 2, 50_000));
        src.observe(candidate(10, 0xAA, 5, 210_000));
        // The sealed block is neither of the candidate hashes, and holds more
        // transactions than the last candidate did.
        let events = src.note_canonical(canonical(10, 0xBB, 9));
        let [MarketEvent::CandidateResolved {
            observations,
            candidate_hashes,
            matched_hash,
            transaction_count_delta,
            gas_used_growth,
            ..
        }] = events.as_slice()
        else {
            panic!("expected one resolution, got {events:?}");
        };
        assert_eq!(*observations, 2);
        assert_eq!(*candidate_hashes, 2);
        assert!(
            !*matched_hash,
            "a partial state is not the sealed block, and the record says so"
        );
        assert_eq!(*transaction_count_delta, 5 - 9);
        assert_eq!(*gas_used_growth, 210_000 - 50_000);
        assert_eq!(src.stats().resolved, 1);
        assert_eq!(src.stats().matched, 0);
        assert_eq!(src.stats().canonical_covered, 1);
    }

    #[test]
    fn a_matching_candidate_hash_is_counted_as_a_match() {
        let mut src = source(FlashblockConfig::default());
        src.observe(candidate(10, 0xAA, 5, 210_000));
        let events = src.note_canonical(canonical(10, 0xAA, 5));
        assert!(matches!(
            events.as_slice(),
            [MarketEvent::CandidateResolved {
                matched_hash: true,
                transaction_count_delta: 0,
                ..
            }]
        ));
        assert_eq!(src.stats().matched, 1);
    }

    #[test]
    fn a_candidate_that_never_seals_expires_and_says_so() {
        let config = FlashblockConfig {
            expiry_cycles: 2,
            ..FlashblockConfig::default()
        };
        let mut src = source(config);
        src.observe(candidate(11, 11, 1, 1_000));
        assert!(src.tick().is_empty());
        assert!(src.tick().is_empty());
        let events = src.tick();
        assert!(
            matches!(
                events.as_slice(),
                [MarketEvent::CandidateExpired {
                    number: 11,
                    observations: 1,
                    ..
                }]
            ),
            "{events:?}"
        );
        assert_eq!(src.stats().expired, 1);
        // Nothing is left outstanding, and the count survives the forget.
        assert!(src.records.is_empty());
        assert_eq!(src.stats().numbers_seen, 1);
    }

    #[test]
    fn a_pending_number_that_rewinds_or_jumps_is_an_anomaly_not_a_silence() {
        let mut src = source(FlashblockConfig::default());
        src.observe(candidate(10, 10, 1, 1_000));
        let jumped = src.observe(candidate(13, 13, 1, 1_000));
        assert!(matches!(
            jumped.as_slice(),
            [MarketEvent::Unknown { .. }, MarketEvent::Candidate(_)]
        ));
        let back = src.observe(candidate(11, 11, 1, 1_000));
        assert!(matches!(back.as_slice(), [MarketEvent::Unknown { .. }]));
        assert_eq!(src.stats().sequence_anomalies, 2);
        assert_eq!(
            src.stats().highest_pending,
            13,
            "a rewind does not move the high-water mark backwards"
        );
    }

    #[test]
    fn no_path_in_this_source_emits_a_canonical_event() {
        // §27 by construction: whatever the candidate stream does, this source
        // never produces the one event type that advances state.
        let mut src = source(FlashblockConfig {
            expiry_cycles: 1,
            max_numbers_held: 1,
            ..FlashblockConfig::default()
        });
        let mut events = Vec::new();
        for number in 10..16u64 {
            events.extend(src.observe(candidate(number, number as u8, number, number * 1_000)));
            events.extend(src.tick());
            events.extend(src.note_canonical(canonical(number, number as u8, number)));
        }
        events.extend(src.observe(candidate(99, 0x99, 1, 1)));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, MarketEvent::Canonical(_))),
            "a candidate source emitted a canonical block: {events:?}"
        );
        assert!(!SourceKind::Flashblock.is_canonical());
    }

    #[test]
    fn an_unprobed_endpoint_cannot_be_reported_as_working() {
        // §31: a run that never asked the provider gets rows that say so. The
        // derived rows may be present, but none of them may read like a pass.
        let src = source(FlashblockConfig::default());
        let rows = src.capability();
        assert_eq!(rows["probed"], json!(false));
        assert_eq!(rows["endpoint"], json!("http://candidate.test"));
        assert!(
            rows.get("connect").is_none() && rows.get("state_integration").is_none(),
            "the rows that quote a provider answer may only exist after a probe: {rows}"
        );
        assert_eq!(rows["sequence"]["status"], json!("FAIL"));
        assert_eq!(
            rows["newheads_reconciliation"]["status"],
            json!("NOT_OBSERVED")
        );
        assert_eq!(rows["end_to_end"]["status"], json!("BLOCKED"));
    }
}
