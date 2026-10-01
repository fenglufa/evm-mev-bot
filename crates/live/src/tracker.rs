//! The ordering kernel: dedup, chain-order emission, gap detection, gap
//! recovery, and the two rules §4 makes absolute — WebSocket arrival order is
//! not chain order, and one block number has one hash until a conflict says
//! otherwise.
//!
//! This is deliberately synchronous and free of transport, because every one of
//! those rules is a pure function of `(what we have, what was announced)`. The
//! sources below own the I/O and feed observations in; the tests in this file
//! cover the ordering without a network and without a timer, so §60's "no
//! dependence on async scheduling order" is satisfied by construction rather
//! than by a test that hopes the scheduler behaves.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, ChainId};

use crate::event::{BlockAnnouncement, GapOutcome, MarketEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerPolicy {
    /// How many blocks ahead of the hole the tracker will hold. This is the
    /// ordering half of §50's backpressure: the tracker refuses to run away from
    /// the blocks it has not yet delivered.
    pub max_pending_ahead: u64,
    /// How many times a missing number is re-probed before the gap is called
    /// unrecovered.
    pub max_gap_attempts: u32,
}

impl Default for TrackerPolicy {
    fn default() -> Self {
        Self {
            max_pending_ahead: 64,
            max_gap_attempts: 5,
        }
    }
}

/// Everything the tracker counted, for the counters §39 and the report ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TrackerStats {
    pub announced: u64,
    pub emitted: u64,
    pub duplicates: u64,
    pub conflicts: u64,
    pub stale: u64,
    pub gaps_detected: u64,
    pub gaps_recovered: u64,
    pub gaps_unrecovered: u64,
    pub blocks_skipped: u64,
    pub next_expected: u64,
    pub highest_announced: u64,
}

/// What the tracker knows about a number it has not yet delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    /// Announced by a head, not yet proven to exist by a body read.
    Announced(BlockAnnouncement),
    /// Read back as absent, `attempts` times so far.
    Absent { attempts: u32 },
}

/// Ordered emission of canonical blocks. See the module comment.
pub struct BlockTracker {
    chain_id: ChainId,
    policy: TrackerPolicy,
    next_expected: BlockNumber,
    /// The highest head any source has claimed, so "the head moved" is measured
    /// against what we were already told rather than against what we delivered.
    last_head: BlockNumber,
    known: BTreeMap<BlockNumber, Known>,
    /// Delivered blocks, kept for the length of `max_pending_ahead` so a late
    /// duplicate of an already-consumed block is recognised as a duplicate
    /// instead of looking like a reorg.
    delivered: BTreeMap<BlockNumber, alloy_primitives::B256>,
    /// The hole currently being reported, so one GapDetected is emitted per hole
    /// rather than one per poll of that hole.
    open_hole: Option<(u64, u64)>,
    stats: TrackerStats,
    /// A gap the provider could not fill. Set once; the run must not continue as
    /// if it were in sync (§8: an alignment failure is reported, not papered over).
    unrecovered: Option<(u64, u64)>,
}

impl BlockTracker {
    pub fn new(chain_id: ChainId, start_after: BlockNumber, policy: TrackerPolicy) -> Self {
        // start_after is the last block the state engine already holds; the
        // tracker's first emission must be the very next one.
        let next_expected = BlockNumber(start_after.0.saturating_add(1));
        Self {
            chain_id,
            policy,
            next_expected,
            last_head: start_after,
            known: BTreeMap::new(),
            delivered: BTreeMap::new(),
            open_hole: None,
            stats: TrackerStats {
                next_expected: next_expected.0,
                ..TrackerStats::default()
            },
            unrecovered: None,
        }
    }

    pub const fn policy(&self) -> TrackerPolicy {
        self.policy
    }

    pub const fn stats(&self) -> TrackerStats {
        let mut stats = self.stats;
        stats.next_expected = self.next_expected.0;
        stats
    }

    pub const fn next_expected(&self) -> BlockNumber {
        self.next_expected
    }

    /// A gap the provider would not close. Once this is set the run is over; the
    /// pipeline reports it instead of continuing with a state it cannot vouch for.
    pub const fn unrecovered_gap(&self) -> Option<(u64, u64)> {
        self.unrecovered
    }

