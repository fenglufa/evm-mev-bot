//! M9.4: the preconfirmation model — what a *growing* `pending` view is, as a type.
//!
//! This file deliberately knows nothing about JSON. The wire shape was
//! characterized first (M9.4 §5/§6, recorded in `docs/v0.1/M9.4 Semantic Audit.md`)
//! and said three things that this model is built to survive:
//!
//! 1. there is **no frame index on the wire**. The pending header's field set is the
//!    standard reth block object; the only `index` anywhere is per-entry
//!    (`transactionIndex`, `logIndex`). So [`PreconfIdentity::wire_index`] is
//!    [`Field::Unknown`] on this endpoint *by measurement*, and the sequence number
//!    the radar actually uses is the locally assigned
//!    [`PreconfIdentity::local_frame_sequence`].
//! 2. the pending view **regrows at one height**: same `number`, new `hash`, longer
//!    transaction list. So a hash is not an identity a frame can be deduplicated by
//!    alone, and a height is not a frame.
//! 3. on the preconfirmation endpoint `stateRoot` is an **all-zero placeholder**, so
//!    the one field that would license reading state at that view is exactly the one
//!    the endpoint does not supply. That is modelled as [`Field::Unknown`] rather
//!    than as a zero worth believing, and it is why nothing downstream can ask this
//!    type for a reserve.
//!
//! ## What this type is forbidden to do (§4, §58)
//!
//! `PreconfirmationState` is a *separate* type from anything canonical. There is no
//! `From`/`Into`/`TryFrom` here that produces a state update, a reserve, a pool
//! state, or a `BlockAnnouncement`, and there is no `source: Rpc | Flashblocks`
//! union on any existing type: one variant set for preconfirmation, another for
//! canonical, never a field that toggles between them. `tests/preconf_isolation.rs`
//! turns that claim into a gate.

use alloy_primitives::{Address, B256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};

/// A field the model would like to have, and the honest record of whether the
/// endpoint actually supplied one (§6: unconfirmable is `Unknown`, never `Guessed`).
///
/// `Ord` exists for one reason (§43): record types that carry these fields must sort
/// deterministically, and the derived order is `Known` before `Unknown`, then by the
/// value / by `key_present`. It is a tie-break, never a semantic ranking.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Field<T> {
    /// The provider named it and this build knows what it means.
    Known(T),
    /// Not usable. `key_present` separates "the payload has no such key" from
    /// "the key is there but its meaning is not confirmed" — the second is a
    /// claim about the endpoint, the first is a claim about the protocol.
    Unknown {
        key_present: bool,
        detail: &'static str,
    },
}

impl<T> Field<T> {
    pub const fn known(value: T) -> Self {
        Self::Known(value)
    }

    pub const fn unknown(key_present: bool, detail: &'static str) -> Self {
        Self::Unknown {
            key_present,
            detail,
        }
    }

    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown { .. } => None,
        }
    }

    /// The reason string, or `None` when the field is known. Evidence tables print
    /// this instead of a fabricated value.
    pub fn unknown_reason(&self) -> Option<&'static str> {
        match self {
            Self::Known(_) => None,
            Self::Unknown { detail, .. } => Some(detail),
        }
    }

    /// `"known"` / `"unknown"`, for the counters that must never be a `bool`
    /// pretending to be a tri-state.
    pub const fn state_label(&self) -> &'static str {
        match self {
            Self::Known(_) => "known",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// §8's identity, with the wire/measurement distinction kept visible.
///
/// `local_frame_sequence` is assigned by [`crate::preconf_radar::EarlyRadar`] in
/// read order and is *not* a provider field; `wire_index` records what the provider
/// did or did not offer. Two frames with equal identity are the same view of the
/// same block, which is the only deduplication this endpoint supports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfIdentity {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    /// Assigned by the read loop in read order
    /// (`crate::preconf_radar::EarlyRadar::next_local_sequence`) — a session-local
    /// counter, not a provider field.
    pub local_frame_sequence: u64,
    /// What the payload said about a frame index. Always
    /// [`Field::Unknown { key_present: false, .. }`] on the endpoint measured in
    /// M9.4 — kept as a field so the absence is evidence rather than an omission.
    pub wire_index: Field<u64>,
    /// The parent this view claims to build on. §9's wrong-parent check compares
    /// this against the identity of the block below.
    pub parent_hash: Field<B256>,
    /// The view's own hash. It moves *within* one block number, so it identifies a
    /// frame, never a block.
    pub view_hash: Field<B256>,
}

