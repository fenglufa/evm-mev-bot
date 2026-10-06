//! M9.4 §9–§19: the early radar — the only thing a preconfirmation view is allowed
//! to become.
//!
//! The whole file is a policy statement in type form:
//!
//! * a frame is **validated** before it is held (§9: wrong chain, late, regressed,
//!   duplicate, wrong parent), and every refusal is emitted as an event rather than
//!   swallowed (§51's "reported, never silently forgotten");
//! * the held view yields [`AffectedPool`]s and nothing else (§12–§14). There is no
//!   reserve here, no price, no path, and no second EVM: the radar's entire output
//!   vocabulary is "this registered pool was named by this not-yet-sealed block";
//! * when the canonical block arrives the view is **compared and then closed**
//!   (§15–§18). The comparison is by content, because M9.4 measured 0/48 hash
//!   matches against 23/48 content matches; a divergence never merges — canonical
//!   wins by [`ReconciliationVerdict::canonical_wins`], which is a constant and not
//!   a branch;
//! * a late frame is compared and discarded (§19), and a reconnect invalidates every
//!   open view (§11), because this endpoint offers no cursor or replay to resume
//!   against — the one case where "unless replay/cursor is proven" is not merely
//!   unimplemented but unprovable from the measured protocol (no frame index exists,
//!   `docs/v0.1/M9.4 Semantic Audit.md` §2.3).
//!
//! Determinism (§41/§43): every collection walked for output is a `BTreeMap`, a
//! `BTreeSet`, or a sorted `Vec`, and no clock is read inside this file. Timestamps
//! arrive on the frame, so the same frames in the same order produce byte-identical
//! evidence.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, B256};
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};

use crate::preconf::{
    AffectedPool, AffectedReason, Field, PreconfLatencies, PreconfReceipt, PreconfStage,
    PreconfTimestamps, PreconfirmationFrame, PreconfirmationState, RadarCounters, RadarEvent,
    ReconciliationVerdict,
};

/// The radar's own bounds. Every one of them is a memory guard, and every one that
/// bites is counted — a bound that silently truncated would turn §51's honesty into
/// a number nobody can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RadarConfig {
    /// How many block numbers may be held at once.
    pub max_heights_held: usize,
    /// How many radar cycles an open view survives without sealing (§30's expiry,
    /// answered locally because no payload field answers it).
    ///
    /// The budget is in *cycles*, not wall-clock seconds, so it has to be read together
    /// with the link's `read_interval_ms`: at the link's 200 ms default this is ~12 s,
    /// comfortably longer than the ~1.5 s a sealed block on this chain takes to reach the
    /// canonical detector, and short enough that a view whose block never sealed cannot sit
    /// in memory for the whole run.
    pub expiry_cycles: u32,
    /// Closed views and sealed digests are kept this far below the head, so a late
    /// frame still has something to be compared against (§19), and no further.
    pub closure_retention: u64,
    /// Upper bound on the transactions one frame contributes to the affected scan.
    pub max_transactions_per_frame: usize,
    /// Upper bound on view hashes remembered per height.
    pub max_views_per_height: usize,
}

impl Default for RadarConfig {
    fn default() -> Self {
        Self {
            max_heights_held: 8,
            expiry_cycles: 60,
            closure_retention: 4,
            max_transactions_per_frame: 4096,
            max_views_per_height: 16,
        }
    }
}

/// §13's "don't invent a field to complete the model", stated once per absent field
/// so the evidence table can print why a cell is empty.
const NO_LOG_DETAIL: &str = "no log backs this entry: the pool was named by a call target";
const NO_TOPIC_DETAIL: &str =
    "no event topic backs this entry: the pool was named by a call target";

/// The sealed block, in the only form the radar is allowed to see it: identity and
/// content, with no state in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CanonicalDigest {
    pub chain_id: ChainId,
    pub number: BlockNumber,
    pub hash: B256,
    pub parent_hash: B256,
    pub chain_timestamp_secs: u64,
    pub transaction_hashes: Vec<B256>,
    /// §16's affected-pool identity, to the extent the canonical read proves it.
    /// `None` means the canonical side supplied no target list, and the pool check is
    /// then recorded as unverified — not as "no pool changed" (§28: missing data is
    /// `N/A`, never an empty set dressed up as a zero).
    pub affected_pools: Option<Vec<Address>>,
    pub observed_at_unix_ms: u64,
}

/// The registered-pool set the radar is pointed at (§12).
///
/// Deliberately a set of addresses and nothing more: the radar must not need a pool
/// *state*, a fee, or a graph, which is what keeps §20's ban on
/// Flashblocks→PathFinder from being negotiable later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolSet {
    chain_id: ChainId,
    pools: BTreeSet<Address>,
}

