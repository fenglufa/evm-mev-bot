//! M8.1 §25–§29: the baseline table, and the sample rule that guards it.
//!
//! A baseline is a distribution over runs, so this file's job is to collect the
//! nanosecond figures a set of traces produced and report what is *measurable*
//! about them. Two rules shape it:
//!
//! §28 — one percentile algorithm, project-wide. [`nearest_rank`] is the same
//! nearest-rank computation M5's [`crate::LatencySeries`] uses, so a p95 written
//! here and a p95 written in `metrics.json` pick the same sample from the same
//! list. Nothing interpolates: a percentile is always a latency that happened.
//!
//! §29 — a rank with too few samples behind it is not a measurement. The rule is
//! stated as a definition rather than as a magic number: a rank is reportable
//! only when the index nearest-rank selects is **strictly below the largest
//! index**, because otherwise the "percentile" is a reprint of `max` under a
//! different name. Six executions reporting `p99` would be reporting the slowest
//! of six and calling it the ninety-ninth percentile of the fleet; here that
//! becomes `null` plus the reason, which is what §29's example asks for.

use std::collections::BTreeMap;

use serde_json::json;

use crate::trace::{LatencyTrace, Stage, TraceSource, HOP_NAMES, TRACE_SCHEMA};

/// The ranks §25 and §48 ask a baseline to publish.
pub const RANKS: [u64; 4] = [50, 90, 95, 99];

/// §29: the reason a rank is not reported, spelled in the evidence file.
const INSUFFICIENT: &str = "insufficient_sample";

/// The name a series is filed under: one stage's own work.
pub fn stage_series(stage: Stage) -> String {
    format!("stage_duration:{}", stage.as_str())
}

/// The name a series is filed under: one of §13's five end-to-end latencies.
pub fn end_to_end_series(name: &str) -> String {
    format!("end_to_end:{name}")
}

/// Nearest-rank over an ascending sample list, `None` when the selected index is
/// the last one (§29: that would reprint `max`, not report a percentile).
///
/// The index is `ceil(rank · n / 100) − 1`, which is M5's formula kept verbatim —
/// so the moment a rank becomes reportable, the value it reports is the value M5
/// would already have reported for the same samples.
pub fn nearest_rank(sorted: &[u64], rank: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let len = sorted.len() as u64;
    let index = (rank * len).div_ceil(100).saturating_sub(1);
    let last = len - 1;
    if index >= last {
        return None;
    }
    Some(sorted[index as usize])
}

/// The smallest sample count at which a rank is reportable.
///
/// Solved from the rule above: `ceil(rank · n / 100) ≤ n − 1` rearranges to
/// `n · (100 − rank) ≥ 100`, so the minimum is `ceil(100 / (100 − rank))` — p50
/// needs 2 samples, p90 10, p95 20, p99 100. The test suite re-derives the same
/// numbers by running [`nearest_rank`] over growing lists, so this closed form and
/// the rule it summarizes cannot drift apart silently.
pub fn minimum_samples_for(rank: u64) -> usize {
    let complement = 100u64.saturating_sub(rank).max(1);
    100u64.div_ceil(complement) as usize
}