/// One entry of a pending block's transaction list, as far as the radar is
/// allowed to know it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfTransaction {
    pub hash: B256,
    /// `transactionIndex` as the payload gave it. Measured present on this
    /// endpoint, unlike the frame index.
    pub index: Field<u64>,
    pub from: Field<Address>,
    pub to: Field<Address>,
    /// First four calldata bytes, the only part of the input the radar reads.
    pub selector: Field<[u8; 4]>,
}

/// One log entry, as the preconfirmation layer holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfLog {
    pub emitter: Address,
    pub topic0: Field<B256>,
    pub log_index: Field<u64>,
    pub transaction_index: Field<u64>,
    /// `removed` as delivered. On a pending read the endpoint answered `false`
    /// throughout the M9.4 capture, but the field is kept because §9 asks whether
    /// a retracted log can reach the radar.
    pub removed: bool,
    pub transaction_hash: B256,
}

/// One receipt from the pending block's receipt list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfReceipt {
    pub transaction_hash: B256,
    pub index: Field<u64>,
    /// `status`: `Some(true)` success, `Some(false)` reverted, `None` the payload
    /// did not say. A reverted receipt must never yield an affected pool (NC10).
    pub status: Field<bool>,
    pub logs: Vec<PreconfLog>,
    /// The block the receipt claims to belong to (§9's wrong-block check).
    pub claimed_block_number: Field<BlockNumber>,
    pub claimed_block_hash: Field<B256>,
}

/// One read of the pending view: a snapshot of a block that has not sealed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfirmationFrame {
    pub identity: PreconfIdentity,
    pub chain_timestamp_secs: Field<u64>,
    pub gas_used: Field<u64>,
    pub transaction_count: usize,
    pub transactions: Vec<PreconfTransaction>,
    /// The state root as the endpoint supplied it. [`Field::Unknown`] when it is
    /// the all-zero placeholder — which is every sample taken from the
    /// preconfirmation host in M9.4's capture.
    pub state_root: Field<B256>,
    /// When *this process* read it. §26's first preconfirmation timestamp.
    pub observed_at_unix_ms: u64,
    /// Which endpoint answered, as a digest (§33: never a URL in evidence).
    pub endpoint_id: String,
}

impl PreconfirmationFrame {
    pub fn number(&self) -> BlockNumber {
        self.identity.block_number
    }
}

/// §10's explicit state machine. A frame sequence for one block number moves
/// through these, and only these; nothing in the radar is a `bool` that means
/// "kind of stale".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum PreconfStage {
    /// Nothing has been read for this number yet.
    Empty,
    /// At least one frame held; a newer frame is expected.
    Streaming,
    /// A canonical block at this number has been seen, so the view is closed.
    Reconciled,
    /// A canonical block at this number has been seen and did not agree with the
    /// held view; the view is closed and canonical won (§17).
    Superseded,
    /// The view was thrown away without a canonical block: a gap, a wrong parent,
    /// or a disconnect (§11, §24).
    Invalidated,
    /// The view aged out without sealing (§30's expiry, kept local).
    Expired,
    /// A frame arrived for this number after it had already closed; compared and
    /// discarded, never merged (§19).
    DiscardedLate,
}

