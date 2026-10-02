//! M8.1 §30 over M7's **real** records: the baseline is built from the files M7 left
//! behind, and every figure in it is checked against the field it was read from.
//!
//! Nothing here runs a pipeline, opens an endpoint, or constructs a market. The inputs
//! are three committed `route-run.json` files under `data/evidence/m7/` — one run that
//! settled and had its profit verified across six real transactions, one that stopped at
//! §26's gate, and one that stopped after building — and the assertions read the same
//! fields back out of those files. That is the point of §30: M8.1's first real baseline
//! should be the latencies the project already paid for, and the test that it does is a
//! comparison against the source, not against a number copied into this file.
//!
//! Two rules do most of the work here:
//!
//! - **A span with no instants stays a span with no instants.** M7's stamps count
//!   milliseconds from its own process, whose zero was never recorded, so the loader
//!   writes `duration_ns` and leaves both ends `null` — and the trace's own
//!   `wall_clock_total` is `null` because a lifecycle this process never ran has no
//!   finish line to stamp (§46).
//! - **The absence of a figure is reported as an absence.** A stage M7 did not time is a
//!   `skipped` row naming the field that would have carried it, never a `0`, and the
//!   percentile tables then carry `null` with `insufficient_sample` at two samples (§29)
//!   rather than a p99 of a population of two.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::{history_baseline, M7Run};

const CHAIN: u64 = 91_342;
const MS: u64 = 1_000_000;

/// The run that went all the way: six transactions on chain, settled, profit verified.
const SETTLED: &str = "data/evidence/m7/route-submit/route-91342-37563264-1790908382146";
/// The run §26's gate refused.
const GATE_REFUSED: &str = "data/evidence/m7/route-submit/route-91342-37563031-1790908149030";
/// The run that stopped at `build-only`.
const BUILD_ONLY: &str = "data/evidence/m7/route-build-only/route-91342-37562423-1790907541327";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_dir(name: &str) -> PathBuf {
    workspace_root().join(name)
}

/// M7's own record, read straight from disk by the test, so every expected figure below
/// is a field in the evidence rather than a number remembered here.
fn source(name: &str) -> Value {
    let path = run_dir(name).join("route-run.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn lines(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: a line is not JSON: {error}", path.display()))
        })
        .collect()
}

