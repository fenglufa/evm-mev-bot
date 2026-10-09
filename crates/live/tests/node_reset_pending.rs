//! §5's rule for the pending views, run through the production accounting: a candidate
//! observed against an old head is invalid, and it is invalidated by a rule the code
//! enforces rather than by a hope that it ages out.
//!
//! The task book's words are 「旧 pending 视图在 reset 后必须按明确规则失效」. A node restart
//! or a head rollback reaches this source as one of three observable facts, and each has a
//! named consequence here:
//!
//! - the head moved past a held number → [`FlashblockSource::tick`] expires it through
//!   `number.0 < highest_canonical`, which is a *head* rule and not the age rule;
//! - `pending` reports a number behind the last one it reported → [`FlashblockSource::observe`]
//!   emits [`MarketEvent::Unknown`] and never opens a record for it;
//! - the session ended and a new one began → the new source holds no records, so a sealed
//!   block resolves nothing and `canonical_covered` stays at zero rather than inheriting the
//!   previous session's window.
//!
//! # What is not claimed here
//!
//! Nothing in this file is evidence about a real node. There is no transport behind these
//! sources — the reader is a double whose every read errors, and the tests assert
//! `reads == 0` so that is visible rather than implied. §5 keeps the real restart experiment
//! as a follow-up infrastructure item, and `LOCAL_FLASHBLOCKS = NOT_VERIFIED` still holds.
//!
//! The invalidation is safe to apply because of §27's ceiling, which the last test in this
//! file re-checks on these exact event sequences: whatever this source decides about a
//! candidate, no path through it emits a canonical block. A view that is dropped early can
//! never have been the view a state update was taken from.

use alloy_primitives::B256;
use async_trait::async_trait;
use evm_chain::{ChainBlock, ChainError, HeadReader, Result as ChainResult};
use evm_core::ChainId;
use evm_live::{
    BlockAnnouncement, FlashblockCandidate, FlashblockConfig, FlashblockSource, MarketDataSource,
    MarketEvent, SourceKind,
};
use serde_json::json;

const CHAIN: ChainId = ChainId(91_342);

/// A candidate the source would have seen from a `pending` read, built directly so the
/// sequences below state the number and hash they test instead of hiding them in a parse.
fn candidate(number: u64, seed: u8, txs: u64, gas: u64) -> FlashblockCandidate {
    FlashblockCandidate {
        chain_id: CHAIN,
        number: evm_core::BlockNumber(number),
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
        number: evm_core::BlockNumber(number),
        hash: B256::repeat_byte(seed),
        parent_hash: B256::repeat_byte(seed.wrapping_sub(1)),
        chain_timestamp_secs: 1_700_000_000 + number,
        transaction_count: txs,
        observed_at_unix_ms: 0,
        source: SourceKind::HttpPoll,
    }
}

/// A config in which the *age* rules can never fire: no candidate is ever old enough or
/// numerous enough to be pushed out. Anything that expires under this config was expired by
/// the head, which is the only way to test the head rule as its own rule.
fn never_ages() -> FlashblockConfig {
    FlashblockConfig {
        expiry_cycles: u32::MAX,
        max_numbers_held: usize::MAX,
        ..FlashblockConfig::default()
    }
}

fn source() -> FlashblockSource<UnusedReader> {
    FlashblockSource::new(
        UnusedReader,
        CHAIN,
        "http://candidate.test".to_string(),
        never_ages(),
    )
}

/// The transport is here to satisfy the type. Every read failing is the point: a test that
/// asked a node would not be proving the invalidation rule, it would be proving a network.
struct UnusedReader;

#[async_trait]
impl HeadReader for UnusedReader {
    fn transport(&self) -> &'static str {
        "test"
    }
    async fn head(&mut self) -> ChainResult<evm_core::BlockNumber> {
        Err(ChainError::MissingData("unused".to_string()))
    }
    async fn block_at(
        &mut self,
        _number: evm_core::BlockNumber,
    ) -> ChainResult<Option<ChainBlock>> {
        Ok(None)
    }
}

/// The head moved on while a number was held — including the shape a restart produces, where
/// the new session's first `pending` read is *behind* the head the same session was just
/// told about. The number dies in the cycle that opened it, on the head's authority alone.
#[test]
fn a_pending_number_behind_the_head_dies_in_the_cycle_that_opened_it() {
    let mut src = source();
    src.note_canonical(canonical(13, 13, 4));

    let opened = src.observe(candidate(11, 11, 1, 1_000));
    assert!(
        matches!(opened.as_slice(), [MarketEvent::Candidate(_)]),
        "the first read of a new session is not judged as a rewind: {opened:?}"
    );
    // Control for the assertion below: with the age rules switched off, nothing here can
    // expire for being old.
    assert_eq!(src.stats().expired, 0);

    let events = src.tick();
    assert!(
        matches!(
            events.as_slice(),
            [MarketEvent::CandidateExpired { number: 11, .. }]
        ),
        "the head, not the clock, retired this view: {events:?}"
    );
    assert_eq!(src.stats().expired, 1);
    assert_eq!(src.stats().reads, 0, "no node was consulted");

    // And it stays dead: a second cycle has nothing left to hold.
    assert!(src.tick().is_empty(), "the view came back");
}