impl PreconfStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Streaming => "streaming",
            Self::Reconciled => "reconciled",
            Self::Superseded => "superseded",
            Self::Invalidated => "invalidated",
            Self::Expired => "expired",
            Self::DiscardedLate => "discarded_late",
        }
    }

    /// A closed stage holds no live view: no further frame may extend it, only be
    /// compared against it and discarded (§19).
    pub const fn is_closed(self) -> bool {
        !matches!(self, Self::Empty | Self::Streaming)
    }

    /// §10 asks for the transitions to be enumerable rather than implied. This is
    /// the whole edge set; [`EarlyRadar`][crate::preconf_radar::EarlyRadar] may not
    /// move a stage anywhere else.
    pub const fn can_move_to(self, next: Self) -> bool {
        match self {
            Self::Empty => matches!(next, Self::Streaming),
            Self::Streaming => matches!(
                next,
                Self::Reconciled
                    | Self::Superseded
                    | Self::Invalidated
                    | Self::Expired
                    | Self::DiscardedLate
            ),
            Self::Reconciled
            | Self::Superseded
            | Self::Invalidated
            | Self::Expired
            | Self::DiscardedLate => false,
        }
    }
}

/// The held view for one block number: the frames seen so far, collapsed to what
/// the radar is allowed to claim.
///
/// Growth is monotone on this endpoint (measured: transaction count up 27 times,
/// down 0), but the *held* view is the last frame rather than a merge, because a
/// merge would be a second state derivation (§17's no-merge rule applied before
/// canonical even arrives).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreconfirmationState {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    pub stage: PreconfStage,
    pub frames_observed: u64,
    pub distinct_view_hashes: u64,
    /// Radar cycles this view has been held open without sealing. The expiry budget
    /// (§30) counts cycles, not frames: a view that is being re-read forever is still
    /// a view that has not become a block.
    pub cycles_held: u32,
    pub first: PreconfirmationFrame,
    pub latest: PreconfirmationFrame,
    /// Transaction hashes the latest frame holds, in payload order.
    pub transaction_hashes: Vec<B256>,
    /// Every distinct view hash this number has been seen under, bounded by the
    /// config window. §16's "did the candidate ever *be* the block" question is
    /// answered against this list; on the endpoint measured in M9.4 it is empty of
    /// matches by construction, and the count is reported anyway.
    pub view_hashes: Vec<B256>,
    /// Affected pools derived from the latest frame (§12–§14). Reserves are not in
    /// this type and cannot be.
    pub affected_pools: Vec<AffectedPool>,
    /// §26's stamps, per block number, as they were first seen.
    pub timestamps: PreconfTimestamps,
}

impl PreconfirmationState {
    /// §26's seventh stamp, kept monotone: a frame that arrived out of wall-clock
    /// order must not shrink the observed growth window, because a negative
    /// `view_growth_ms` would then read as a reorg rather than as a source that
    /// rewound — and §9 already records the rewind where it actually belongs.
    pub fn bump_last_received(&mut self, observed_at_unix_ms: u64) {
        self.timestamps.flashblock_last_received_at_unix_ms = Some(
            self.timestamps
                .flashblock_last_received_at_unix_ms
                .unwrap_or(observed_at_unix_ms)
                .max(observed_at_unix_ms),
        );
    }
}

/// §12's output: a registered pool that a preconfirmation frame touched — one entry
/// per (pool, transaction, log), not one entry per pool.
///
/// §13 asks the model to carry pool id, transaction hash, block number, flashblock
/// index and event identity, and explicitly forbids inventing fields to complete the
/// list. So `local_frame_sequence` stands in for `flashblock_index` *named as what it
/// is* (a locally assigned read counter — no wire index exists on this endpoint), and
/// `transaction_index`, `log_index` and `topic0` are [`Field`]s: filled when the
/// payload said, [`Field::Unknown`] when it did not. NC12's three changes to one pool
/// in one block stay three entries, because a set that deduplicated to "this pool was
/// touched" would throw away exactly the identity §13 asks to keep.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AffectedPool {
    pub chain_id: ChainId,
    pub block_number: u64,
    pub pool: Address,
    /// How the frame named it: a transaction sent *to* the pool, or a log emitted
    /// *by* it. The two are different strengths of evidence and the report says
    /// which one it is standing on.
    pub reason: AffectedReason,
    pub transaction_hash: B256,
    pub local_frame_sequence: u64,
    pub transaction_index: Field<u64>,
    pub log_index: Field<u64>,
    pub topic0: Field<B256>,
}