fn whole(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn stages(trace: &Value) -> Vec<&Value> {
    trace["stages"]
        .as_array()
        .expect("every trace carries its stages")
        .iter()
        .collect()
}

fn stage_row(trace: &Value, name: &str) -> Value {
    stages(trace)
        .into_iter()
        .find(|row| row["stage"] == name)
        .unwrap_or_else(|| panic!("no {name} row in a trace"))
        .clone()
}

fn duration_of(trace: &Value, name: &str) -> Option<u64> {
    stage_row(trace, name)["duration_ns"].as_u64()
}

fn outcome_of(trace: &Value, name: &str) -> String {
    stage_row(trace, name)["outcome"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A fresh output directory under `target/`, emptied if a previous run of this test left
/// one behind: §44's no-overwrite rule is what several of these tests assert, so they
/// cannot share a path.
fn fresh(name: &str) -> PathBuf {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// §30's baseline over the settled run, figure for figure.
///
/// The three cross-checks at the bottom are the interesting part: M7 wrote both the
/// ladder's stamps *and* named figures for sign, submission and inclusion, computed by
/// `meter` from the same stamps. The loader recomputes the spans from the stamps. Two
/// independent readings of one run agreeing is evidence the pairing is right; a loader
/// that mixed the rungs up would show up here as a mismatch, not as a plausible number.
#[test]
fn m7s_own_records_become_a_baseline_of_the_spans_it_wrote_down() {
    let out = fresh("history-settled");
    let baseline = history_baseline(&[run_dir(SETTLED)], &out).expect("one run is a baseline");
    assert_eq!(baseline.samples, 1);
    assert_eq!(baseline.mode, "submit");

    let record = source(SETTLED);
    let latency = &record["latency_ms"];
    let stamps = &latency["rung_stamps"];
    let traces = lines(&out.join("traces.jsonl"));
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];

    // §44's metadata, on the line itself: which chain, which block, which opportunity,
    // and which run the numbers were read out of.
    assert_eq!(
        trace["source"], "replay",
        "§45: history is not a live sample"
    );
    assert_eq!(trace["chain_id"], CHAIN);
    assert_eq!(trace["opportunity_block"], record["pinned_block"]);
    assert_eq!(
        trace["opportunity_id"].as_str(),
        record["execution"]["opportunity_id"].as_str(),
        "§14: the trace reuses the identity M7 gave the finding"
    );
    assert_eq!(trace["session_id"].as_str(), record["session_id"].as_str());
    assert!(
        trace["trace_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("lat-"),
        "§14: the id is a prefixed hash, not a bare hex digest"
    );
    assert_eq!(
        stages(trace).len(),
        14,
        "§4's fourteen stages, every row present"
    );

    // The three discovery stages this file holds no figure for, and the risk decision M7
    // did reach but never timed: absent, with the reason stated. Not zero.
    for name in ["observation", "state_update", "graph_update"] {
        assert_eq!(outcome_of(trace, name), "skipped", "{name}");
        assert_eq!(stage_row(trace, name)["duration_ns"], Value::Null, "{name}");
        assert!(
            stage_row(trace, name)["note"]
                .as_str()
                .unwrap()
                .contains("handed its pair on the command line"),
            "{name} must say why it has no figure"
        );
    }
    assert_eq!(outcome_of(trace, "risk"), "skipped");
    assert_eq!(outcome_of(trace, "receipt"), "skipped");

    // The figures M7 wrote directly, in nanoseconds at millisecond granularity.
    for (name, key) in [
        ("opportunity_detection", "opportunity_detection_latency_ms"),
        ("simulation", "simulation_latency_ms"),
        ("preflight", "preflight_latency_ms"),
    ] {
        let expected = latency[key].as_u64().expect("M7 wrote this figure");
        assert_eq!(outcome_of(trace, name), "completed", "{name}");
        assert_eq!(
            duration_of(trace, name),
            Some(expected * MS),
            "{name} is {key} as it stands in the file"
        );
        assert_eq!(stage_row(trace, name)["granularity"], "millisecond");
        assert_eq!(stage_row(trace, name)["started_ns"], Value::Null, "{name}");
        assert!(
            stage_row(trace, name)["note"]
                .as_str()
                .unwrap()
                .contains(key),
            "{name} must name the field it read"
        );
    }

    // §42.3, on the one M7 figure whose name covers more than §12's stage: its clock starts
    // before the head read, so the number is §13's milestone. The row says that out loud —
    // a milestone filed under a stage name is exactly the claim this file must not make.
    let detection = stage_row(trace, "opportunity_detection")["note"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        detection.contains("observation→opportunity milestone"),
        "the detection row must say its figure is a milestone: {detection}"
    );
    assert!(
        detection.contains("rather than §12's detection-stage span"),
        "and say which name it is not: {detection}"
    );

    // The execution rungs, from the difference of the two stamps M7 paired for each —
    // the same pairing `meter` uses.
    let pairs = [
        ("build", "created", "built"),
        ("sign", "built", "signed"),
        ("submit", "signed", "submitted"),
        ("inclusion", "submitted", "included"),
        ("settlement", "included", "settled"),
        ("profit_verification", "settled", "profit_verified"),
    ];
    for (name, from, to) in pairs {
        let expected = stamps[to].as_u64().expect("this run reached both rungs")
            - stamps[from].as_u64().expect("and this one");
        assert_eq!(
            duration_of(trace, name),
            Some(expected * MS),
            "{name} is `{to}` minus `{from}`"
        );
        assert!(
            stage_row(trace, name)["note"]
                .as_str()
                .unwrap()
                .contains(&format!("`{from}` and `{to}`")),
            "{name} must name the two stamps it subtracted"
        );
    }

    // §46's independent readings: M7's own named figures for three of these spans.
    for (figure, name) in [
        ("sign_latency_ms", "sign"),
        ("submission_latency_ms", "submit"),
        ("inclusion_latency_ms", "inclusion"),
    ] {
        assert_eq!(
            duration_of(trace, name),
            latency[figure].as_u64().map(|ms| ms * MS),
            "the span read off the stamps must equal {figure}, which M7 computed from the \
             same stamps"
        );
    }

    // The settlement rung and its verdict rung are climbed back to back, so the span
    // between them is the clock's resolution and the row says so.
    assert_eq!(duration_of(trace, "profit_verification"), Some(0));
    assert!(
        stage_row(trace, "profit_verification")["note"]
            .as_str()
            .unwrap()
            .contains("resolution of M7's clock"),
        "a zero from two equal stamps must arrive with the reason it is a zero"
    );

    // One record, six transactions: the submission and inclusion rows cannot be read as
    // covering all of them.
    for name in ["submit", "inclusion"] {
        assert!(
            stage_row(trace, name)["note"]
                .as_str()
                .unwrap()
                .contains("not all 6 steps"),
            "{name} must say which of the six it measures"
        );
    }
}

/// §10's two totals, applied to a baseline this process did not time: the sum of the
/// stages is real and the wall clock is not.
#[test]
fn a_history_trace_holds_spans_and_claims_no_instants_of_its_own() {
    let out = fresh("history-instants");
    history_baseline(&[run_dir(SETTLED)], &out).expect("one run is a baseline");
    let written = lines(&out.join("traces.jsonl"));
    let trace = &written[0];
    let record = source(SETTLED);
    let latency = &record["latency_ms"];
    let stamps = &latency["rung_stamps"];

    // Every row: a duration, and `null` at both ends. No instant of M7's process clock
    // is presented as a place in time (§46), and no `started_ns` is filled in with the
    // loader's own clock.
    for row in stages(trace) {
        assert_eq!(row["started_ns"], Value::Null, "{}", row["stage"]);
        assert_eq!(row["ended_ns"], Value::Null, "{}", row["stage"]);
        if let Some(ms) = row["duration_ns"].as_u64() {
            assert!(ms % MS == 0, "{} is whole milliseconds", row["stage"]);
        }
    }
    assert!(
        stages(trace)
            .into_iter()
            .filter_map(|row| row["duration_ns"].as_u64())
            .all(|ms| ms < u64::MAX / MS),
        "no duration is negative: every row holds a difference M7 could have written"
    );

    assert_eq!(trace["started_ns"], Value::Null);
    assert_eq!(
        trace["completed_ns"],
        Value::Null,
        "the loading process's clock is not this lifecycle's end"
    );
    assert_eq!(trace["totals_ns"]["wall_clock_total"], Value::Null);
    let expected_sum: u64 = [
        latency["opportunity_detection_latency_ms"].as_u64(),
        latency["simulation_latency_ms"].as_u64(),
        latency["preflight_latency_ms"].as_u64(),
        stamps["built"]
            .as_u64()
            .map(|at| at - stamps["created"].as_u64().unwrap()),
        stamps["signed"]
            .as_u64()
            .map(|at| at - stamps["built"].as_u64().unwrap()),
        stamps["submitted"]
            .as_u64()
            .map(|at| at - stamps["signed"].as_u64().unwrap()),
        stamps["included"]
            .as_u64()
            .map(|at| at - stamps["submitted"].as_u64().unwrap()),
        stamps["settled"]
            .as_u64()
            .map(|at| at - stamps["included"].as_u64().unwrap()),
        stamps["profit_verified"]
            .as_u64()
            .map(|at| at - stamps["settled"].as_u64().unwrap()),
    ]
    .into_iter()
    .flatten()
    .sum();
    assert_eq!(
        trace["totals_ns"]["stage_duration_sum"],
        json!(expected_sum * MS)
    );

    // §12's named hops between stages need two instants, so they are absent — while a
    // stage's own duration is a sample either way.
    assert_eq!(trace["hops_ns"]["observation_to_opportunity"], Value::Null);
    assert_eq!(
        trace["hops_ns"]["simulation_duration"],
        json!(latency["simulation_latency_ms"].as_u64().unwrap() * MS)
    );
}

/// §45 applied one level down: §45 separates sources, and the same reasoning says a run
/// that paid for six inclusions and a run that stopped at the gate are not one
/// population. The refusal must happen before anything is written.
#[test]
fn two_modes_are_not_one_baseline_and_the_refusal_writes_nothing() {
    let out = fresh("history-mixed-modes");
    let error = history_baseline(&[run_dir(SETTLED), run_dir(BUILD_ONLY)], &out)
        .expect_err("a build-only run and a submitted run are not one sample population");
    let text = error.to_string();
    assert!(
        text.contains("build-only") && text.contains("submit"),
        "{text}"
    );
    assert!(
        !out.exists(),
        "a refused baseline leaves no half-written directory behind: {}",
        out.display()
    );

    // The two submit runs *are* one population, and both are read.
    let out = fresh("history-two-submit");
    let baseline = history_baseline(&[run_dir(SETTLED), run_dir(GATE_REFUSED)], &out)
        .expect("two runs of one mode are one baseline");
    assert_eq!(baseline.samples, 2);
    assert_eq!(baseline.sessions.len(), 2);
    assert_eq!(lines(&out.join("traces.jsonl")).len(), 2);
}

/// §44's 「不覆盖历史运行」, enforced on the loader itself: the second history into the
/// same directory is refused, and the first one's file is left exactly as it was.
#[test]
fn a_second_history_never_appends_to_one_already_on_disk() {
    let out = fresh("history-append");
    history_baseline(&[run_dir(SETTLED)], &out).expect("the first baseline is written");
    let before = std::fs::read(out.join("traces.jsonl")).expect("the file is there");

    let error = history_baseline(&[run_dir(GATE_REFUSED)], &out)
        .expect_err("a second history into one directory is refused");
    assert!(error.to_string().contains("already holds"), "{error}");
    let after = std::fs::read(out.join("traces.jsonl")).expect("the file is still there");
    assert_eq!(before, after, "the refusal changed nothing");
    assert_eq!(lines(&out.join("traces.jsonl")).len(), 1);
}

/// A run that stopped at the gate still has detection, simulation and gate timings — and
/// every rung below the stop is a `skipped` row that says where the run stopped. §34's
/// T3/T4 over M7's evidence instead of a fixture's.
#[test]
fn a_stopped_run_keeps_its_spans_and_names_where_its_ladder_halted() {
    let out = fresh("history-gate-refused");
    history_baseline(&[run_dir(GATE_REFUSED)], &out).expect("one run is a baseline");
    let written = lines(&out.join("traces.jsonl"));
    let trace = &written[0];
    let record = source(GATE_REFUSED);
    let latency = &record["latency_ms"];

    assert_eq!(outcome_of(trace, "preflight"), "completed");
    assert_eq!(
        duration_of(trace, "preflight"),
        Some(latency["preflight_latency_ms"].as_u64().unwrap() * MS)
    );
    // `created` exists and `built` does not, so there is no span and no invented one.
    assert_eq!(latency["rung_stamps"]["built"], Value::Null);
    let status = record["execution"]["status"].as_str().unwrap_or_default();
    for name in [
        "build",
        "sign",
        "submit",
        "inclusion",
        "settlement",
        "profit_verification",
    ] {
        assert_eq!(outcome_of(trace, name), "skipped", "{name}");
        assert_eq!(stage_row(trace, name)["duration_ns"], Value::Null, "{name}");
        assert!(
            stage_row(trace, name)["note"]
                .as_str()
                .unwrap()
                .contains(&format!("status `{status}`")),
            "{name} must say where the run stopped: {:?}",
            stage_row(trace, name)["note"]
        );
    }
}

/// §29 on real figures: two samples give a p50 and no p99, and the file says which of
/// the two it is refusing to report.
#[test]
fn two_real_samples_are_a_baseline_that_admits_it_is_small() {
    let out = fresh("history-small-sample");
    history_baseline(&[run_dir(SETTLED), run_dir(GATE_REFUSED)], &out)
        .expect("two runs of one mode are one baseline");
    let summary = whole(&out.join("summary.json"));

    assert_eq!(summary["sample_count"], 2);
    assert_eq!(summary["sources"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        summary["execution_mode"], "submit",
        "§44: one mode per baseline, and the file says which"
    );
    assert_eq!(summary["sources_are_never_blended"], json!(true));
    assert_eq!(summary["generated_at_used_for_durations"], json!(false));
    assert_eq!(summary["percentile_algorithm"], "nearest_rank");
    let bucket = &summary["sources"][0];
    assert_eq!(bucket["source"], "replay");
    assert_eq!(bucket["sample_count"], 2);
    assert_eq!(bucket["chain_id"], CHAIN);

    // M7 wrote this figure for both runs, so the row's ends are checkable against
    // the two files rather than against a number remembered here.
    let settled = source(SETTLED)["latency_ms"]["simulation_latency_ms"]
        .as_u64()
        .unwrap();
    let refused = source(GATE_REFUSED)["latency_ms"]["simulation_latency_ms"]
        .as_u64()
        .unwrap();
    let simulation = &bucket["latencies_ns"]["stage_duration:simulation"];
    assert_eq!(simulation["samples"], 2);
    assert_eq!(simulation["min_ns"], json!(settled.min(refused) * MS));
    assert_eq!(simulation["max_ns"], json!(settled.max(refused) * MS));
    assert_eq!(
        simulation["p99_ns"],
        Value::Null,
        "two samples cannot carry a p99"
    );
    assert_eq!(
        simulation["p99_reason"]["reason"], "insufficient_sample",
        "§29: the refusal names the rule, and the minimum it comes from"
    );
    assert_eq!(simulation["p99_reason"]["minimum_samples"], 100);
    assert!(
        simulation["p50_ns"].as_u64().is_some(),
        "a median of two is a reported figure: {:?}",
        simulation["p50_ns"]
    );

    // The hops a history cannot state are present as absent, not missing: a reader
    // sees the question rather than an empty table.
    assert_eq!(
        bucket["latencies_ns"]["observation_to_opportunity"]["samples"],
        0
    );
    assert_eq!(
        bucket["latencies_ns"]["observation_to_opportunity"]["measured"],
        json!(false)
    );

    let readme = std::fs::read_to_string(out.join("README.md")).expect("§25's README");
    assert!(readme.contains("replay"), "{readme}");
}

/// The loader's own view of one run, without the file layer: §30's N/A must reach the
/// recorder as a skip, and a read must not be able to invent a stage.
#[test]
fn reading_one_run_shapes_all_fourteen_stages_from_the_file_alone() {
    let run = M7Run::read(&run_dir(BUILD_ONLY)).expect("M7's record parses");
    assert_eq!(run.mode, "build-only");
    assert_eq!(run.market.as_deref(), Some("REAL_MARKET"));
    let trace = run.trace();
    assert_eq!(trace.source(), evm_metrics::TraceSource::Replay);
    assert_eq!(trace.chain_id(), CHAIN);

    let recorded = run.recorded();
    let written = recorded
        .trace
        .as_ref()
        .expect("a history trace exists")
        .to_json();
    let rows = stages(&written);
    assert_eq!(rows.len(), 14, "§4's fourteen rows, always");
    let shaped = rows.iter().filter(|row| row["outcome"] != "absent").count();
    assert!(
        (8..=14).contains(&shaped),
        "every stage the file can speak to is a row, and every other stage is still a \
         skip rather than a hole: {shaped}"
    );
    assert_eq!(
        rows.iter().filter(|row| row["outcome"] == "absent").count(),
        0,
        "a history read writes all fourteen stages, so no row is `absent`"
    );
    assert!(
        recorded.refusals.is_empty(),
        "a history read is never refused: {:?}",
        recorded.refusals
    );
}
