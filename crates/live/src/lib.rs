//! M5: market data sources. One event type, three producers.
//!
//! A source owns a transport and nothing else. It emits [`MarketEvent`]s — ordered
//! canonical block announcements, candidates, gap and status reports — and never
//! touches pool state, the graph, or an opportunity: reading the chain and
//! deciding what the chain means are kept apart on purpose, so the same
//! [`evm_replay::ReplayEngine`] can serve a live run and a historical one without
//! either knowing it.

pub mod error;
pub mod event;
pub mod flashblocks;
pub mod source;
pub mod tracker;
pub mod websocket;

pub use error::{LiveError, LiveResult};
pub use event::{
    now_unix_ms, BlockAnnouncement, FlashblockCandidate, GapOutcome, MarketEvent, SourceKind,
    SourceStatus,
};
pub use flashblocks::{candidate_from_value, FlashblockConfig, FlashblockSource, FlashblockStats};
pub use source::{EndedBy, MarketDataSource, PollingSource, RunReport, SourceConfig};
pub use tracker::{BlockTracker, TrackerPolicy, TrackerStats};
pub use websocket::WebSocketSource;
