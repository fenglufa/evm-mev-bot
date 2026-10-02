//! M5: latency, counters and evidence-shaped numbers.
//!
//! This crate measures; it decides nothing. It exists because §38 requires five
//! named latencies, §39 requires them measured against the right clock, and §40
//! forbids reporting a mean where a distribution was asked for — three rules
//! that are easy to violate by accident when each stage of a pipeline stamps its
//! own `SystemTime`.
//!
//! Two clocks, deliberately not merged:
//!
//! ```text
//! Clock            monotonic ms since this run began   -> in-process latency
//! chain timestamp    seconds on the chain's own clock  -> propagation, named as such
//! ```
//!
//! M8.1 added the two modules a latency *baseline* needs on top of M5's per-run
//! counters: [`trace`] holds one opportunity's lifecycle as an ordered set of
//! stage records, read only from stamps the run already produced, and
//! [`baseline`] folds a set of those traces into the per-source percentile tables
//! §25 asks for, withholding any rank too few samples can support (§29).
//!
//! Nothing in either module is consulted by a decision. They are the bypass §2.1
//! describes: a run that writes no trace behaves, and is judged, exactly as a run
//! that never had them.

pub mod baseline;
pub mod clock;
pub mod counters;
pub mod latency;
pub mod timing;
pub mod trace;

pub use baseline::{
    end_to_end_series, minimum_samples_for, nearest_rank, stage_series, stats, BaselineSet,
    LatencyBaseline, RANKS,
};
pub use clock::{chain_to_wall_lag_ms, unix_ms, Clock};
pub use counters::{Counters, Metrics};
pub use latency::{LatencySeries, LatencyTable};
pub use timing::PipelineTiming;
pub use trace::{
    CostSplit, Domain, EndToEnd, Granularity, Half, LatencyTrace, Stage, StageOutcome, StageRecord,
    TraceError, TraceSource, HOP_NAMES, TRACE_SCHEMA,
};