/// The same three numbers against a source that was never told about a head are all held.
/// Without this, the test above would pass for any source that expires everything.
#[test]
fn the_same_views_survive_when_no_head_has_passed_them() {
    let mut src = source();
    for number in 10..13u64 {
        src.observe(candidate(number, number as u8, number, number * 1_000));
    }
    assert!(src.tick().is_empty(), "nothing was expired without a cause");
    assert_eq!(src.stats().expired, 0);
    assert_eq!(src.stats().numbers_seen, 3);
}

/// `pending` comes back behind where it was: the view is recorded as an anomaly and dropped,
/// not re-emitted as a fresh partial state to be simulated again.
#[test]
fn a_rewound_pending_view_is_recorded_and_never_reopened() {
    let mut src = source();
    src.observe(candidate(12, 12, 1, 1_000));
    src.observe(candidate(13, 13, 2, 2_000));
    src.note_canonical(canonical(14, 14, 3));

    let before = src.stats();
    let events = src.observe(candidate(12, 0xcc, 9, 9_000));
    assert!(
        matches!(events.as_slice(), [MarketEvent::Unknown { .. }]),
        "a rewind produces a statement, not a candidate: {events:?}"
    );
    let after = src.stats();
    assert_eq!(
        after.sequence_anomalies,
        before.sequence_anomalies + 1,
        "and the anomaly is counted"
    );
    assert_eq!(
        after.distinct_candidates, before.distinct_candidates,
        "the rewound hash became no new partial state"
    );
    assert_eq!(
        after.highest_pending, 13,
        "the high-water mark does not move backwards"
    );
    assert!(src.tick().is_empty(), "and nothing was left held behind");
}

/// A restart is a new session: it cannot close the previous session's candidate window, so
/// it reports that it never opened one instead of carrying the old answer forward.
#[test]
fn a_new_session_resolves_no_window_it_never_observed() {
    let mut old = source();
    old.observe(candidate(20, 20, 3, 3_000));
    let resolved_before = old.stats().resolved;

    // The session ends. What follows is the process's state after a reconnect, not a
    // continuation of what came before.
    let mut fresh = source();
    let events = fresh.note_canonical(canonical(20, 20, 3));
    assert!(
        events.is_empty(),
        "a resolution needs a record this session made: {events:?}"
    );
    let stats = fresh.stats();
    assert_eq!(stats.canonical_seen, 1, "the block was still noticed");
    assert_eq!(stats.resolved, 0, "and matched against nothing");
    assert_eq!(
        stats.canonical_covered, 0,
        "no candidate window was closed by inheritance"
    );
    assert_eq!(
        stats.highest_canonical, 20,
        "the head it did learn is remembered"
    );
    assert_eq!(resolved_before, 0);

    // The capability record says the same thing in the report's own words: a session that
    // was shown a head and resolved no window of its own is a FAIL for the mapping, not a
    // inherited PASS.
    let rows = fresh.capability();
    assert_eq!(
        rows["newheads_reconciliation"]["status"],
        serde_json::json!("FAIL"),
        "{rows}"
    );
    assert_eq!(
        rows["newheads_reconciliation"]["canonical_covered"],
        json!(0)
    );

    // …and a session that was never shown one at all is NOT_OBSERVED rather than a pass.
    let blind = source();
    assert_eq!(
        blind.capability()["newheads_reconciliation"]["status"],
        json!("NOT_OBSERVED"),
        "an unshown mapping is reported as unobserved, never assumed"
    );
}

/// §27's ceiling, applied to every sequence above: invalidating a view early is only safe
/// because this source has no path to a canonical event. If one ever appeared, a dropped
/// pending view could also be a dropped state update.
#[test]
fn none_of_the_invalidations_reaches_a_canonical_event() {
    let mut src = source();
    let mut events = Vec::new();
    events.extend(src.note_canonical(canonical(9, 9, 1)));
    for number in 8..14u64 {
        events.extend(src.observe(candidate(number, number as u8, number, number * 1_000)));
        events.extend(src.tick());
    }
    events.extend(src.observe(candidate(3, 3, 1, 1)));
    events.extend(src.tick());

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, MarketEvent::Canonical(_))),
        "a candidate source emitted state: {events:?}"
    );
    assert!(!SourceKind::Flashblock.is_canonical());
    assert_eq!(src.stats().reads, 0);
}
