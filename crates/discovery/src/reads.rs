//! The reads verification is built from, kept as records.
//!
//! Two jobs, deliberately done in two types. This module collects — it is the
//! only place in the crate that asks a node anything about a single candidate,
//! and it writes down what came back in a form that can be committed. [`crate::verify`]
//! then decides, from those records alone, with no chain and no clock.
//!
//! The split is what makes M9.1's evidence more than a printout of a run: the
//! committed records can be fed back through the same decision function in a test
//! with no network, so a reviewer recomputes "verified" and "rejected" from the
//! raw material rather than trusting that the original run said so.
//!
//! Every read is pinned to the candidate's own discovery block (M9.1 §22). Not
//! `latest`, and not the head of the run: a candidate born at block N has to be
//! verified against the chain as of N, or the answer describes a contract that
//! did not exist when the claim was made. The block is recorded per read, in the
//! read, so a table that mixed pinned and unpinned rows would be visibly wrong.

use alloy_primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};

use evm_chain::{CallRequest, ChainAdapter, ChainLog};
use evm_core::{BlockNumber, LogIndex, PoolId, TxHash, TxIndex};
use evm_protocol::{V2Call, V2Topics};

use crate::candidate::CandidatePool;
use crate::error::{DiscoveryError, Result};

/// Which question a [`CallRecord`] answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallKind {
    /// `eth_getCode` — does an implemented contract live at this address.
    Bytecode,
    /// `token0()`.
    Token0,
    /// `token1()`.
    Token1,
    /// `getReserves()`.
    GetReserves,
}

impl CallKind {
    /// The canonical signature the selector was derived from, or the RPC method
    /// for the one read that is not a contract call. This is the string that ends
    /// up in `EvidenceRef.signature`, so it names the question, not the answer.
    pub fn signature(&self) -> &'static str {
        match self {
            Self::Bytecode => "eth_getCode(address)",
            Self::Token0 => "token0()",
            Self::Token1 => "token1()",
            Self::GetReserves => "getReserves()",
        }
    }

    pub fn call(&self) -> Option<V2Call> {
        match self {
            Self::Bytecode => None,
            Self::Token0 => Some(V2Call::Token0),
            Self::Token1 => Some(V2Call::Token1),
            Self::GetReserves => Some(V2Call::GetReserves),
        }
    }
}

/// One read: what was asked, of whom, at which block, and what came back.
///
/// `return_data` and `error` are mutually exclusive by construction of the
/// collector, and both stay `Option` so a failed read is still a record of the
/// attempt. A rejection needs the node's own words behind it (M9.1 §19: no number
/// without a reproducible source), and "the call failed" is only auditable if the
/// failure text was kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    pub kind: CallKind,
    pub target: Address,
    /// The block this read was pinned at. Equal to the candidate's discovery
    /// block for every read a historical run makes (§22), and checked there.
    pub pinned_at: BlockNumber,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_data: Option<Bytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Bytecode is the one read too large to commit whole: a V2 pair is tens of
    /// kilobytes, and 44 of them would make the evidence file mostly hex. Its
    /// length and digest are kept instead — enough to prove the contract had code
    /// and enough for a re-run to compare against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_len: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_digest: Option<String>,
}

impl CallRecord {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    pub fn return_data(&self) -> Option<&Bytes> {
        self.return_data.as_ref()
    }

    /// Row identity for evidence: which candidate, which question, which block.
    /// Never a sort position, so two runs agree on which rows are the same row.
    pub fn identity(&self) -> (Address, u8, u64) {
        (self.target, self.kind as u8, self.pinned_at.0)
    }
}

/// The pool's own `Sync(uint112,uint112)` — authoritative market state, and the
/// only thing in this crate that may become a `PoolState` (M9.1 §12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRecord {
    pub pool: PoolId,
    pub block_number: BlockNumber,
    pub tx_hash: TxHash,
    pub tx_index: TxIndex,
    pub log_index: LogIndex,
    pub reserve0: U256,
    pub reserve1: U256,
}

impl SyncRecord {
    /// Row identity: the log's chain position, which is unique.
    pub fn identity(&self) -> (u64, u64, u64) {
        (self.block_number.0, self.tx_index.0, self.log_index.0)
    }

    pub fn position(&self) -> evm_protocol::LogPosition {
        evm_protocol::LogPosition {
            chain_id: self.pool.chain_id,
            block_number: self.block_number,
            tx_hash: self.tx_hash,
            tx_index: self.tx_index,
            log_index: self.log_index,
        }
    }
}