impl PoolSet {
    pub fn new(chain_id: ChainId, pools: impl IntoIterator<Item = Address>) -> Self {
        Self {
            chain_id,
            pools: pools.into_iter().collect(),
        }
    }

    pub const fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    pub fn len(&self) -> usize {
        self.pools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    pub fn contains(&self, address: &Address) -> bool {
        self.pools.contains(address)
    }
}

/// A frame plus the receipts read against the same height, as one unit of input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadarInput {
    pub frame: PreconfirmationFrame,
    /// Already decoded. `None` when the source supplies no receipts at all, which is
    /// every live read on this endpoint (§34).
    pub receipts: Option<Vec<PreconfReceipt>>,
    /// §26's `flashblock_decoded_at`, stamped by the caller: the decode happens in
    /// the read loop, so the loop knows the number and the radar must not invent one.
    pub decoded_at_unix_ms: u64,
}

/// §26's remaining stamps (`sequence_validated_at`, `affected_pool_detected_at`,
/// `reconciled_at`) fall *inside* a radar call, so the radar cannot be handed their
/// value beforehand and must ask for it. The clock stays a caller-supplied function:
/// the radar never names a system clock itself, which is what lets a replay pass the
/// recorded stamps back in and reproduce byte-identical evidence (§41, §43).
pub type Stamp<'a> = &'a mut dyn FnMut() -> u64;

/// One frame read, bundled so [`EarlyRadar::extend`] does not need seven arguments.
struct FrameRead<'a> {
    frame: &'a PreconfirmationFrame,
    receipts: Option<&'a [PreconfReceipt]>,
    decoded_at_unix_ms: u64,
    view_hash: Option<B256>,
}

/// §9's parent question, with the three answers that are actually distinct. A
/// boolean would collapse "cannot check" into "checked and fine", and that collapse
/// is exactly the kind of claim §6 forbids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentOutcome {
    /// Nothing sealed directly below this height in this session, so continuity is
    /// not checkable — a statement about the session, not a pass.
    NotCheckable,
    /// The frame names no parent at all. Unconfirmed, never guessed (§6).
    Unconfirmed,
    Continues,
    Conflicts {
        parent: B256,
        expected: B256,
    },
}

/// The radar. Owns the held views, the sealed digests, and the counters; owns
/// nothing else, and in particular owns no state the pipeline could read.
pub struct EarlyRadar {
    chain_id: ChainId,
    endpoint_id: String,
    config: RadarConfig,
    pools: PoolSet,
    views: BTreeMap<BlockNumber, PreconfirmationState>,
    /// `number → digest`, for the parent-continuity check and §19's late comparison.
    sealed: BTreeMap<BlockNumber, CanonicalDigest>,
    highest_canonical: Option<BlockNumber>,
    last_accepted_number: Option<BlockNumber>,
    local_sequence: u64,
    counters: RadarCounters,
}

impl EarlyRadar {
    pub fn new(
        chain_id: ChainId,
        endpoint_id: impl Into<String>,
        pools: PoolSet,
        config: RadarConfig,
    ) -> Self {
        Self {
            chain_id,
            endpoint_id: endpoint_id.into(),
            config,
            pools,
            views: BTreeMap::new(),
            sealed: BTreeMap::new(),
            highest_canonical: None,
            last_accepted_number: None,
            local_sequence: 0,
            // §9's gap question is answered `false` once, here, from the measured
            // protocol: with no frame index there is nothing whose absence could be
            // detected. Reporting it as `false` rather than omitting it is the
            // difference between "we could not measure" and "we measured nothing".
            counters: RadarCounters {
                sequence_gaps_measurable: false,
                ..RadarCounters::default()
            },
        }
    }

    pub const fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// Which endpoint this radar is pointed at, as a digest (§33).
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    pub const fn config(&self) -> &RadarConfig {
        &self.config
    }

    pub const fn counters(&self) -> &RadarCounters {
        &self.counters
    }

    /// The session-local frame counter, assigned in read order. The wire offers no
    /// index (§2.3 of the audit), so this is the only sequence number the radar has —
    /// and it is labelled local everywhere it appears.
    pub fn next_local_sequence(&mut self) -> u64 {
        self.local_sequence += 1;
        self.local_sequence
    }

    pub fn held_heights(&self) -> Vec<BlockNumber> {
        self.views.keys().copied().collect()
    }

    pub fn view(&self, number: BlockNumber) -> Option<&PreconfirmationState> {
        self.views.get(&number)
    }

    /// Every held height, in chain order, as printable records (§31's sequence
    /// table). Deterministic by construction: `views` is a `BTreeMap`.
    pub fn records(&self) -> Vec<HeightRecord> {
        self.views
            .keys()
            .filter_map(|n| self.height_record(*n))
            .collect()
    }

