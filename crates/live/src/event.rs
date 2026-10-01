//! What a market data source produces: one event type for live, flashblock and
//! replay alike.
//!
//! The rule this file exists to enforce (§9, and the supplement's "Flashblocks
//! must not become a second pipeline"): a source's only job is to say *which
//! canonical block numbers exist, in chain order*. It does not decode, it does
//! not touch state, and it does not carry block bodies. The pipeline reads a
//! block through [`evm_chain::ChainAdapter::get_block_data`] no matter which
//! source announced it, so the decode → StateUpdate → StateStore path below is
//! literally the same code for live and replay rather than a re-implementation
//! that happens to agree.

use serde::Serialize;

use evm_core::{BlockNumber, ChainId};

/// Which producer an announcement came from. Part of the evidence, not part of
/// the semantics: `Canonical` blocks from any of the three mean the same thing
/// to the state engine, and only `Candidate` means something different (§27).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum SourceKind {
    /// Polling canonical blocks over HTTP.
    HttpPoll,
    /// One WebSocket connection: subscription if the provider can, polling over
    /// the same socket if it cannot (§3.1A).
    WebSocket,
    /// The `pending` tag, re-read as it grows. Candidate state only.
    Flashblock,
    /// Recorded blocks, through the same event type (§32, §67).
    Replay,
}

impl SourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpPoll => "http-poll",
            Self::WebSocket => "websocket",
            Self::Flashblock => "flashblock",
            Self::Replay => "replay",
        }
    }

    /// Whether an announcement of this kind may reach the state engine. Only a
    /// sealed block may: a flashblock candidate is never a reserve authority
    /// (§27, and §12's rule that only a canonical Sync sets a reserve).
    pub const fn is_canonical(&self) -> bool {
        !matches!(self, Self::Flashblock)
    }
}

/// A sealed block, as far as ordering is concerned.
///
/// `observed_at_unix_ms` is when *this process* first heard of the block;
/// `chain_timestamp_secs` is the chain's own clock. Both are kept because §70's
/// `block_received_latency` is only meaningful when it says which clock it was
/// measured against, and the difference between them is the provider's
/// propagation delay rather than our work.
///
/// There is deliberately no base fee or state here. An announcement is about
/// *which block exists*; everything about what happened inside it is read, once,
/// through [`evm_chain::ChainAdapter`], which is also where a simulation gets its
/// pinned header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BlockAnnouncement {
    pub chain_id: ChainId,
    pub number: BlockNumber,
    pub hash: alloy_primitives::B256,
    pub parent_hash: alloy_primitives::B256,
    pub chain_timestamp_secs: u64,
    pub transaction_count: u64,
    pub observed_at_unix_ms: u64,
    pub source: SourceKind,
}

/// A `pending`-tag observation: a block still being built. It has a number and
/// a hash that no reader can resolve (§ flashblocks capability record), so it is
/// evidence about the sequence, not evidence about the market.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FlashblockCandidate {
    pub chain_id: ChainId,
    pub number: BlockNumber,
    pub hash: alloy_primitives::B256,
    pub parent_hash: Option<alloy_primitives::B256>,
    pub chain_timestamp_secs: u64,
    pub transaction_count: u64,
    pub gas_used: u64,
    pub observed_at_unix_ms: u64,
}

/// The connection layer's own state, forwarded so a session record can show a
/// disconnect instead of pretending the run was uninterrupted (§48, §69).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum SourceStatus {
    Connected {
        source: SourceKind,
        attempt: u32,
    },
    Disconnected {
        source: SourceKind,
        reason: String,
    },
    Reconnecting {
        source: SourceKind,
        attempt: u32,
    },
    Failed {
        source: SourceKind,
        reason: String,
    },
    /// Subscribed, or not: which one, and the provider's words if not.
    Subscription {
        source: SourceKind,
        outcome: String,
    },
    /// A read cycle completed with nothing new. Proof the source is alive rather
    /// than merely quiet.
    Idle {
        source: SourceKind,
        head: Option<BlockNumber>,
    },
}

