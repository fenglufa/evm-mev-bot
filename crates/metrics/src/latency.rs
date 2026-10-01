//! §40: a latency is a distribution, not a mean.
//!
//! Every series keeps its samples and reports count / min / p50 / p95 / p99 /
//! max. Nothing here averages: an average over a run that was quiet for 40
//! blocks and stuck for 2 describes neither.

use std::collections::BTreeMap;

use serde::Serialize;

/// One measured latency series, named after the pair of stages it spans.
#[derive(Clone, Debug, Default, Serialize)]
pub struct LatencySeries {
    name: String,
    samples: Vec<u64>,
}

impl LatencySeries {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            samples: Vec::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn record(&mut self, millis: u64) {
        self.samples.push(millis);
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    /// The §40 table. Empty series report `count: 0` and no percentiles rather
    /// than a row of zeros, because a zero latency was never measured.
    pub fn stats(&self) -> serde_json::Value {
        if self.samples.is_empty() {
            return serde_json::json!({
                "name": self.name,
                "count": 0,
                "unit": "ms",
            });
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        serde_json::json!({
            "name": self.name,
            "unit": "ms",
            "count": sorted.len(),
            "min_ms": sorted[0],
            "p50_ms": percentile(&sorted, 50),
            "p95_ms": percentile(&sorted, 95),
            "p99_ms": percentile(&sorted, 99),
            "max_ms": sorted[sorted.len() - 1],
        })
    }
}

/// Nearest-rank percentile over an already sorted slice.
fn percentile(sorted: &[u64], rank: u64) -> u64 {
    let index = (rank * sorted.len() as u64).div_ceil(100).saturating_sub(1);
    let last = sorted.len().saturating_sub(1) as u64;
    sorted[index.min(last) as usize]
}

/// The set of series one run produced, keyed by stage pair.
#[derive(Clone, Debug, Default)]
pub struct LatencyTable {
    series: BTreeMap<String, LatencySeries>,
}

impl LatencyTable {
    pub fn record(&mut self, name: &str, millis: u64) {
        self.series
            .entry(name.to_string())
            .or_insert_with(|| LatencySeries::new(name))
            .record(millis);
    }

    /// Measure `millis` from `started` on the clock the caller is already using.
    pub fn record_from(&mut self, name: &str, started_ms: u64, now_ms: u64) {
        self.record(name, now_ms.saturating_sub(started_ms));
    }

    pub fn count(&self, name: &str) -> usize {
        self.series.get(name).map(LatencySeries::count).unwrap_or(0)
    }

    pub fn stats(&self) -> serde_json::Value {
        let rows: Vec<serde_json::Value> = self
            .series
            .values()
            .map(|series| {
                let stats = series.stats();
                // A series with no samples is a stage that never completed, not
                // a stage that took zero time; the report has to be able to tell
                // the two apart.
                serde_json::json!({
                    "name": series.name(),
                    "measured": series.count() > 0,
                    "stats": stats,
                })
            })
            .collect();
        serde_json::Value::Array(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_report_the_distribution_not_an_average() {
        let mut table = LatencyTable::default();
        for sample in [10u64, 20, 30, 40, 1000] {
            table.record("block_to_state", sample);
        }
        let stats = table.stats();
        let row = &stats[0]["stats"];
        assert_eq!(row["count"], 5);
        assert_eq!(row["min_ms"], 10);
        assert_eq!(row["max_ms"], 1000);
        assert_eq!(row["p50_ms"], 30);
        assert_eq!(row["p95_ms"], 1000);
        assert_eq!(row["p99_ms"], 1000);
        assert!(
            stats[0]["stats"].get("mean_ms").is_none(),
            "an average is not in this table"
        );
    }

    #[test]
    fn a_series_that_never_fired_says_so_instead_of_reporting_zeros() {
        let mut table = LatencyTable::default();
        table.record("block_to_state", 5);
        let stats = table.stats();
        let empty = LatencySeries::new("block_to_risk").stats();
        assert_eq!(empty["count"], 0);
        assert!(empty.get("p50_ms").is_none());
        assert_eq!(stats.as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn one_sample_is_its_own_percentile() {
        // The boundary a short live session sits on: a hop measured once. A table
        // that reported a fabricated 0 for the percentiles here would make a slow
        // hop look fast exactly when there are too few samples to average it away.
        let mut table = LatencyTable::default();
        table.record("block_to_risk", 777);
        let stats = table.stats();
        let row = &stats[0]["stats"];
        assert_eq!(row["count"], 1);
        for key in ["p50_ms", "p95_ms", "p99_ms", "min_ms", "max_ms"] {
            assert_eq!(row[key], 777, "{key} invented a value");
        }
    }

    #[test]
    fn every_reported_percentile_is_a_sample_that_happened() {
        let mut table = LatencyTable::default();
        let samples = [1u64, 2, 2, 3, 900, 1200, 1201];
        for sample in samples {
            table.record("block_to_opportunity", sample);
        }
        let stats = table.stats();
        let row = &stats[0]["stats"];
        for key in ["p50_ms", "p95_ms", "p99_ms"] {
            let value = row[key].as_u64().expect("a percentile");
            assert!(
                samples.contains(&value),
                "{key} = {value}, which is not a latency this series measured — nearest \
                 rank must pick a sample, never interpolate one"
            );
        }
        // The tail is the whole point of a distribution: one 1200 ms hop in a
        // mostly-fast series must show up, which an average would smooth away.
        assert_eq!(row["p99_ms"], 1201);
        assert_eq!(row["max_ms"], 1201);
    }

    #[test]
    fn a_clock_that_went_backwards_records_zero_not_a_wrap() {
        // `record_from` subtracts two readings of one clock. If they ever arrive
        // out of order, the wrong answer is a 1.8e19 ms latency polluting the
        // percentiles; the right one is the floor, plus a sample that is clearly
        // not a real hop.
        let mut table = LatencyTable::default();
        table.record_from("block_to_state", 5_000, 4_999);
        let stats = table.stats();
        assert_eq!(stats[0]["stats"]["max_ms"], 0);
        assert_eq!(table.count("block_to_state"), 1);
    }
}