    /// §26's six timestamps and §28's derived latencies, per height, plus the two
    /// field-availability facts the report claims per sample rather than globally.
    pub fn height_record(&self, number: BlockNumber) -> Option<HeightRecord> {
        let state = self.views.get(&number)?;
        Some(HeightRecord {
            number: number.0,
            stage: state.stage,
            frames_observed: state.frames_observed,
            distinct_view_hashes: state.distinct_view_hashes,
            transactions_in_latest: state.transaction_hashes.len() as u64,
            affected_pools: state.affected_pools.len() as u64,
            state_root_known: state.latest.state_root.is_known(),
            // Always false on the endpoint measured in M9.4 (§2.3), recorded per
            // height so the claim is checkable rather than asserted.
            wire_index_known: state.latest.identity.wire_index.is_known(),
            timestamps: state.timestamps,
            latencies: PreconfLatencies::from_timestamps(&state.timestamps),
            endpoint_id: state.latest.endpoint_id.clone(),
        })
    }

    fn fresh_state(&self, frame: &PreconfirmationFrame) -> PreconfirmationState {
        PreconfirmationState {
            chain_id: self.chain_id,
            block_number: frame.number(),
            stage: PreconfStage::Empty,
            frames_observed: 0,
            distinct_view_hashes: 0,
            cycles_held: 0,
            first: frame.clone(),
            latest: frame.clone(),
            transaction_hashes: Vec::new(),
            view_hashes: Vec::new(),
            affected_pools: Vec::new(),
            timestamps: PreconfTimestamps::default(),
        }
    }

    /// §10's edge set, applied to a state in place. An illegal move is reported and
    /// refused — the stage stays where it was — because a panic here would be §40's
    /// first violation and would end a live run over a bookkeeping claim.
    fn apply_stage(state: &mut PreconfirmationState, next: PreconfStage) -> Option<RadarEvent> {
        if state.stage == next {
            return None;
        }
        if !state.stage.can_move_to(next) {
            return Some(RadarEvent::Sequence {
                detail: format!(
                    "height {} is {}; §10 forbids {} -> {}, so the frame was held and the stage left alone",
                    state.block_number.0,
                    state.stage.as_str(),
                    state.stage.as_str(),
                    next.as_str()
                ),
            });
        }
        let from = state.stage.as_str();
        state.stage = next;
        Some(RadarEvent::StageChanged {
            number: state.block_number.0,
            from,
            to: next.as_str(),
        })
    }

    fn check_parent(&self, frame: &PreconfirmationFrame) -> ParentOutcome {
        let number = frame.number();
        let below = BlockNumber(number.0.saturating_sub(1));
        let Some(expected) = self.sealed.get(&below).map(|digest| digest.hash) else {
            return ParentOutcome::NotCheckable;
        };
        match frame.identity.parent_hash.value() {
            None => ParentOutcome::Unconfirmed,
            Some(parent) if *parent == expected => ParentOutcome::Continues,
            Some(parent) => ParentOutcome::Conflicts {
                parent: *parent,
                expected,
            },
        }
    }