impl AffectedPool {
    /// The identity a re-read must not duplicate. Deliberately excludes
    /// [`Self::local_frame_sequence`]: the same transaction seen in a later frame of
    /// the same height is the same finding, while the same pool touched by two
    /// transactions is two findings (NC12).
    pub fn identity_key(&self) -> (u64, Address, AffectedReason, B256, Field<u64>) {
        (
            self.block_number,
            self.pool,
            self.reason,
            self.transaction_hash,
            self.log_index.clone(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum AffectedReason {
    /// `to == pool`. A call, not yet an effect: no receipt backs it.
    TransactionTarget,
    /// A log emitted by the pool in a receipt whose status was success.
    SuccessfulLogEmitter,
}

impl AffectedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TransactionTarget => "transaction_target",
            Self::SuccessfulLogEmitter => "successful_log_emitter",
        }
    }
}

/// §26's six instrumentation stamps, per held block number. Each is `None` until the
/// event that would fill it has happened, and a `None` prints `N/A` — never 0 (§28).
///
/// The first four are taken from the *first* frame that reached each step, because
/// §27's lead is a claim about when the radar first knew, not about when it finished
/// catching up. `flashblock_last_received_at_unix_ms` is beyond §26's list and exists
/// because the semantic audit measured one height being read many times: a single
/// "received" stamp would hide the growth window that is the whole finding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PreconfTimestamps {
    /// The block's own `timestamp`, in seconds — the chain's clock, kept separate
    /// from the process clocks below so a derived latency never mixes them silently.
    pub chain_timestamp_secs: Option<u64>,
    /// `flashblock_received_at`: the payload arrived from the provider.
    pub flashblock_received_at_unix_ms: Option<u64>,
    /// `flashblock_decoded_at`: this model's shape exists for it.
    pub flashblock_decoded_at_unix_ms: Option<u64>,
    /// `sequence_validated_at`: §9's checks passed and the frame was held.
    pub sequence_validated_at_unix_ms: Option<u64>,
    /// `affected_pool_detected_at`: the first pool the radar could name.
    pub affected_pool_detected_at_unix_ms: Option<u64>,
    /// `canonical_seen_at`: the canonical block that closed this number.
    pub canonical_seen_at_unix_ms: Option<u64>,
    /// `reconciled_at`: the verdict itself.
    pub reconciled_at_unix_ms: Option<u64>,
    /// Beyond §26: the last frame read for this number.
    pub flashblock_last_received_at_unix_ms: Option<u64>,
}

/// §26's six derived latencies, in milliseconds, signed where the sign is the
/// finding. `None` means one of the pair is missing, and prints `N/A` (§28).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PreconfLatencies {
    /// `stream_latency`: received → decoded. Our own decode cost.
    pub stream_latency_ms: Option<i64>,
    /// `decode_latency`: decoded → sequence validated. §9's validation cost.
    pub decode_latency_ms: Option<i64>,
    /// `affected_pool_detection_latency`: received → first pool named.
    pub affected_pool_detection_latency_ms: Option<i64>,
    /// `canonical_lag`: canonical detection minus the block's own chain timestamp.
    /// How far behind the detector is, on the canonical side.
    pub canonical_lag_ms: Option<i64>,
    /// `reconciliation_latency`: canonical detection → verdict.
    pub reconciliation_latency_ms: Option<i64>,
    /// §27's headline, `flashblocks_lead` =
    /// `canonical_detection_time − flashblock_detection_time`, where the flashblock
    /// detection time is the first frame *received* for that number. Positive means
    /// the preconfirmation view was seen first; zero and negative are recorded as
    /// themselves (§27: a lead cannot be proven by choosing the window).
    pub flashblocks_lead_ms: Option<i64>,
    /// Beyond §26: first frame received → last frame received for one height.
    pub view_growth_ms: Option<i64>,
}

