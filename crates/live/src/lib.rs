//! M5: market data sources. One event type, three producers.
//!
//! A source owns a transport and nothing else. It emits [`MarketEvent`]s — ordered
//! canonical block announcements, candidates, gap and status reports — and never
//! touches pool state, the graph, or an opportunity: reading the chain and
//! deciding what the chain means are kept apart on purpose, so the same
//! [`evm_replay::ReplayEngine`] can serve a live run and a historical one without
//! either knowing it.
//!
//! M9.4 adds the early-radar lane (`preconf*` modules): the preconfirmation host's
//! pending view, read over the same polling transport, decoded into
//! [`PreconfirmationFrame`], folded per block number into [`PreconfirmationState`],
//! and closed against a canonical digest. Its whole claim is *earlier detection at the
//! same authority* — a flashblock is never canonical state, never an execution
//! authorization, and never an opportunity. That is enforced by the build graph, not by
//! discipline: `evm-live`'s production dependencies reach no canonical crate, so
//! `crates/live/src` cannot name a `StateStore` at all. The gate that keeps it that way,
//! and the two directions it scans, are in `tests/preconf_isolation.rs`.

pub mod error;
pub mod event;
pub mod flashblocks;
pub mod preconf;
pub mod preconf_decode;
pub mod preconf_loop;
pub mod preconf_provider;
pub mod preconf_radar;
pub mod source;
pub mod tracker;
pub mod websocket;

pub use error::{LiveError, LiveResult};
pub use event::{
    now_unix_ms, BlockAnnouncement, FlashblockCandidate, GapOutcome, MarketEvent, SourceKind,
    SourceStatus,
};
pub use flashblocks::{candidate_from_value, FlashblockConfig, FlashblockSource, FlashblockStats};
pub use preconf::{
    AffectedPool, AffectedReason, Field, PreconfError, PreconfIdentity, PreconfLatencies,
    PreconfLog, PreconfReceipt, PreconfStage, PreconfTimestamps, PreconfTransaction,
    PreconfirmationFrame, PreconfirmationState, RadarCounters, RadarEvent, ReconciliationVerdict,
};
pub use preconf_decode::{
    frame_from_pending_value, receipt_from_value, receipts_from_value, state_root_field,
};
pub use preconf_loop::{LinkConfig, LinkEndedBy, LinkRunReport, LinkStage, PreconfLink};
pub use preconf_provider::{FrameSource, PollingFrameSource, RecordedFrame, ReplayFrameSource};
pub use preconf_radar::{
    CanonicalDigest, EarlyRadar, HeightRecord, PoolSet, RadarConfig, RadarInput,
};
pub use source::{EndedBy, MarketDataSource, PollingSource, RunReport, SourceConfig};
pub use tracker::{BlockTracker, TrackerPolicy, TrackerStats};
pub use websocket::WebSocketSource;