    /// §9 + §10 + §19 + §24/§25: validate a frame, and if it survives, hold it.
    pub fn ingest(&mut self, input: RadarInput, stamp: Stamp<'_>) -> Vec<RadarEvent> {
        let mut events = Vec::new();
        let decoded_at_unix_ms = input.decoded_at_unix_ms;
        let frame = input.frame;
        let receipts = input.receipts;
        let number = frame.number();
        self.counters.frames_offered += 1;

        // — wrong chain identity: this frame is not this radar's to hold at all.
        if frame.identity.chain_id != self.chain_id {
            self.counters.frames_rejected += 1;
            self.counters.frames_wrong_chain += 1;
            events.push(RadarEvent::FrameRejected {
                number: number.0,
                reason: "chain_id_mismatch",
            });
            return events;
        }

        // — §19: this number has already sealed. Compare for the record, discard.
        if self
            .highest_canonical
            .is_some_and(|head| number.0 <= head.0)
        {
            self.counters.frames_rejected += 1;
            self.counters.frames_late += 1;
            self.counters.late_frames_compared += 1;
            let sealed = self.sealed.get(&number);
            let held = self.held_transactions(&frame);
            let comparison = match sealed {
                Some(digest) => compare_content(&held, &digest.transaction_hashes)
                    .as_str()
                    .to_string(),
                None => "sealed_digest_pruned".to_string(),
            };
            let sealed_before_unix_ms = sealed
                .map(|digest| digest.observed_at_unix_ms)
                .unwrap_or(frame.observed_at_unix_ms);
            events.push(RadarEvent::FrameRejected {
                number: number.0,
                reason: "late_after_canonical",
            });
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "frame for {} arrived after canonical had already sealed it (detected at {sealed_before_unix_ms} ms); compared by content: {comparison}; discarded, never merged (§19, §17: canonical wins)",
                    number.0
                ),
            });
            // A still-open view at a number the head has passed closes as late; a
            // closed or absent one needs nothing.
            if let Some(state) = self.views.get_mut(&number) {
                if let Some(change) = Self::apply_stage(state, PreconfStage::DiscardedLate) {
                    events.push(change);
                }
            }
            return events;
        }

        // — §9's sequence findings that do not by themselves refuse the frame.
        if let Some(last) = self.last_accepted_number {
            if number.0 < last.0 {
                self.counters.frames_regressed += 1;
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "the pending view moved backwards: {} after {} — recorded and the frame still held, because a source that rewinds is evidence about the source, not about the chain",
                        number.0, last.0
                    ),
                });
            } else if number.0 > last.0 {
                let skipped = number.0 - last.0 - 1;
                if skipped > 0 {
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "pending moved {} -> {}: {skipped} height(s) were never observed as pending — GAP_DETECTED by block number. Gaps *within* a height are NOT_MEASURABLE on this transport: no wire frame index exists, so a skipped height is indistinguishable from one that sealed faster than the read interval (§9)",
                            last.0, number.0
                        ),
                    });
                }
            }
        }

        // — §9's wrong-parent check, against what this radar knows the chain to be.
        match self.check_parent(&frame) {
            ParentOutcome::Conflicts { parent, expected } => {
                self.counters.frames_rejected += 1;
                self.counters.frames_wrong_parent += 1;
                if let Some(state) = self.views.remove(&number) {
                    self.counters.views_invalidated += 1;
                    events.push(RadarEvent::ViewInvalidated {
                        number: number.0,
                        reason: "wrong_parent",
                    });
                    let _ = state;
                }
                events.push(RadarEvent::FrameRejected {
                    number: number.0,
                    reason: "wrong_parent",
                });
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "frame for {} claims parent {parent:#x}, which does not continue the sealed {} ({expected:#x}); the held view is thrown away rather than patched (§24: a wrong identity invalidates, it does not merge)",
                        number.0,
                        number.0.saturating_sub(1)
                    ),
                });
                return events;
            }
            ParentOutcome::Unconfirmed => {
                self.counters.parent_unconfirmed += 1;
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "frame for {} names no parent hash: continuity is UNCONFIRMED, which is not the same claim as 'it continues' (§6)",
                        number.0
                    ),
                });
            }
            ParentOutcome::Continues => self.counters.parent_checks_passed += 1,
            ParentOutcome::NotCheckable => self.counters.parent_checks_uncheckable += 1,
        }

        let view_hash = frame.identity.view_hash.value().copied();
        let existing = self.views.remove(&number);
        let duplicate = view_hash.is_some_and(|hash| {
            existing
                .as_ref()
                .is_some_and(|state| state.view_hashes.contains(&hash))
        });

        let mut state = match existing {
            None => self.fresh_state(&frame),
            Some(state) if state.stage.is_closed() => {
                // §10: a closed stage cannot be re-opened, so this frame does not
                // extend that view — it starts a new one, and the replacement is
                // counted rather than passed off as continuity.
                self.counters.views_replaced_after_closure += 1;
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "height {} had already closed as {}; this frame starts a fresh view instead of extending it (§10)",
                        number.0,
                        state.stage.as_str()
                    ),
                });
                self.fresh_state(&frame)
            }
            Some(state) => state,
        };
        state.cycles_held = 0;

        if duplicate {
            self.counters.frames_duplicate += 1;
            state.frames_observed += 1;
            state.latest = frame.clone();
            state.bump_last_received(frame.observed_at_unix_ms);
            if state.timestamps.sequence_validated_at_unix_ms.is_none() {
                state.timestamps.sequence_validated_at_unix_ms = Some(stamp());
            }
            events.push(RadarEvent::FrameAccepted {
                number: number.0,
                local_frame_sequence: frame.identity.local_frame_sequence,
                transaction_count: frame.transaction_count,
                duplicate_view_hash: true,
            });
        } else {
            let read = FrameRead {
                frame: &frame,
                receipts: receipts.as_deref(),
                decoded_at_unix_ms,
                view_hash,
            };
            self.extend(&mut state, &read, &mut events, stamp);
        }

        self.views.insert(number, state);
        self.counters.frames_accepted += 1;
        self.last_accepted_number = Some(self.last_accepted_number.unwrap_or(number).max(number));
        events.extend(self.prune());
        events
    }

    /// Fold one non-duplicate frame into the held view: §26's stamps, §12's affected
    /// pools, §10's stage, and the counters that make the claims checkable.
    fn extend(
        &mut self,
        state: &mut PreconfirmationState,
        read: &FrameRead<'_>,
        events: &mut Vec<RadarEvent>,
        stamp: Stamp<'_>,
    ) {
        let frame = read.frame;
        let first_for_height = state.frames_observed == 0;
        let validated_at_unix_ms = stamp();
        state.frames_observed += 1;
        state.latest = frame.clone();
        if first_for_height {
            self.counters.heights_seen += 1;
            state.timestamps.chain_timestamp_secs = frame.chain_timestamp_secs.value().copied();
            state.timestamps.flashblock_received_at_unix_ms = Some(frame.observed_at_unix_ms);
            state.timestamps.flashblock_decoded_at_unix_ms = Some(read.decoded_at_unix_ms);
            state.timestamps.sequence_validated_at_unix_ms = Some(validated_at_unix_ms);
        }
        state.bump_last_received(frame.observed_at_unix_ms);
        if frame.state_root.unknown_reason().is_some() {
            self.counters.state_root_placeholders += 1;
        }
        if let Some(hash) = read.view_hash {
            if !state.view_hashes.contains(&hash) {
                state.distinct_view_hashes += 1;
                if state.view_hashes.len() < self.config.max_views_per_height {
                    state.view_hashes.push(hash);
                }
                // Counted at the crossing, not per frame, so `heights_multi_view` is
                // a count of heights and `heights_ge_three_views` is the §48 budget.
                match state.distinct_view_hashes {
                    2 => self.counters.heights_multi_view += 1,
                    3 => self.counters.heights_ge_three_views += 1,
                    _ => {}
                }
            }
        }

        let held = self.held_transactions(frame);
        if frame.transactions.len() > self.config.max_transactions_per_frame {
            self.counters.frames_truncated += 1;
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "frame for {} holds {} transactions, over the bound of {}; {} entry(ies) past it are counted here and excluded from the affected scan (§51: announced, not cut in silence)",
                    frame.number().0,
                    frame.transactions.len(),
                    self.config.max_transactions_per_frame,
                    frame.transactions.len() - self.config.max_transactions_per_frame
                ),
            });
        }
        self.counters.transactions_observed += held.len() as u64;
        state.transaction_hashes = held;

        let mut affected: Vec<AffectedPool> = Vec::new();
        self.scan_transactions(frame, &mut affected);
        if let Some(receipts) = read.receipts {
            self.scan_receipts(receipts, frame, &mut affected, events);
        }
        affected.sort();
        affected.dedup();
        let known: BTreeSet<_> = state
            .affected_pools
            .iter()
            .map(AffectedPool::identity_key)
            .collect();
        for pool in &affected {
            if known.contains(&pool.identity_key()) {
                continue;
            }
            if state.timestamps.affected_pool_detected_at_unix_ms.is_none() {
                state.timestamps.affected_pool_detected_at_unix_ms = Some(stamp());
            }
            self.counters.affected_pools_emitted += 1;
            match pool.reason {
                AffectedReason::TransactionTarget => self.counters.affected_pools_by_target += 1,
                AffectedReason::SuccessfulLogEmitter => self.counters.affected_pools_by_log += 1,
            }
            events.push(RadarEvent::PoolAffected(pool.clone()));
        }
        state.affected_pools = affected;

        if let Some(change) = Self::apply_stage(state, PreconfStage::Streaming) {
            events.push(change);
        }
        events.push(RadarEvent::FrameAccepted {
            number: frame.number().0,
            local_frame_sequence: frame.identity.local_frame_sequence,
            transaction_count: frame.transaction_count,
            duplicate_view_hash: false,
        });
    }

    /// The transaction hashes this frame contributes, cut by the memory bound.
    fn held_transactions(&self, frame: &PreconfirmationFrame) -> Vec<B256> {
        frame
            .transactions
            .iter()
            .take(self.config.max_transactions_per_frame)
            .map(|tx| tx.hash)
            .collect()
    }

    /// §12/§13: a transaction sent *to* a registered pool. A call, not an effect —
    /// the reason string says so, and nothing here reads it as one.
    fn scan_transactions(&self, frame: &PreconfirmationFrame, out: &mut Vec<AffectedPool>) {
        for tx in frame
            .transactions
            .iter()
            .take(self.config.max_transactions_per_frame)
        {
            let Some(to) = tx.to.value().copied() else {
                continue;
            };
            if self.pools.contains(&to) {
                out.push(AffectedPool {
                    chain_id: self.chain_id,
                    block_number: frame.number().0,
                    pool: to,
                    reason: AffectedReason::TransactionTarget,
                    transaction_hash: tx.hash,
                    local_frame_sequence: frame.identity.local_frame_sequence,
                    transaction_index: tx.index.clone(),
                    // A call target has no log and no topic. Recording that as
                    // `Unknown { key_present: false }` is §13's "don't invent fields
                    // to complete the list" applied to the output side too.
                    log_index: Field::unknown(false, NO_LOG_DETAIL),
                    topic0: Field::unknown(false, NO_TOPIC_DETAIL),
                });
            }
        }
    }

    /// §14 + NC10: only a *successful* receipt's logs may name a pool, and only logs
    /// the provider did not mark `removed`.
    fn scan_receipts(
        &mut self,
        receipts: &[PreconfReceipt],
        frame: &PreconfirmationFrame,
        out: &mut Vec<AffectedPool>,
        events: &mut Vec<RadarEvent>,
    ) {
        for receipt in receipts {
            // §9's wrong-block check, applied to the receipts describing this frame.
            if let Some(claimed) = receipt.claimed_block_number.value().copied() {
                if claimed != frame.number() {
                    self.counters.receipts_wrong_block += 1;
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "receipt for {:?} claims block {}, the frame being extended is {} — the receipt contributes nothing",
                            receipt.transaction_hash,
                            claimed.0,
                            frame.number().0
                        ),
                    });
                    continue;
                }
            }
            if receipt.status.value().copied() != Some(true) {
                // Reverted, or the payload did not say: neither is a confirmed effect.
                self.counters.reverted_receipts_skipped += 1;
                continue;
            }
            for log in &receipt.logs {
                if log.removed {
                    self.counters.receipts_removed_logs += 1;
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "log at index {} of transaction {:?} is marked removed; excluded from the affected set",
                            log.log_index.state_label(),
                            log.transaction_hash
                        ),
                    });
                    continue;
                }
                if self.pools.contains(&log.emitter) {
                    out.push(AffectedPool {
                        chain_id: self.chain_id,
                        block_number: frame.number().0,
                        pool: log.emitter,
                        reason: AffectedReason::SuccessfulLogEmitter,
                        transaction_hash: log.transaction_hash,
                        local_frame_sequence: frame.identity.local_frame_sequence,
                        transaction_index: receipt.index.clone(),
                        log_index: log.log_index.clone(),
                        topic0: log.topic0.clone(),
                    });
                }
            }
        }
    }

    /// Bound what is held. A closed view older than the retention window leaves; so
    /// does a sealed digest. If the map is still over budget, the *oldest* open view
    /// is invalidated, and that is announced rather than a `remove()` nobody sees.
    fn prune(&mut self) -> Vec<RadarEvent> {
        let mut events = Vec::new();
        if let Some(head) = self.highest_canonical {
            let floor = head.0.saturating_sub(self.config.closure_retention);
            let closed: Vec<BlockNumber> = self
                .views
                .iter()
                .filter(|(number, state)| state.stage.is_closed() && number.0 < floor)
                .map(|(number, _)| *number)
                .collect();
            for number in closed {
                self.views.remove(&number);
            }
            let stale_sealed: Vec<BlockNumber> = self
                .sealed
                .range(..BlockNumber(floor))
                .map(|(number, _)| *number)
                .collect();
            for number in stale_sealed {
                self.sealed.remove(&number);
            }
        }
        while self.views.len() > self.config.max_heights_held {
            let Some(oldest) = self.views.keys().next().copied() else {
                break;
            };
            let Some(state) = self.views.remove(&oldest) else {
                break;
            };
            self.counters.views_invalidated += 1;
            events.push(RadarEvent::ViewInvalidated {
                number: oldest.0,
                reason: "hold_bound_exceeded",
            });
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "height {} dropped after {} frame(s) to bound the held set; its {} affected pool(s) went with it and are not recoverable from the radar",
                    oldest.0,
                    state.frames_observed,
                    state.affected_pools.len()
                ),
            });
        }
        events
    }

    /// §30's expiry, driven by the radar's own cycles rather than by a payload field
    /// that does not exist. A view that keeps being re-read resets its budget; one
    /// the head has moved past ages out.
    pub fn tick(&mut self) -> Vec<RadarEvent> {
        let mut events = Vec::new();
        for state in self.views.values_mut() {
            if !state.stage.is_closed() {
                state.cycles_held += 1;
            }
        }
        let stale: Vec<BlockNumber> = self
            .views
            .iter()
            .filter(|(_, state)| {
                !state.stage.is_closed() && state.cycles_held > self.config.expiry_cycles
            })
            .map(|(number, _)| *number)
            .collect();
        for number in stale {
            let Some(mut state) = self.views.remove(&number) else {
                continue;
            };
            self.counters.views_expired += 1;
            if let Some(change) = Self::apply_stage(&mut state, PreconfStage::Expired) {
                events.push(change);
            }
            events.push(RadarEvent::ViewExpired {
                number: number.0,
                frames_observed: state.frames_observed,
            });
            // Kept so a later frame at the same number has something to be compared
            // against rather than silently reopening a view (§19's rule, §10's edge).
            self.views.insert(number, state);
        }
        events
    }

    /// §11 + §25: the transport dropped. Every open view is invalidated, because this
    /// endpoint has no cursor and no frame index to resume against — the escape the
    /// task book allows ("unless replay/cursor is proven") is not merely unimplemented
    /// here, it is unprovable on the measured protocol.
    pub fn on_disconnect(&mut self, reason: &str) -> Vec<RadarEvent> {
        let mut events = Vec::new();
        let open: Vec<BlockNumber> = self
            .views
            .iter()
            .filter(|(_, state)| !state.stage.is_closed())
            .map(|(number, _)| *number)
            .collect();
        for number in open {
            let Some(mut state) = self.views.remove(&number) else {
                continue;
            };
            self.counters.views_invalidated += 1;
            if let Some(change) = Self::apply_stage(&mut state, PreconfStage::Invalidated) {
                events.push(change);
            }
            events.push(RadarEvent::ViewInvalidated {
                number: number.0,
                reason: "transport_reconnect",
            });
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "height {} held for {} frame(s) was discarded at reconnect ({reason}): no cursor exists on this transport, so a resumed read cannot be shown to continue the same view (§11)",
                    number.0, state.frames_observed
                ),
            });
        }
        // The local sequence continues (it is a session counter), but the number
        // continuity it was compared against is gone: after a reconnect the next read
        // starts a new observed sequence rather than pretending to extend the old one.
        self.last_accepted_number = None;
        events
    }

    /// §15–§18: the block sealed. Compare by content, close the view, hand the
    /// latencies back in the same event so the latency table and the counters are
    /// built from one set of numbers.
    pub fn note_canonical(
        &mut self,
        sealed: &CanonicalDigest,
        stamp: Stamp<'_>,
    ) -> Vec<RadarEvent> {
        let mut events = Vec::new();
        if sealed.chain_id != self.chain_id {
            events.push(RadarEvent::Sequence {
                detail: format!(
                    "canonical digest for chain {:?} refused by a radar on chain {:?} (§9's identity check, applied to the closing side)",
                    sealed.chain_id, self.chain_id
                ),
            });
            return events;
        }
        self.highest_canonical = Some(
            self.highest_canonical
                .unwrap_or(sealed.number)
                .max(sealed.number),
        );
        // The digest's own continuity, recorded rather than acted on: a reorg on the
        // canonical side is the pipeline's business (§37 of M5), and the radar has no
        // authority to resolve one. It is noted because the lead numbers on the other
        // side of this height would otherwise be read as if the chain were stable.
        let below = BlockNumber(sealed.number.0.saturating_sub(1));
        if let Some(parent_digest) = self.sealed.get(&below) {
            if parent_digest.hash != sealed.parent_hash {
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "canonical {} claims parent {:#x}, this session sealed {} as {:#x}: a canonical-side reorg. The radar changes nothing and stays authoritative-free (§15: canonical state is decided by the canonical path, not here)",
                        sealed.number.0,
                        sealed.parent_hash,
                        below.0,
                        parent_digest.hash
                    ),
                });
            }
        }
        self.sealed.insert(sealed.number, sealed.clone());

        let state = self.views.remove(&sealed.number);
        let mut timestamps = state
            .as_ref()
            .map(|state| state.timestamps)
            .unwrap_or_default();
        timestamps.chain_timestamp_secs = Some(sealed.chain_timestamp_secs);
        timestamps.canonical_seen_at_unix_ms = Some(sealed.observed_at_unix_ms);
        // Stamped before the derivation, not after: `reconciliation_latency_ms` is
        // the verdict's own cost, and filling the stamp after the computation would
        // leave it permanently `N/A` while looking like a measurement.
        timestamps.reconciled_at_unix_ms = Some(stamp());
        let latencies = PreconfLatencies::from_timestamps(&timestamps);

        self.counters.canonical_closures += 1;
        let (verdict, hash_matched, stage) = match &state {
            None => {
                self.counters.reconciled_no_view += 1;
                (ReconciliationVerdict::NoView, false, None)
            }
            Some(state) => {
                let hash_matched = state.view_hashes.contains(&sealed.hash);
                if hash_matched {
                    self.counters.reconciled_hash_match += 1;
                }
                let verdict =
                    compare_content(&state.transaction_hashes, &sealed.transaction_hashes);
                match verdict {
                    ReconciliationVerdict::ContentEqual => {
                        self.counters.reconciled_content_equal += 1
                    }
                    ReconciliationVerdict::ContentPrefix { .. } => {
                        self.counters.reconciled_content_prefix += 1
                    }
                    ReconciliationVerdict::ContentSuperset { .. } => {
                        self.counters.reconciled_content_superset += 1
                    }
                    ReconciliationVerdict::ContentDivergent { .. } => {
                        self.counters.reconciled_content_divergent += 1
                    }
                    ReconciliationVerdict::NoView
                    | ReconciliationVerdict::LateFrameDiscarded { .. } => {}
                }
                let stage = match verdict {
                    ReconciliationVerdict::ContentEqual
                    | ReconciliationVerdict::ContentPrefix { .. } => PreconfStage::Reconciled,
                    _ => PreconfStage::Superseded,
                };
                (verdict, hash_matched, Some(stage))
            }
        };

        // §16's affected-pool identity, checked against whatever the canonical side
        // proved. Three outcomes, and the third is not a pass.
        match (&state, sealed.affected_pools.as_ref()) {
            (Some(state), Some(canonical_pools)) => {
                let view_pools: BTreeSet<Address> =
                    state.affected_pools.iter().map(|pool| pool.pool).collect();
                let canonical_pools: BTreeSet<Address> = canonical_pools
                    .iter()
                    .copied()
                    .filter(|p| self.pools.contains(p))
                    .collect();
                if view_pools == canonical_pools {
                    self.counters.reconciled_pools_matched += 1;
                } else {
                    self.counters.reconciled_pools_diverged += 1;
                    events.push(RadarEvent::Sequence {
                        detail: format!(
                            "affected pools differ at {}: view {} vs canonical {} (by set, ignoring which transaction caused each). Canonical is the record; the view's extra pools are not carried forward (§17: no merge)",
                            sealed.number.0,
                            view_pools.len(),
                            canonical_pools.len()
                        ),
                    });
                }
            }
            (Some(_), None) => {
                self.counters.reconciled_pools_unverified += 1;
                events.push(RadarEvent::Sequence {
                    detail: format!(
                        "affected-pool identity at {} is UNVERIFIED: the canonical side supplied no target list, which is not the same finding as 'the pool sets agree' (§28: missing data is N/A, never an empty set read as zero)",
                        sealed.number.0
                    ),
                });
            }
            (None, _) => {}
        }

        events.push(RadarEvent::Reconciled {
            number: sealed.number.0,
            verdict,
            hash_matched,
            latencies,
        });
        events.push(RadarEvent::Sequence {
            detail: format!(
                "{} closed by canonical {:#x}; verdict {}; hash_matched {hash_matched}; canonical_wins={} (§17: the verdict classes differ only in what is recorded about the view, never in who it yields to)",
                sealed.number.0,
                sealed.hash,
                verdict.as_str(),
                verdict.canonical_wins()
            ),
        });

        if let Some(mut state) = state {
            // The closure stamps belong to the height, not to the event: §26 asks for
            // them per block number, so the record a later table read has to carry the
            // same `canonical_seen_at` / `reconciled_at` the verdict was computed from.
            state.timestamps = timestamps;
            if let Some(stage) = stage {
                if let Some(change) = Self::apply_stage(&mut state, stage) {
                    events.push(change);
                }
            }
            self.views.insert(sealed.number, state);
        }
        events.extend(self.prune());
        events
    }
}