fn diff_ms(later: Option<u64>, earlier: Option<u64>) -> Option<i64> {
    match (later, earlier) {
        (Some(later), Some(earlier)) => Some(later as i64 - earlier as i64),
        _ => None,
    }
}

impl PreconfLatencies {
    /// Derived from one number's stamps. No clock is consulted here, so the same
    /// record recomputes to the same table (§43).
    pub fn from_timestamps(stamps: &PreconfTimestamps) -> Self {
        let chain_lag = match (
            stamps.canonical_seen_at_unix_ms,
            stamps.chain_timestamp_secs,
        ) {
            (Some(seen), Some(chain_secs)) => Some(seen as i64 - chain_secs as i64 * 1000),
            _ => None,
        };
        Self {
            stream_latency_ms: diff_ms(
                stamps.flashblock_decoded_at_unix_ms,
                stamps.flashblock_received_at_unix_ms,
            ),
            decode_latency_ms: diff_ms(
                stamps.sequence_validated_at_unix_ms,
                stamps.flashblock_decoded_at_unix_ms,
            ),
            affected_pool_detection_latency_ms: diff_ms(
                stamps.affected_pool_detected_at_unix_ms,
                stamps.flashblock_received_at_unix_ms,
            ),
            canonical_lag_ms: chain_lag,
            reconciliation_latency_ms: diff_ms(
                stamps.reconciled_at_unix_ms,
                stamps.canonical_seen_at_unix_ms,
            ),
            flashblocks_lead_ms: diff_ms(
                stamps.canonical_seen_at_unix_ms,
                stamps.flashblock_received_at_unix_ms,
            ),
            view_growth_ms: diff_ms(
                stamps.flashblock_last_received_at_unix_ms,
                stamps.flashblock_received_at_unix_ms,
            ),
        }
    }

    /// The §51 rule in code form: a non-positive lead is recorded as what it is.
    pub const fn lead_is_positive(&self) -> Option<bool> {
        match self.flashblocks_lead_ms {
            Some(lead) => Some(lead > 0),
            None => None,
        }
    }
}