/// Why a gap ended the way it did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum GapOutcome {
    /// Every number in the range came back.
    Recovered { from: u64, to: u64, blocks: u64 },
    /// The provider never produced one of them, within the retry budget.
    Unrecovered {
        from: u64,
        to: u64,
        missing: u64,
        attempts: u32,
        detail: String,
    },
}

/// One thing a source can tell the pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum MarketEvent {
    /// A sealed block, in chain order. This is the only variant that advances
    /// state.
    Canonical(BlockAnnouncement),
    /// A `pending` observation. Never advances state (§27).
    Candidate(FlashblockCandidate),
    /// A candidate number resolved: its canonical block arrived, so the candidate
    /// window closes and the mapping between the two is recorded (§27's
    /// canonical-to-flashblock mapping, measured rather than assumed).
    ///
    /// `matched_hash` is the honest answer to "did the candidate predict the
    /// sealed block?". On this endpoint it is expected to be false: a candidate is
    /// a partial state, and a partial state that showed *some* of the block's
    /// transactions is not the block.
    CandidateResolved {
        number: u64,
        observations: u64,
        candidate_hashes: u64,
        matched_hash: bool,
        canonical_hash: String,
        /// What the candidate's last read claimed minus what the sealed block
        /// holds: negative means the candidate stream under-reported the block.
        transaction_count_delta: i64,
        /// Gas the candidate accumulated between its first and its last read for
        /// this number — the shape of a partial state being built.
        gas_used_growth: i64,
    },
    /// A candidate whose number stopped being pending and never became canonical
    /// within the holding budget (§30's expiry question, answered locally).
    CandidateExpired {
        number: u64,
        hash: String,
        observations: u64,
    },
    /// A number we were asked to skip past: the head moved beyond what we have,
    /// and the hole is being filled.
    GapDetected {
        from: u64,
        to: u64,
    },
    Gap(GapOutcome),
    /// The same block seen twice. Reported rather than swallowed silently (§51).
    Duplicate {
        number: u64,
        hash: String,
    },
    /// One number, two hashes. v0.1 does not resolve reorgs; it records that one
    /// was seen and refuses to overwrite the block already consumed (§37).
    CanonicalityConflict {
        number: u64,
        kept: String,
        rejected: String,
    },
    /// A block number below what the pipeline has already consumed.
    StaleAnnouncement {
        number: u64,
        next_expected: u64,
    },
    Status(SourceStatus),
    /// A frame or payload this layer could not name. An error class, not a panic
    /// (§52), and still counted so nothing vanishes.
    Unknown {
        detail: String,
    },
}

impl MarketEvent {
    pub const fn canonical_block(&self) -> Option<BlockNumber> {
        match self {
            Self::Canonical(announcement) => Some(announcement.number),
            _ => None,
        }
    }

    /// A one-word label for a counter and for the evidence line (§40).
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Canonical(_) => "canonical",
            Self::Candidate(_) => "candidate",
            Self::CandidateResolved { .. } => "candidate_resolved",
            Self::CandidateExpired { .. } => "candidate_expired",
            Self::GapDetected { .. } => "gap_detected",
            Self::Gap(GapOutcome::Recovered { .. }) => "gap_recovered",
            Self::Gap(GapOutcome::Unrecovered { .. }) => "gap_unrecovered",
            Self::Duplicate { .. } => "duplicate",
            Self::CanonicalityConflict { .. } => "canonicality_conflict",
            Self::StaleAnnouncement { .. } => "stale_announcement",
            Self::Status(SourceStatus::Connected { .. }) => "connected",
            Self::Status(SourceStatus::Disconnected { .. }) => "disconnected",
            Self::Status(SourceStatus::Reconnecting { .. }) => "reconnecting",
            Self::Status(SourceStatus::Failed { .. }) => "source_failed",
            Self::Status(SourceStatus::Subscription { .. }) => "subscription",
            Self::Status(SourceStatus::Idle { .. }) => "idle",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|error| {
            serde_json::json!({"unknown": {"detail": format!("event did not serialize: {error}")}})
        })
    }
}

/// Unix milliseconds, from the same clock the chain's own timestamps are
/// compared against in §70's latency table.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}
