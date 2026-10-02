//! §39's two clocks, kept apart on purpose.
//!
//! In-process latency is measured against one monotonic origin (`Clock`), so a
//! number here can never be moved by the system clock being adjusted mid-run.
//! Chain-facing latency is measured against `chain_timestamp_secs`, and that one
//! *does* contain the provider's propagation delay — which is why it is reported
//! under its own name and never added to a monotonic figure.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Origin for every monotonic stamp in a run.
///
/// `Instant` itself is not passed around: it is opaque, cannot be serialized
/// into an evidence file, and comparing two of them across a process restart is
/// meaningless. A clock turns one into milliseconds since *this run began*, which
/// is both comparable and writable.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    origin: Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    /// Milliseconds since this run began.
    pub fn now_ms(&self) -> u64 {
        self.elapsed_ms(Instant::now())
    }

    /// Nanoseconds since this run began.
    ///
    /// M8.1 §8 asks for stage durations in nanoseconds, and a nanosecond reading
    /// of the *same* origin as the millisecond one keeps the two clocks in this
    /// file from ever becoming three: the finer unit is only a finer resolution
    /// on the one monotonic timeline, so an ms stamp and an ns stamp can be
    /// subtracted from each other without mixing in wall time.
    pub fn now_ns(&self) -> u64 {
        self.elapsed_ns(Instant::now())
    }

    pub const fn origin_instant(&self) -> Instant {
        self.origin
    }

    /// Elapsed milliseconds, measured against this clock rather than reported
    /// (so the number in the evidence is a measurement, not a subtraction the
    /// reader has to redo).
    pub fn since_ms(&self, earlier: u64) -> u64 {
        let now = self.now_ms();
        now.saturating_sub(earlier)
    }

    /// The nanosecond form of [`Clock::since_ms`]: elapsed, measured, floored at
    /// zero rather than wrapped (§8 forbids a negative duration, and a reading
    /// that arrives out of order would produce one).
    pub fn since_ns(&self, earlier: u64) -> u64 {
        let now = self.now_ns();
        now.saturating_sub(earlier)
    }

    fn elapsed_ms(&self, at: Instant) -> u64 {
        at.duration_since(self.origin).as_millis() as u64
    }

    fn elapsed_ns(&self, at: Instant) -> u64 {
        at.duration_since(self.origin).as_nanos() as u64
    }

    /// The monotonic stamp for `at`, so a timestamp taken outside this process's
    /// start still lands on this clock rather than on wall time.
    pub fn stamp(&self, at: Instant) -> u64 {
        self.elapsed_ms(at)
    }

    /// The nanosecond stamp for `at`.
    pub fn stamp_ns(&self, at: Instant) -> u64 {
        self.elapsed_ns(at)
    }
}

/// Wall-clock Unix milliseconds, for the evidence files' own timestamps.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

/// The gap between the chain's clock and this one, in milliseconds.
///
/// Positive means the block's own timestamp is behind our wall clock, which is
/// the normal direction: the block had to be produced, propagated, and read.
/// Reported as a measurement, never as a correction.
pub fn chain_to_wall_lag_ms(chain_timestamp_secs: u64, wall_unix_ms: u64) -> i64 {
    let chain_ms = i64::try_from(chain_timestamp_secs).unwrap_or(i64::MAX) * 1000;
    let wall = i64::try_from(wall_unix_ms).unwrap_or(i64::MAX);
    wall - chain_ms
}