/// Everything one candidate was measured against.
///
/// A plain struct of records, and the whole input to
/// [`crate::verify::verify`]. If a field is missing here, verification cannot
/// have used it — which is what makes "no hidden inputs" checkable by reading the
/// type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReads {
    pub candidate: CandidatePool,
    /// The block every read below was pinned at. Verification requires it equals
    /// the candidate's discovery block; a run that quietly used `latest` would
    /// otherwise look identical in its output.
    pub pinned_at: BlockNumber,
    /// The four contract reads, in the order [`CallKind`] declares them, so the
    /// serialized form never depends on completion order.
    pub calls: Vec<CallRecord>,
    /// The pool's latest `Sync` inside the scanned range, if it emitted one.
    /// `None` is a finding, not a gap: it means nobody has published this pool's
    /// reserves, so the candidate has no state evidence.
    pub sync: Option<SyncRecord>,
    /// How many `Sync` logs the search saw for this pool. Kept because
    /// `sync: None` with `sync_logs_seen: 7` and `sync: None` with `0` are
    /// different facts, and only one of them is "this pool never traded".
    pub sync_logs_seen: usize,
    /// Last block of the scanned range, i.e. how far the `Sync` search looked.
    pub state_search_to_block: BlockNumber,
}

impl CandidateReads {
    pub fn call(&self, kind: CallKind) -> Option<&CallRecord> {
        self.calls.iter().find(|record| record.kind == kind)
    }
}

/// The hex of a record's return data, for evidence rows that show the raw answer.
/// An errored read has no answer, and is written as `0x` rather than as nothing:
/// the row still has to exist in the table, which is the point of keeping failed
/// reads as records at all.
pub fn return_hex(record: &CallRecord) -> String {
    match &record.return_data {
        Some(data) => crate::scan::hex_of(data),
        None => "0x".to_string(),
    }
}

/// Ask the four identity questions of one candidate, pinned at its own discovery
/// block, and keep the answers.
///
/// One explicit request per question, per candidate — no batch, no cache, no
/// reuse of a read made for another stage (M9.1 §23). If this looks wasteful
/// next milestone, that is the point of writing the requests down: an RPC
/// reduction has to start from a measured count, and
/// `data/evidence/m8/m8.6` already concluded there was no safe one to take.
pub async fn collect_contract_reads<A: ChainAdapter + ?Sized>(
    chain: &A,
    candidate: &CandidatePool,
) -> Result<Vec<CallRecord>> {
    let at = candidate.discovery_block();
    let pool = candidate.pool.address;
    let mut records = Vec::with_capacity(4);
    for kind in [
        CallKind::Bytecode,
        CallKind::Token0,
        CallKind::Token1,
        CallKind::GetReserves,
    ] {
        records.push(read_one(chain, kind, pool, at).await?);
    }
    records.sort_by_key(|record| record.identity());
    Ok(records)
}

async fn read_one<A: ChainAdapter + ?Sized>(
    chain: &A,
    kind: CallKind,
    pool: Address,
    at: BlockNumber,
) -> Result<CallRecord> {
    let mut record = CallRecord {
        kind,
        target: pool,
        pinned_at: at,
        return_data: None,
        error: None,
        code_len: None,
        code_digest: None,
    };
    match kind {
        CallKind::Bytecode => match chain.get_code(at, pool).await {
            Ok(code) => {
                record.code_len = Some(code.len());
                record.code_digest = Some(digest(&code));
            }
            Err(err) => record.error = Some(err.to_string()),
        },
        CallKind::Token0 | CallKind::Token1 | CallKind::GetReserves => {
            // `call` is `Some` for exactly these three variants.
            let call = kind.call().ok_or_else(|| {
                crate::error::DiscoveryError::Configuration(format!(
                    "{:?} carries no contract call",
                    kind
                ))
            })?;
            let request = CallRequest {
                to: pool,
                data: call.encode(),
            };
            match chain.call(at, &request).await {
                Ok(return_data) => record.return_data = Some(return_data),
                Err(err) => record.error = Some(err.to_string()),
            }
        }
    }
    Ok(record)
}

