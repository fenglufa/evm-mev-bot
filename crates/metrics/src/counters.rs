//! §51: everything that could have been dropped silently is a number instead.
//!
//! A counter is the cheapest form of "this happened, and it was not swallowed".
//! Keys are plain strings chosen at the call site, because an enum would have to
//! be extended for every new fact and the temptation when it is not extended is
//! the silence this file exists to prevent.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::latency::LatencyTable;

/// Named, monotonic counts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counters(BTreeMap<String, u64>);

impl Counters {
    pub fn add(&mut self, name: &str, amount: u64) {
        *self.0.entry(name.to_string()).or_insert(0) += amount;
    }

    pub fn bump(&mut self, name: &str) {
        self.add(name, 1);
    }

    pub fn get(&self, name: &str) -> u64 {
        self.0.get(name).copied().unwrap_or(0)
    }

    /// Every key ever bumped, including the ones sitting at zero. A run that
    /// recorded "gap_detected: 0" said something different from a run that never
    /// looked.
    pub fn entries(&self) -> &BTreeMap<String, u64> {
        &self.0
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self.0.clone()).unwrap_or_else(|_| serde_json::json!({}))
    }
}

/// The one record a run produces: what happened, and how long each hop took.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub counters: Counters,
    pub latency: LatencyTable,
}

impl Metrics {
    pub fn bump(&mut self, name: &str) {
        self.counters.bump(name);
    }

    pub fn add(&mut self, name: &str, amount: u64) {
        self.counters.add(name, amount);
    }

    pub fn get(&self, name: &str) -> u64 {
        self.counters.get(name)
    }

    pub fn record_latency(&mut self, name: &str, millis: u64) {
        self.latency.record(name, millis);
    }

    /// A duration between two stamps taken on the same [`crate::Clock`].
    ///
    /// Saturation rather than a subtraction that can underflow: two stamps from
    /// one monotonic clock cannot legitimately arrive out of order, and if a
    /// caller ever hands them over swapped, a zero-length sample is the honest
    /// reading of that mistake.
    pub fn record_from(&mut self, name: &str, from_ms: u64, to_ms: u64) {
        self.record_latency(name, to_ms.saturating_sub(from_ms));
    }

    /// `chain timestamp → this process`, in milliseconds, as its own series:
    /// this is the only latency here that is measured across two clocks, so it
    /// is named for that fact (§39).
    pub fn record_chain_lag(&mut self, name: &str, chain_timestamp_secs: u64, wall_unix_ms: u64) {
        let lag = crate::clock::chain_to_wall_lag_ms(chain_timestamp_secs, wall_unix_ms);
        self.record_latency(name, lag.max(0) as u64);
        if lag < 0 {
            // The chain clock is ahead of ours. Not clamped away silently: an
            // endpoint whose timestamps run ahead is a fact about the endpoint.
            self.bump("chain_clock_ahead_of_wall");
        }
    }

    /// §50: a queue that filled up is reported the moment it happens, and the
    /// report is a counter plus a latency-free fact — never a dropped event.
    pub fn backpressure(&mut self, queue: &str) {
        self.bump(&format!("backpressure_detected.{queue}"));
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "counters": self.counters.to_json(),
            "latency": self.latency.stats(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_are_readable_at_zero() {
        let mut metrics = Metrics::default();
        metrics.add("blocks", 3);
        metrics.bump("blocks");
        assert_eq!(metrics.get("blocks"), 4);
        assert_eq!(metrics.get("never_bumped"), 0);
        metrics.backpressure("event_queue");
        assert_eq!(metrics.get("backpressure_detected.event_queue"), 1);
    }

    #[test]
    fn a_chain_clock_running_ahead_is_counted_not_clamped() {
        let mut metrics = Metrics::default();
        // block timestamp in the future relative to our wall clock
        metrics.record_chain_lag("chain_to_received", 2_000_000_000, 1_000_000_000_000);
        assert_eq!(metrics.get("chain_clock_ahead_of_wall"), 1);
        assert_eq!(metrics.latency.count("chain_to_received"), 1);
    }
}