/// One height, as the sequence table prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HeightRecord {
    pub number: u64,
    pub stage: PreconfStage,
    pub frames_observed: u64,
    pub distinct_view_hashes: u64,
    pub transactions_in_latest: u64,
    pub affected_pools: u64,
    pub state_root_known: bool,
    /// Always false on the endpoint measured in M9.4 (§2.3), recorded per height so
    /// the claim is checkable rather than asserted.
    pub wire_index_known: bool,
    pub timestamps: PreconfTimestamps,
    pub latencies: PreconfLatencies,
    pub endpoint_id: String,
}

/// §16's content comparison, in the only form the measurement supports: the held
/// view is compared against the sealed transaction list as a set, and the verdict
/// says which direction the difference runs.
fn compare_content(held: &[B256], sealed: &[B256]) -> ReconciliationVerdict {
    if held == sealed {
        return ReconciliationVerdict::ContentEqual;
    }
    let held_set: BTreeSet<B256> = held.iter().copied().collect();
    let sealed_set: BTreeSet<B256> = sealed.iter().copied().collect();
    let only_in_view = held_set.difference(&sealed_set).count() as u64;
    let only_in_canonical = sealed_set.difference(&held_set).count() as u64;
    if only_in_view == 0 {
        return ReconciliationVerdict::ContentPrefix {
            held: held.len() as u64,
            sealed: sealed.len() as u64,
        };
    }
    if only_in_canonical == 0 {
        return ReconciliationVerdict::ContentSuperset {
            held: held.len() as u64,
            sealed: sealed.len() as u64,
        };
    }
    ReconciliationVerdict::ContentDivergent {
        only_in_view,
        only_in_canonical,
    }
}