    pub const fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// The numbers a source must read this cycle: everything owed up to `head`,
    /// ascending, capped by the policy window. A number already held as announced
    /// is not re-read (it is waiting for the hole behind it); a number held as
    /// read-but-absent is re-read, which is what §5's gap recovery means in code.
    pub fn owed(&self, head: BlockNumber, limit: usize) -> Vec<BlockNumber> {
        let mut owed = Vec::new();
        for number in self.next_expected.0..=head.0 {
            if matches!(
                self.known.get(&BlockNumber(number)),
                Some(Known::Announced(_))
            ) {
                continue;
            }
            owed.push(BlockNumber(number));
            if owed.len() >= limit.max(1) {
                break;
            }
        }
        owed
    }

    /// A head moved, and these are the numbers between what we owe and what the
    /// provider now claims exists. Registering them is what turns "the head is
    /// N" into "N is owed", so a block announced once and lost to a reconnect is
    /// still going to be asked for.
    pub fn note_head(&mut self, head: BlockNumber) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        if head.0 <= self.last_head.0 && !self.known.is_empty() {
            // A head answer that does not advance what we were already told is
            // not news; the numbers we owe are unchanged. A tracker that has not
            // registered anything yet still falls through, so the first head
            // after a restart is processed normally.
            return events;
        }
        let jumped_from = self.last_head.0.saturating_add(1);
        self.last_head = self.last_head.max(head);
        for number in self.next_expected.0..=head.0 {
            let number = BlockNumber(number);
            if self.known.contains_key(&number) || self.delivered.contains_key(&number) {
                continue;
            }
            self.known.insert(number, Known::Absent { attempts: 0 });
        }
        // The gap is the range the head claimed in one step, clipped to what is
        // still owed: a head that arrives one block at a time is a chain running
        // at its own cadence, not a hole in our ingestion. The clip is a no-op in
        // practice — `next_expected` can never pass `last_head + 1`, since no number
        // is read ahead of the head that offered it — but it is what lets a test
        // that reads slower than the chain (§57) assert that no hole was claimed, and
        // it keeps `GapDetected` naming the range that still has to be recovered.
        let from = jumped_from.max(self.next_expected.0);
        if head.0 > from {
            match self.open_hole {
                None => {
                    self.open_hole = Some((from, head.0));
                    self.stats.gaps_detected += 1;
                    events.push(MarketEvent::GapDetected { from, to: head.0 });
                }
                // The same hole, longer: still one hole, so one detection.
                Some((open_from, open_to)) => {
                    self.open_hole = Some((open_from, open_to.max(head.0)));
                }
            }
        }
        events
    }

    /// A block body read succeeded: this is the ordered path.
    pub fn observe(&mut self, announcement: BlockAnnouncement) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        self.stats.announced += 1;
        if announcement.chain_id != self.chain_id {
            events.push(MarketEvent::Unknown {
                detail: format!(
                    "block {} announced for chain {} by a {} source tracking chain {}",
                    announcement.number.0,
                    announcement.chain_id.0,
                    announcement.source.as_str(),
                    self.chain_id.0
                ),
            });
            return events;
        }
        if announcement.number < self.next_expected {
            // Already delivered, or below what we owe. Either way it cannot
            // advance state again.
            self.stats.stale += 1;
            let known = self.delivered.get(&announcement.number).copied();
            match known {
                Some(hash) if hash == announcement.hash => {
                    self.stats.duplicates += 1;
                    events.push(MarketEvent::Duplicate {
                        number: announcement.number.0,
                        hash: format!("{:?}", announcement.hash),
                    });
                }
                Some(hash) => {
                    self.stats.conflicts += 1;
                    events.push(MarketEvent::CanonicalityConflict {
                        number: announcement.number.0,
                        kept: format!("{hash:?}"),
                        rejected: format!("{:?}", announcement.hash),
                    });
                }
                None => events.push(MarketEvent::StaleAnnouncement {
                    number: announcement.number.0,
                    next_expected: self.next_expected.0,
                }),
            }
            return events;
        }
        match self.known.get(&announcement.number).copied() {
            Some(Known::Announced(previous)) => {
                if previous.hash == announcement.hash {
                    self.stats.duplicates += 1;
                    events.push(MarketEvent::Duplicate {
                        number: announcement.number.0,
                        hash: format!("{:?}", announcement.hash),
                    });
                    return events;
                }
                self.stats.conflicts += 1;
                events.push(MarketEvent::CanonicalityConflict {
                    number: announcement.number.0,
                    kept: format!("{:?}", previous.hash),
                    rejected: format!("{:?}", announcement.hash),
                });
                // The first-sealed hash stays. Replacing it would let a later
                // answer silently rewrite a block already handed downstream.
                return events;
            }
            Some(Known::Absent { .. }) | None => {
                self.known
                    .insert(announcement.number, Known::Announced(announcement));
            }
        }
        events.extend(self.drain());
        events
    }

    /// A block body read came back empty. Retried up to the policy budget, then
    /// the gap is called unrecovered.
    pub fn observe_absent(&mut self, number: BlockNumber) -> Vec<MarketEvent> {
        if number < self.next_expected {
            self.stats.stale += 1;
            return Vec::new();
        }
        let attempts = match self.known.get(&number) {
            Some(Known::Absent { attempts }) => {
                let attempts = attempts + 1;
                self.known.insert(number, Known::Absent { attempts });
                attempts
            }
            Some(Known::Announced(_)) => {
                // A head claimed it and a body read says no: keep the head's
                // hash, ask again.
                self.known.insert(number, Known::Absent { attempts: 1 });
                1
            }
            None => {
                self.known.insert(number, Known::Absent { attempts: 1 });
                1
            }
        };
        if attempts < self.policy.max_gap_attempts {
            return Vec::new();
        }
        let highest = self.known.keys().next_back().copied().unwrap_or(number);
        let range = (number.0, highest.0);
        self.unrecovered = Some(range);
        self.stats.gaps_unrecovered += 1;
        self.stats.blocks_skipped = highest.0 - number.0;
        vec![MarketEvent::Gap(GapOutcome::Unrecovered {
            from: range.0,
            to: range.1,
            missing: self
                .known
                .range(number..=highest)
                .filter(|(_, k)| matches!(k, Known::Absent { .. }))
                .count() as u64,
            attempts,
            detail: format!(
                "the provider was asked {attempts} times for block {} and never produced it; {} number(s) in {}..={} stayed undelivered",
                number.0,
                self.known
                    .range(number..=highest)
                    .filter(|(_, k)| matches!(k, Known::Absent { .. }))
                    .count(),
                number.0,
                highest.0
            ),
        })]
    }

    /// The hole closed: everything owed has been delivered.
    fn drain(&mut self) -> Vec<MarketEvent> {
        let mut events = Vec::new();
        while let Some(Known::Announced(announcement)) =
            self.known.get(&self.next_expected).copied()
        {
            let number = self.next_expected;
            self.known.remove(&number);
            self.delivered.insert(number, announcement.hash);
            // Trim the duplicate window so a long run does not grow it forever.
            while self.delivered.len() as u64 > self.policy.max_pending_ahead * 4 {
                self.delivered.pop_first();
            }
            self.next_expected = BlockNumber(number.0 + 1);
            self.stats.emitted += 1;
            self.stats.highest_announced = self.stats.highest_announced.max(number.0);
            events.push(MarketEvent::Canonical(announcement));
        }
        if let Some((from, to)) = self.open_hole {
            if self.next_expected.0 > to {
                events.push(MarketEvent::Gap(GapOutcome::Recovered {
                    from,
                    to,
                    blocks: to - from + 1,
                }));
                self.stats.gaps_recovered += 1;
                self.open_hole = None;
            }
        }
        events
    }

    /// After a reconnect: the provider's head is known again, and everything the
    /// tracker still owes stays owed. Nothing is forgotten and nothing is
    /// re-delivered, which is what §69 asks for.
    pub fn reconcile(&mut self, head: BlockNumber) -> Vec<MarketEvent> {
        // A reconnect changes nothing about what is owed: the head is re-noted and
        // everything already delivered stays delivered. That is §69 in one line —
        // no block re-emitted, no block forgotten.
        self.note_head(head)
    }

    /// Numbers held but not delivered, ascending.
    pub fn outstanding(&self) -> BTreeSet<BlockNumber> {
        self.known.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;

    use super::*;
    use crate::event::SourceKind;

    const CHAIN: ChainId = ChainId(1);

    fn block(number: u64, seed: u8, source: SourceKind) -> BlockAnnouncement {
        BlockAnnouncement {
            chain_id: CHAIN,
            number: BlockNumber(number),
            hash: B256::repeat_byte(seed),
            parent_hash: B256::repeat_byte(seed.wrapping_sub(1)),
            chain_timestamp_secs: 1_700_000_000 + number,
            transaction_count: 3,
            observed_at_unix_ms: 0,
            source,
        }
    }

    fn canonicals(events: &[MarketEvent]) -> Vec<u64> {
        events
            .iter()
            .filter_map(MarketEvent::canonical_block)
            .map(|n| n.0)
            .collect()
    }

    #[test]
    fn a_head_that_arrives_out_of_order_is_still_emitted_in_chain_order() {
        // §4: arrival order is not chain order. 12, 10, 11 in that order.
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let mut seen = Vec::new();
        for number in [12u64, 10, 11] {
            seen.extend(canonicals(&tracker.observe(block(
                number,
                number as u8,
                SourceKind::WebSocket,
            ))));
        }
        assert_eq!(
            seen,
            vec![10, 11, 12],
            "emission followed the chain, not the socket"
        );
        assert_eq!(tracker.next_expected(), BlockNumber(13));
    }

    #[test]
    fn the_same_block_twice_is_a_duplicate_and_never_advances_state() {
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let first = tracker.observe(block(10, 10, SourceKind::WebSocket));
        let second = tracker.observe(block(10, 10, SourceKind::WebSocket));
        assert_eq!(canonicals(&first), vec![10]);
        assert!(canonicals(&second).is_empty());
        assert!(
            matches!(
                second.as_slice(),
                [MarketEvent::Duplicate { number: 10, .. }]
            ),
            "{second:?}"
        );
        assert_eq!(tracker.stats().duplicates, 1);
        assert_eq!(tracker.stats().emitted, 1);
    }

    #[test]
    fn a_reconnect_redelivering_a_consumed_block_is_not_read_as_a_reorg() {
        // §60 plus §69 together: the reconnect replays 10 (same hash) and 11.
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        tracker.observe(block(10, 10, SourceKind::WebSocket));
        let events = tracker.observe(block(10, 10, SourceKind::WebSocket));
        assert!(matches!(events.as_slice(), [MarketEvent::Duplicate { .. }]));
        let events = tracker.observe(block(11, 11, SourceKind::WebSocket));
        assert_eq!(canonicals(&events), vec![11]);
        assert_eq!(tracker.stats().conflicts, 0);
    }

    #[test]
    fn one_number_two_hashes_is_recorded_and_the_first_hash_wins() {
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let first = tracker.observe(block(10, 10, SourceKind::WebSocket));
        assert_eq!(canonicals(&first), vec![10]);
        // 10 again, different hash, but 10 is already consumed: a conflict, not a
        // rewrite, and nothing re-emitted.
        let second = tracker.observe(block(10, 0xAA, SourceKind::WebSocket));
        assert!(canonicals(&second).is_empty());
        assert!(matches!(
            second.as_slice(),
            [MarketEvent::CanonicalityConflict { number: 10, .. }]
        ));
        assert_eq!(tracker.next_expected(), BlockNumber(11));
    }

    #[test]
    fn a_hole_in_the_head_is_detected_once_and_reported_as_recovered_when_it_closes() {
        // §5: the head jumps 10 → 13 with nothing for 11 and 12 yet.
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let events = tracker.note_head(BlockNumber(10));
        assert!(events.is_empty(), "no hole at all: {events:?}");
        let events = tracker.note_head(BlockNumber(13));
        assert_eq!(
            events,
            vec![MarketEvent::GapDetected { from: 11, to: 13 }],
            "the head moved past blocks we do not have"
        );
        // Asking again about the same hole must not spam a new detection.
        assert!(tracker.note_head(BlockNumber(14)).is_empty());
        // 10 arrives, so the hole starts at 11 and is still open.
        tracker.observe(block(10, 10, SourceKind::HttpPoll));
        let mut closing = Vec::new();
        for number in [11u64, 12, 13] {
            closing.extend(tracker.observe(block(number, number as u8, SourceKind::HttpPoll)));
        }
        assert!(
            closing.iter().any(|e| matches!(
                e,
                MarketEvent::Gap(GapOutcome::Recovered {
                    from: 11,
                    to: 13,
                    blocks: 3
                })
            )),
            "{closing:?}"
        );
        let last = tracker.observe(block(14, 14, SourceKind::HttpPoll));
        assert_eq!(canonicals(&last), vec![14]);
        assert_eq!(tracker.stats().gaps_detected, 1);
        assert_eq!(tracker.stats().gaps_recovered, 1);
    }

    #[test]
    fn a_head_that_advances_one_at_a_time_is_lag_and_not_a_gap() {
        // §57's "no misjudged gap" from the other side of §5: a hole is the head
        // passing a number it never offered, not the reader being behind. Here the
        // head advances exactly one number per poll while two numbers are owed — a
        // queue — and a tracker that called that a gap would report every continuous
        // live run as broken.
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        assert!(tracker.note_head(BlockNumber(10)).is_empty());
        // The reader is slower than the chain: 10 has not been read when the head
        // says 11, and 11 has not been read when the head says 12. Two numbers owed
        // at once is a queue.
        assert_eq!(
            tracker.owed(BlockNumber(11), 8),
            vec![BlockNumber(10), BlockNumber(11)]
        );
        assert!(
            tracker.note_head(BlockNumber(11)).is_empty(),
            "the head stepped one block at a time; nothing was jumped over"
        );
        assert_eq!(
            canonicals(&tracker.observe(block(10, 10, SourceKind::HttpPoll))),
            vec![10]
        );
        assert!(tracker.note_head(BlockNumber(12)).is_empty());
        assert_eq!(
            tracker.owed(BlockNumber(12), 8),
            vec![BlockNumber(11), BlockNumber(12)]
        );
        for number in [11u64, 12] {
            tracker.observe(block(number, number as u8, SourceKind::HttpPoll));
        }
        assert_eq!(tracker.next_expected(), BlockNumber(13));
        let stats = tracker.stats();
        assert_eq!(
            (
                stats.gaps_detected,
                stats.gaps_recovered,
                stats.blocks_skipped
            ),
            (0, 0, 0),
            "a lagging reader must not be reported as a broken chain"
        );
    }

    #[test]
    fn a_gap_the_provider_never_fills_ends_the_run_instead_of_looking_like_sync() {
        let policy = TrackerPolicy {
            max_pending_ahead: 64,
            max_gap_attempts: 3,
        };
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), policy);
        tracker.observe(block(10, 10, SourceKind::HttpPoll));
        tracker.note_head(BlockNumber(12));
        let mut events = Vec::new();
        for _ in 0..3 {
            events.extend(tracker.observe_absent(BlockNumber(11)));
        }
        let [MarketEvent::Gap(GapOutcome::Unrecovered {
            from,
            to,
            missing,
            attempts,
            ..
        })] = events.as_slice()
        else {
            panic!("expected one unrecovered-gap event, got {events:?}");
        };
        assert_eq!(
            (*from, *to, *missing, *attempts),
            (11, 12, 2, 3),
            "after the retry budget the gap is called what it is"
        );
        assert_eq!(
            *missing, 2,
            "11 was probed to exhaustion and 12 was never produced either — both are missing"
        );
        assert_eq!(tracker.unrecovered_gap(), Some((11, 12)));
        assert_eq!(tracker.next_expected(), BlockNumber(11));
        // Nothing further is emitted: the run stops rather than skipping.
        assert!(tracker
            .observe(block(12, 12, SourceKind::HttpPoll))
            .is_empty());
    }

    #[test]
    fn note_head_registers_every_number_owed_so_a_lost_announcement_is_still_asked_for() {
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        tracker.note_head(BlockNumber(12));
        assert_eq!(
            tracker.owed(BlockNumber(12), 64),
            vec![BlockNumber(10), BlockNumber(11), BlockNumber(12)],
            "the head is a promise about a range, not about its last block"
        );
        // 11 is delivered late; 10 and 12 are still what the source must read, and
        // 10 is now the hole, so a head at 12 does not make 10 disappear.
        tracker.observe(block(11, 11, SourceKind::HttpPoll));
        assert_eq!(
            tracker.owed(BlockNumber(12), 64),
            vec![BlockNumber(10), BlockNumber(12)],
            "an announced-but-undeliverable block is not re-read, a hole still is"
        );
    }

    #[test]
    fn a_reconcile_after_reconnect_owes_exactly_what_was_owed_before() {
        // §69: no duplicate state and no missed blocks across a disconnect.
        let mut before = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        before.observe(block(10, 10, SourceKind::WebSocket));
        before.note_head(BlockNumber(13));
        let mut after = BlockTracker::new(CHAIN, BlockNumber(10), TrackerPolicy::default());
        after.observe(block(10, 10, SourceKind::WebSocket));
        // The reconnect hands back the same head it had before the drop.
        let events = after.reconcile(BlockNumber(13));
        assert!(canonicals(&events).is_empty());
        assert_eq!(after.outstanding(), before.outstanding());
        assert_eq!(after.next_expected(), BlockNumber(11));
        assert_eq!(
            after.stats().emitted,
            0,
            "the reconnect restarted after block 10, so 10 is not counted twice"
        );
        assert_eq!(
            after.stats().stale,
            1,
            "and the refused re-delivery was recorded rather than swallowed (§51)"
        );
    }

    #[test]
    fn a_block_from_another_chain_is_reported_as_unknown_and_not_emitted() {
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let mut foreign = block(10, 10, SourceKind::Replay);
        foreign.chain_id = ChainId(999);
        let events = tracker.observe(foreign);
        assert!(canonicals(&events).is_empty());
        assert!(matches!(events.as_slice(), [MarketEvent::Unknown { .. }]));
        assert_eq!(tracker.next_expected(), BlockNumber(10));
    }

    #[test]
    fn emission_is_identical_whichever_source_announced_the_same_blocks() {
        // §32's parity claim, at the ordering layer only: replay and live must be
        // able to run the same numbers through the same tracker.
        let make = |source: SourceKind| {
            let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
            let mut out = Vec::new();
            for number in 10..14u64 {
                out.extend(tracker.observe(block(number, number as u8, source)));
            }
            (out, tracker.stats())
        };
        let (live, live_stats) = make(SourceKind::WebSocket);
        let (replay, replay_stats) = make(SourceKind::Replay);
        assert_eq!(
            live.iter().map(|e| e.canonical_block()).collect::<Vec<_>>(),
            replay
                .iter()
                .map(|e| e.canonical_block())
                .collect::<Vec<_>>(),
            "same numbers, same order, same source kind or not"
        );
        assert_eq!(
            live_stats.emitted, replay_stats.emitted,
            "{live_stats:?} {replay_stats:?}"
        );
        assert_eq!(
            live[0].canonical_block().zip(replay[0].canonical_block()),
            Some(BlockNumber(10)).zip(Some(BlockNumber(10)))
        );
    }

    #[test]
    fn arrival_order_between_two_sources_never_changes_what_is_emitted() {
        // §34 with a live socket and a replay feed interleaved: a candidate or a
        // late replay block cannot reorder the canonical stream.
        let mut tracker = BlockTracker::new(CHAIN, BlockNumber(9), TrackerPolicy::default());
        let mut emitted = Vec::new();
        emitted.extend(canonicals(&tracker.observe(block(
            12,
            12,
            SourceKind::WebSocket,
        ))));
        emitted.extend(canonicals(&tracker.observe(block(
            11,
            11,
            SourceKind::Replay,
        ))));
        emitted.extend(canonicals(&tracker.observe(block(
            13,
            13,
            SourceKind::HttpPoll,
        ))));
        emitted.extend(canonicals(&tracker.observe(block(
            10,
            10,
            SourceKind::Replay,
        ))));
        assert_eq!(emitted, vec![10, 11, 12, 13]);
    }
}