/// The error taxonomy §40 asks for — and the boundary it sits on.
///
/// A returned error means *this payload could not become a frame*, which is the only
/// decision a read loop has to make atomically: decode, or fail closed. Everything
/// else §9–§19 names — a wrong parent, a regressed height, a frame for a height that
/// already sealed, a view thrown away at reconnect — is a decision the radar makes
/// *about a frame it did decode*, and those are reported as
/// [`RadarEvent`]s plus [`RadarCounters`] fields, not as errors: a live run that
/// aborted over a bookkeeping claim would be less informative than one that carried
/// on and said exactly what it refused (§51). No variant here is a name the code
/// never returns — an unused error class is a claim that a path exists.
#[derive(Debug, thiserror::Error)]
pub enum PreconfError {
    #[error("payload field `{0}` is missing or not a hex quantity; fail-closed")]
    Decode(&'static str),
    #[error("payload transaction is not a full object (hash-only list), so the radar cannot name its target")]
    HashOnlyTransaction,
    /// An entry that is neither a full object nor a bare 32-byte hash — a number, a
    /// bool, an array, a string of the wrong length. Naming the actual kind matters:
    /// reporting a `null` entry as "hash-only" describes a shape the payload never
    /// had, and sends whoever reads the line looking for the wrong provider behaviour.
    #[error("pending transactions[{index}] is a {kind}, which is neither a full transaction object nor a bare hash; fail-closed")]
    UnexpectedTransactionEntry { index: usize, kind: &'static str },
    #[error("provider read failed: {0}")]
    Transport(String),
    /// §49: the only failure that ends a run *as an error* rather than as a report.
    /// A link that cannot hand its events anywhere has to say so, because the
    /// alternative — dropping frames and carrying on — is the failure mode §24/§25
    /// forbid.
    #[error("the radar event queue closed after {events_sent} event(s): the link stopped rather than dropping frames")]
    EventQueueClosed { events_sent: u64 },
}

/// The verdict of comparing a held preconfirmation view with the block that
/// sealed at the same number (§15–§18).
///
/// Content, not hash: measured 0/48 hash equalities against 23/48 content
/// equalities, so a hash comparison would report "nothing ever matches" while the
/// view was in fact an exact prefix of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ReconciliationVerdict {
    /// The view's transaction list was exactly the sealed list.
    ContentEqual,
    /// The view held a prefix of the sealed list: a partial block, as expected.
    ContentPrefix { held: u64, sealed: u64 },
    /// The view held transactions the sealed block does not.
    ContentSuperset { held: u64, sealed: u64 },
    /// The two lists differ by more than order: §17 makes canonical win, no merge.
    ContentDivergent {
        only_in_view: u64,
        only_in_canonical: u64,
    },
    /// Nothing was held for this number: the view arrived after the block sealed,
    /// or never arrived.
    NoView,
    /// A preconfirmation frame arrived for a number that had already sealed.
    /// Compared for the record, then discarded (§19).
    LateFrameDiscarded {
        number: u64,
        sealed_before_unix_ms: u64,
    },
}

impl ReconciliationVerdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContentEqual => "content_equal",
            Self::ContentPrefix { .. } => "content_prefix",
            Self::ContentSuperset { .. } => "content_superset",
            Self::ContentDivergent { .. } => "content_divergent",
            Self::NoView => "no_view",
            Self::LateFrameDiscarded { .. } => "late_frame_discarded",
        }
    }

    /// §17: canonical wins in every class, and the classes differ only in what
    /// gets recorded about the view. This is the one method that says so.
    pub const fn canonical_wins(&self) -> bool {
        true
    }
}

/// What the radar did, as an event. Kept separate from
/// [`crate::event::MarketEvent`]: the canonical event type is what advances the
/// pipeline, and nothing in this file produces one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RadarEvent {
    /// A frame was accepted and the held view moved forward.
    FrameAccepted {
        number: u64,
        local_frame_sequence: u64,
        transaction_count: usize,
        duplicate_view_hash: bool,
    },
    /// A frame was refused, with the class of refusal.
    FrameRejected { number: u64, reason: &'static str },
    /// §9's sequence findings that do not kill a frame: a regressed number, a
    /// height that was never pending.
    Sequence { detail: String },
    /// A pool named by a frame, derived per §12.
    PoolAffected(AffectedPool),
    /// A canonical block closed a held view.
    Reconciled {
        number: u64,
        verdict: ReconciliationVerdict,
        /// Whether the sealed block's own hash was among the view's hashes.
        /// Measured structurally unfireable on this endpoint; kept because a
        /// different endpoint could make it true.
        hash_matched: bool,
        latencies: PreconfLatencies,
    },
    /// The held view was thrown away without canonical backing.
    ViewInvalidated { number: u64, reason: &'static str },
    /// A held view aged out (§30, local budget).
    ViewExpired { number: u64, frames_observed: u64 },
    /// The provider's own stage moved; recorded so a report can say which stage
    /// ended the run rather than inferring it.
    StageChanged {
        number: u64,
        from: &'static str,
        to: &'static str,
    },
    /// §10's *transport* machine moving. Kept separate from [`Self::StageChanged`],
    /// which is per block number: a link has no height, and pretending it did would
    /// put a fake block number into the sequence table.
    LinkStageChanged {
        from: &'static str,
        to: &'static str,
        detail: String,
    },
}

impl RadarEvent {
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::FrameAccepted { .. } => "frame_accepted",
            Self::FrameRejected { .. } => "frame_rejected",
            Self::Sequence { .. } => "sequence",
            Self::PoolAffected { .. } => "pool_affected",
            Self::Reconciled { .. } => "reconciled",
            Self::ViewInvalidated { .. } => "view_invalidated",
            Self::ViewExpired { .. } => "view_expired",
            Self::StageChanged { .. } => "stage_changed",
            Self::LinkStageChanged { .. } => "link_stage_changed",
        }
    }
}