/// The §25 table for one series: count, min, max, and each rank either as a
/// measured nanosecond figure or as `null` with its reason.
///
/// `min` and `max` are always available from one sample onwards (§29 allows
/// exactly that much for a short run), and are printed as measurements rather
/// than as percentiles for the same reason they need no policy: they are the
/// ends of the list, not positions interpolated within it.
pub fn stats(samples: &[u64]) -> serde_json::Value {
    if samples.is_empty() {
        return json!({
            "unit": "ns",
            "samples": 0,
            "measured": false,
            "note": "no run in this baseline produced this figure",
        });
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mut row = serde_json::Map::new();
    row.insert("unit".to_string(), json!("ns"));
    row.insert("samples".to_string(), json!(sorted.len()));
    row.insert("measured".to_string(), json!(true));
    row.insert("min_ns".to_string(), json!(sorted[0]));
    for rank in RANKS {
        let key = format!("p{rank}_ns");
        match nearest_rank(&sorted, rank) {
            Some(value) => {
                row.insert(key, json!(value));
            }
            None => {
                row.insert(key, serde_json::Value::Null);
                row.insert(
                    format!("p{rank}_reason"),
                    json!({
                        "reason": INSUFFICIENT,
                        "minimum_samples": minimum_samples_for(rank),
                    }),
                );
            }
        }
    }
    row.insert("max_ns".to_string(), json!(sorted[sorted.len() - 1]));
    // A spread of one sample is worth saying out loud: `min == max` in a row that
    // also has two measured ranks reads like a tight distribution.
    if sorted.len() == 1 {
        row.insert(
            "note".to_string(),
            json!("a single sample: this row is one measurement, not a distribution"),
        );
    }
    serde_json::Value::Object(row)
}

/// One source's share of the baseline.
///
/// §45 makes the per-source split the whole structure of this type: a replay's
/// latency is a directory read from disk and a live run's is a node answering over
/// the network, so a p50 computed over both is a number about nothing. Sources are
/// bucketed on the way in, and there is no method here that adds two of them.
#[derive(Clone, Debug)]
pub struct LatencyBaseline {
    source: TraceSource,
    chain_id: Option<u64>,
    /// The traces that fed this bucket, counted rather than inferred from the
    /// longest series, so `sample_count` in §44's metadata means observations of
    /// a lifecycle rather than of a stage.
    traces: usize,
    trace_ids: Vec<String>,
    series: BTreeMap<String, Vec<u64>>,
}

impl LatencyBaseline {
    pub fn new(source: TraceSource) -> Self {
        Self {
            source,
            chain_id: None,
            traces: 0,
            trace_ids: Vec::new(),
            series: BTreeMap::new(),
        }
    }

    pub fn source(&self) -> TraceSource {
        self.source
    }

    pub fn traces(&self) -> usize {
        self.traces
    }

    /// Fold one trace's figures into this bucket.
    ///
    /// A `None` hop is not recorded, which is the mechanism by which an unrun
    /// stage keeps its N/A status all the way to the table: it is absent from the
    /// sample list, and an absent series prints as "no run produced this figure"
    /// rather than as a zero (§13, §29).
    pub fn record(&mut self, trace: &LatencyTrace) {
        self.traces += 1;
        self.chain_id = Some(trace.chain_id());
        self.trace_ids.push(trace.trace_id().to_string());
        for stage in Stage::ALL {
            if let Some(duration) = trace.span_ns(stage) {
                self.push(stage_series(stage), duration);
            }
        }
        for (name, value) in trace.hops() {
            if let Some(value) = value {
                self.push(name.to_string(), value);
            }
        }
        let end_to_end = trace.end_to_end();
        for (name, value) in [
            ("detection_latency", end_to_end.detection_ns),
            (
                "execution_preparation_latency",
                end_to_end.execution_preparation_ns,
            ),
            ("inclusion_latency", end_to_end.inclusion_ns),
            ("settlement_latency", end_to_end.settlement_ns),
            ("end_to_end_latency", end_to_end.end_to_end_ns),
        ] {
            if let Some(value) = value {
                self.push(end_to_end_series(name), value);
            }
        }
        // §10's two totals, kept as two series precisely because they differ.
        if let Some(total) = trace.wall_clock_total_ns() {
            self.push("total:wall_clock".to_string(), total);
        }
        if let Some(sum) = trace.stage_duration_sum_ns() {
            self.push("total:stage_duration_sum".to_string(), sum);
        }
    }

    fn push(&mut self, name: String, value_ns: u64) {
        self.series.entry(name).or_default().push(value_ns);
    }

    pub fn series(&self, name: &str) -> Option<&[u64]> {
        self.series.get(name).map(Vec::as_slice)
    }

    pub fn to_json(&self) -> serde_json::Value {
        let latencies: serde_json::Map<String, serde_json::Value> = self
            .series
            .iter()
            .map(|(name, samples)| (name.clone(), stats(samples)))
            .collect();
        // A hop §12 names that no trace in this bucket produced still gets a row,
        // so the table lists everything that was asked for and the reader can see
        // which of them this run did not reach.
        let mut rows = latencies;
        for name in HOP_NAMES {
            let key = (*name).to_string();
            if !rows.contains_key(&key) {
                rows.insert(key, stats(&[]));
            }
        }
        json!({
            "source": self.source.as_str(),
            "chain_id": self.chain_id,
            "sample_count": self.traces,
            "trace_ids": self.trace_ids,
            "latencies_ns": rows,
        })
    }
}

/// The whole baseline: every source's table, plus §44's metadata.
#[derive(Clone, Debug, Default)]
pub struct BaselineSet {
    by_source: BTreeMap<TraceSource, LatencyBaseline>,
    git_revision: String,
    execution_mode: String,
    generated_at_unix_ms: u64,
}

impl BaselineSet {
    pub fn new(
        git_revision: impl Into<String>,
        execution_mode: impl Into<String>,
        generated_at_unix_ms: u64,
    ) -> Self {
        Self {
            by_source: BTreeMap::new(),
            git_revision: git_revision.into(),
            execution_mode: execution_mode.into(),
            generated_at_unix_ms,
        }
    }

    /// Route a trace to its own source's bucket.
    pub fn record(&mut self, trace: &LatencyTrace) {
        self.by_source
            .entry(trace.source())
            .or_insert_with(|| LatencyBaseline::new(trace.source()))
            .record(trace);
    }

    pub fn baseline(&self, source: TraceSource) -> Option<&LatencyBaseline> {
        self.by_source.get(&source)
    }

    /// §44's `summary.json`. `generated_at` is here so a reader can tell when the
    /// file was written, and is used for nothing else: no duration in this
    /// document is computed from it, because it is a wall-clock reading and every
    /// figure in the tables is monotonic (§2.2, §44).
    pub fn to_json(&self) -> serde_json::Value {
        let sources: Vec<serde_json::Value> = self
            .by_source
            .values()
            .map(LatencyBaseline::to_json)
            .collect();
        json!({
            "schema_version": TRACE_SCHEMA,
            "git_revision": self.git_revision,
            "execution_mode": self.execution_mode,
            "generated_at_unix_ms": self.generated_at_unix_ms,
            "generated_at_used_for_durations": false,
            "unit": "ns",
            "percentile_algorithm": "nearest_rank",
            "percentile_rule": "a rank is reported only when its index is strictly below \
             the largest index; otherwise it would reprint max",
            "minimum_samples_for_rank": {
                "p50": minimum_samples_for(50),
                "p90": minimum_samples_for(90),
                "p95": minimum_samples_for(95),
                "p99": minimum_samples_for(99),
            },
            "sources_are_never_blended": true,
            // §27's `sample_count` at the top level, so a reader who opens the file and
            // reads one field knows how many lifecycles are in it. It is a count of
            // lines, not a blended statistic: every table below is per source, and the
            // line above says so.
            "sample_count": sources
                .iter()
                .map(|row| row["sample_count"].as_u64().unwrap_or(0))
                .sum::<u64>(),
            "sources": sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{Granularity, StageRecord};

    /// The §29 rule, first as the definition it is: a rank whose index lands on
    /// the largest sample is not reportable, whatever the count.
    #[test]
    fn a_rank_that_would_only_reprint_max_is_not_reported() {
        assert_eq!(nearest_rank(&[10, 20], 50), Some(10));
        assert_eq!(nearest_rank(&[10], 50), None, "one sample is max");
        assert_eq!(
            nearest_rank(&[10, 20, 30, 40, 50, 60, 70, 80, 90], 90),
            None
        );
        assert_eq!(
            nearest_rank(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100], 90),
            Some(90)
        );
        assert_eq!(nearest_rank(&[1; 99], 99), None);
        assert_eq!(nearest_rank(&[1; 100], 99), Some(1));
        // p95 needs the same rule, not a separate opinion about it.
        assert_eq!(nearest_rank(&[5; 19], 95), None);
        assert_eq!(nearest_rank(&[5; 20], 95), Some(5));
    }

    /// The threshold table the report publishes has to agree with the rule, so
    /// this test is the check that the two are the same statement.
    #[test]
    fn the_published_sample_thresholds_are_the_rule_evaluated() {
        assert_eq!(minimum_samples_for(50), 2);
        assert_eq!(minimum_samples_for(90), 10);
        assert_eq!(minimum_samples_for(95), 20);
        assert_eq!(minimum_samples_for(99), 100);
        for rank in RANKS {
            let needed = minimum_samples_for(rank);
            assert_eq!(
                nearest_rank(&vec![7u64; needed - 1], rank),
                None,
                "p{rank} is printed as measurable below its stated minimum"
            );
            assert_eq!(
                nearest_rank(&vec![7u64; needed], rank),
                Some(7),
                "p{rank} is withheld at its stated minimum"
            );
        }
    }

    #[test]
    fn a_six_sample_run_reports_its_ends_and_withholds_its_percentiles() {
        // §29's own example: six executions. `min`, `max` and the sample count are
        // facts; p90 and above are not.
        let mut baseline = LatencyBaseline::new(TraceSource::Live);
        for duration in [100u64, 200, 300, 400, 500, 60_000] {
            let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(1), Some("six"));
            trace
                .record(StageRecord::measured(Stage::Sign, 0, duration))
                .expect("a record");
            baseline.record(&trace);
        }
        let row = &baseline.to_json()["latencies_ns"]["stage_duration:sign"];
        assert_eq!(row["samples"], 6);
        assert_eq!(row["min_ns"], 100);
        assert_eq!(row["max_ns"], 60_000);
        assert_eq!(row["p50_ns"], 300);
        assert_eq!(row["p90_ns"], serde_json::Value::Null);
        assert_eq!(row["p90_reason"]["reason"], INSUFFICIENT);
        assert_eq!(row["p90_reason"]["minimum_samples"], 10);
        assert_eq!(row["p99_reason"]["minimum_samples"], 100);
    }

    /// §28's cross-check: where both tables report, they report the same sample.
    #[test]
    fn nearest_rank_agrees_with_the_m5_series_over_the_same_samples() {
        let samples = [3u64, 1, 4, 1, 5, 9, 2, 6, 5, 35, 8, 97, 93, 23, 84, 62];
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let mut series = crate::latency::LatencyTable::default();
        for sample in samples {
            // M5's series is in milliseconds; the algorithm is on ranks, so the
            // unit is irrelevant as long as both sides see the same ordering.
            series.record("hop", sample);
        }
        let m5 = series.stats()[0]["stats"].clone();
        for rank in [50, 95, 99] {
            let ours = nearest_rank(&sorted, rank);
            let m5_value = m5[format!("p{rank}_ms")].as_u64();
            match ours {
                Some(value) => assert_eq!(
                    Some(value),
                    m5_value,
                    "p{rank} differs from M5's table for the same samples"
                ),
                // Where M5 clamps to the largest sample, this file withholds the
                // rank instead — and M5's value there is exactly `max`, which is
                // the reprint §29 refuses to call a percentile.
                None => assert_eq!(m5_value, Some(sorted[samples.len() - 1])),
            }
        }
    }

    #[test]
    fn an_unreached_stage_has_no_row_rather_than_a_zero_row() {
        let mut baseline = LatencyBaseline::new(TraceSource::Live);
        let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(1), Some("one"));
        trace.begin(Stage::Observation, 0).expect("open");
        trace.complete(Stage::Observation, 1_000).expect("close");
        trace.skip(Stage::Build, "no execution lane").expect("skip");
        baseline.record(&trace);
        let json = baseline.to_json();
        assert_eq!(
            json["latencies_ns"]["stage_duration:observation"]["samples"],
            1
        );
        assert!(
            json["latencies_ns"].get("stage_duration:build").is_none(),
            "a skipped stage contributes no sample; §9's no-duration rule survives \
             the aggregation"
        );
        // The §12 name is still listed, with the reason it has no number.
        assert_eq!(json["latencies_ns"]["build_duration"]["samples"], 0);
        assert_eq!(json["latencies_ns"]["build_duration"]["measured"], false);
    }

    /// §45: the buckets are the structure, not a label on one merged table.
    #[test]
    fn sources_are_kept_apart_in_one_set() {
        let mut set = BaselineSet::new("revision", "none", 1_790_000_000_000);
        for (source, duration) in [
            (TraceSource::Live, 5_000u64),
            (TraceSource::Replay, 500),
            (TraceSource::Fixture, 50),
        ] {
            let mut trace = LatencyTrace::new(source, 91_342, Some(1), Some("per-source"));
            trace
                .record(StageRecord::measured(Stage::Simulation, 0, duration))
                .expect("a record");
            set.record(&trace);
        }
        assert_eq!(set.baseline(TraceSource::Live).expect("live").traces(), 1);
        let json = set.to_json();
        let rows = json["sources"].as_array().expect("an array");
        assert_eq!(rows.len(), 3, "three sources, three tables: {json}");
        let names: Vec<&str> = rows
            .iter()
            .map(|row| row["source"].as_str().expect("a source"))
            .collect();
        // The order is the declaration order of `TraceSource`, which is what makes
        // two runs of this program write the same file rather than a permuted one.
        assert_eq!(names, ["live", "replay", "fixture"]);
        for row in rows {
            let samples = row["latencies_ns"]["stage_duration:simulation"]["samples"].clone();
            assert_eq!(samples, 1, "{:?}", row["source"]);
        }
        assert_eq!(json["minimum_samples_for_rank"]["p99"], 100);
        assert_eq!(json["sources_are_never_blended"], true);
        assert_eq!(json["generated_at_used_for_durations"], false);
    }

    /// §33, as a guard on the writer: a fixture's figure has to be findable *as a
    /// fixture's* figure in the machine-readable output.
    #[test]
    fn a_fixture_bucket_is_labelled_as_one() {
        let mut set = BaselineSet::new("revision", "none", 0);
        let mut trace = LatencyTrace::new(TraceSource::Fixture, 91_342, Some(10), Some("fixture"));
        trace
            .record(StageRecord::duration_only(
                Stage::Simulation,
                12,
                Granularity::Millisecond,
                "measured on the run's clock",
            ))
            .expect("a record");
        set.record(&trace);
        let json = set.to_json();
        assert_eq!(json["sources"][0]["source"], "fixture");
        assert_eq!(json["sources"][0]["chain_id"], 91_342);
        assert_eq!(json["sources"][0]["sample_count"], 1);
        assert_eq!(
            json["sources"][0]["latencies_ns"]["stage_duration:simulation"]["samples"], 1,
            "one trace, one sample"
        );
    }

    #[test]
    fn an_empty_series_says_it_was_never_measured() {
        let row = stats(&[]);
        assert_eq!(row["samples"], 0);
        assert_eq!(row["measured"], false);
        assert!(row.get("min_ns").is_none(), "no sample, so no minimum");
        assert!(row.get("p50_ns").is_none());
    }

    #[test]
    fn the_baseline_holds_every_hop_name_even_when_only_one_was_measured() {
        let mut baseline = LatencyBaseline::new(TraceSource::Replay);
        let mut trace = LatencyTrace::new(TraceSource::Replay, 1, Some(1), Some("hop"));
        trace.begin(Stage::Simulation, 0).expect("open");
        trace.complete(Stage::Simulation, 10).expect("close");
        baseline.record(&trace);
        let json = baseline.to_json();
        for name in HOP_NAMES {
            assert!(
                json["latencies_ns"].get(*name).is_some(),
                "{name} is missing from the table"
            );
        }
        let measured = &json["latencies_ns"]["simulation_duration"];
        assert_eq!(measured["samples"], 1);
        assert_eq!(measured["min_ns"], 10, "the one hop this trace measured");
        assert_eq!(measured["max_ns"], 10);
        assert_eq!(
            measured["p50_ns"],
            serde_json::Value::Null,
            "and even it has no percentile on a single sample (§29)"
        );
        assert_eq!(json["latencies_ns"]["sign_duration"]["samples"], 0);
    }
}