/// Find the pool's latest `Sync` in `from..=to`, as a [`SyncRecord`].
///
/// This is a second `eth_getLogs` per candidate, filtered to that one pool. It
/// looks like it could have come free out of the discovery scan — the scan already
/// asked the node for logs — and that is exactly why it does not happen:
/// discovery filtered on `PairCreated` topic0 across the whole chain, and
/// reinterpreting that response as "also the Sync history of every pool in it"
/// would be cross-stage state reuse (§23). The questions are different, so the
/// requests are different.
///
/// Returns the last one by chain position, plus how many were seen. A pool that
/// emitted only zero-reserve `Sync` logs still has state evidence here: the
/// reserves are the pool's own statement, and it is the graph's job to refuse to
/// price an empty side, not this function's job to hide that it made one.
pub async fn collect_sync_record<A: ChainAdapter + ?Sized>(
    chain: &A,
    candidate: &CandidatePool,
    from: BlockNumber,
    to: BlockNumber,
) -> Result<(Option<SyncRecord>, usize)> {
    let topics = V2Topics::default();
    let filter = evm_chain::LogFilter {
        from_block: from,
        to_block: to,
        addresses: vec![candidate.pool.address],
        topics: vec![Some(vec![topics.sync])],
    };
    let logs = chain.get_logs(filter).await?;
    let seen = logs.len();
    let mut best: Option<SyncRecord> = None;
    for log in logs {
        let Some(record) = sync_record_of(&candidate.pool, &log) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|previous| previous.identity() < record.identity())
        {
            best = Some(record);
        }
    }
    Ok((best, seen))
}

/// Everything [`crate::verify`] needs about one candidate, collected in one pass.
///
/// The two collectors stay separate because they answer separate questions, but a
/// run has to call them in this order and with these blocks, and that pairing is
/// exactly the logic a historical replay and a live run must share (§18: one
/// implementation, not two).
///
/// `state_search_to_block` is the only degree of freedom — how far past the
/// candidate's own creation block to look for a `Sync`. It cannot re-price the
/// verification: the contract reads stay pinned at the discovery block whatever this
/// says (§22), so widening the search can only ever add state evidence. A search
/// window that ends *before* the claim is refused rather than silently emptied.
pub async fn collect_candidate_reads<A: ChainAdapter + ?Sized>(
    chain: &A,
    candidate: &CandidatePool,
    state_search_to_block: BlockNumber,
) -> Result<CandidateReads> {
    let from = candidate.discovery_block();
    if state_search_to_block < from {
        return Err(DiscoveryError::Configuration(format!(
            "state search ends at {}, before this candidate's discovery block {}",
            state_search_to_block.0, from.0
        )));
    }
    let calls = collect_contract_reads(chain, candidate).await?;
    let (sync, sync_logs_seen) =
        collect_sync_record(chain, candidate, from, state_search_to_block).await?;
    Ok(CandidateReads {
        candidate: candidate.clone(),
        pinned_at: from,
        calls,
        sync,
        sync_logs_seen,
        state_search_to_block,
    })
}

/// Read a `Sync` log's two `uint112` reserves.
///
/// Deliberately not `V2Adapter::decode_log`: the adapter gates `Sync` on the
/// registry, and a candidate being verified here is by definition not in the
/// registry yet — that gate is what discovery must not bypass, so discovery decodes
/// through the same word-level helpers the adapter uses and says so. A log whose
/// shape is wrong yields `None`, and its candidate ends up with `sync: None`: no
/// state evidence, therefore no graph.
fn sync_record_of(pool: &PoolId, log: &ChainLog) -> Option<SyncRecord> {
    let topics = V2Topics::default();
    if log.topics.first() != Some(&topics.sync) {
        return None;
    }
    if log.topics.len() != 1 || log.data.len() != 64 {
        return None;
    }
    // A uint112 lives in the low 14 bytes of its word; the 18 above it must be
    // zero or this is not the event it claims to be. Same rule as `Sync` decoding
    // in `evm-protocol`.
    let reserve0 = evm_protocol::signatures::word(&log.data, 0).ok()?;
    let reserve1 = evm_protocol::signatures::word(&log.data, 1).ok()?;
    if log.data[..18] != [0u8; 18] || log.data[32..50] != [0u8; 18] {
        return None;
    }
    Some(SyncRecord {
        pool: *pool,
        block_number: log.block_number,
        tx_hash: log.tx_hash,
        tx_index: log.tx_index,
        log_index: log.log_index,
        reserve0,
        reserve1,
    })
}

/// keccak256 of a byte string, as a lowercase `0x` hex digest — the shape
/// `crates/chain/src/rpc_trace.rs` uses for endpoint identities, so evidence
/// across milestones compares the same way.
fn digest(bytes: &[u8]) -> String {
    alloy_primitives::keccak256(bytes).to_string()
}
