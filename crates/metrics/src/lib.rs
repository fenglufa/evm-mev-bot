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

pub mod clock;
pub mod counters;
pub mod latency;
pub mod timing;

pub use clock::{chain_to_wall_lag_ms, unix_ms, Clock};
pub use counters::{Counters, Metrics};
pub use latency::{LatencySeries, LatencyTable};
pub use timing::PipelineTiming;