/// Every counter the report prints, in one place, so an independent recomputation
/// has a fixed target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RadarCounters {
    pub frames_offered: u64,
    pub frames_accepted: u64,
    pub frames_rejected: u64,
    pub frames_duplicate: u64,
    /// A frame whose chain identity is not this radar's (§9). The other half of §9's
    /// "wrong block" question is answered by [`Self::receipts_wrong_block`] (a receipt
    /// naming a block other than the frame it was read with) and by
    /// [`Self::frames_late`] (a frame naming a height the canonical path already
    /// sealed) — three witnesses for three different defects, rather than one counter
    /// standing in for all of them.
    pub frames_wrong_chain: u64,
    pub frames_wrong_parent: u64,
    pub frames_late: u64,
    pub frames_regressed: u64,
    pub heights_seen: u64,
    /// Heights that showed more than one distinct view hash (§2.2 of the audit).
    pub heights_multi_view: u64,
    /// Heights that reached the §48 budget of three distinct views.
    pub heights_ge_three_views: u64,
    pub state_root_placeholders: u64,
    pub transactions_observed: u64,
    pub affected_pools_emitted: u64,
    pub affected_pools_by_target: u64,
    pub affected_pools_by_log: u64,
    pub reverted_receipts_skipped: u64,
    pub canonical_closures: u64,
    pub reconciled_content_equal: u64,
    pub reconciled_content_prefix: u64,
    pub reconciled_content_superset: u64,
    pub reconciled_content_divergent: u64,
    pub reconciled_no_view: u64,
    pub reconciled_hash_match: u64,
    pub views_invalidated: u64,
    pub views_expired: u64,
    /// §9's parent check, three-way because "the chain below is not known yet" and
    /// "the frame names the wrong parent" are different findings and neither is a
    /// pass.
    pub parent_checks_passed: u64,
    pub parent_checks_uncheckable: u64,
    pub parent_unconfirmed: u64,
    /// §19: frames that arrived after their number had already sealed, compared for
    /// the record and discarded.
    pub late_frames_compared: u64,
    /// A frame for a number whose view had already closed, so it started a fresh
    /// view instead of extending a closed one (§10).
    pub views_replaced_after_closure: u64,
    /// Frames whose transaction list was cut by the memory bound. The entries past
    /// the bound are counted here rather than silently dropped (§51).
    pub frames_truncated: u64,
    /// §9's wrong-block check, applied to the receipt list read against a frame.
    pub receipts_wrong_block: u64,
    /// Logs carrying `removed: true` that were excluded from the affected set.
    pub receipts_removed_logs: u64,
    /// §16's affected-pool identity check, three-way because the canonical side may
    /// simply not have supplied a target list — which is "not verified", not "no
    /// pools changed" (§28's rule that missing data is `N/A`).
    pub reconciled_pools_matched: u64,
    pub reconciled_pools_diverged: u64,
    pub reconciled_pools_unverified: u64,
    /// §9's question, answered as unmeasurable rather than as zero: no wire index
    /// exists, so a missing frame number cannot be distinguished from a quiet one.
    pub sequence_gaps_measurable: bool,
}
