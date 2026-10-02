//! M8.3.1 §19: the A/B evidence directory, assembled from the runs that produced it.
//!
//! §19's one hard rule is 「证据必须来自实际运行。不要手写数字。」 — so this file contains no
//! measured figure at all. It reads the six live route runs this milestone recorded (three
//! per arm, each in `data/evidence/m8/optimization/raw/<arm>/run-00N/`), reads the controlled
//! stub A/B that `crates/simulation/tests/state_reuse_ab.rs` wrote, and derives every number
//! in the output from those files. The derivation is checked twice over: against the two
//! independent fields of the same trace line (the per-method table and the call list it was
//! built from), and against the run's own record of which arm it was given.
//!
//! ## Why the comparison is honest rather than convenient
//!
//! The two arms cannot share a pinned block on a live chain: `arbitrage` takes the head the
//! node is at, and §6's shared-pin A/B is what the controlled stub run at block 37 191 169
//! provides instead. So `result_equal` is reported in two parts — the stub's, which is a real
//! equality of two simulations of one state, and the live arms', which is `null` with the
//! reason. A sum over three runs is labelled as a sum over three runs for the same reason:
//! `240` beside a per-run table of `[80, 80, 80]` says what it is, while `240` alone reads
//! like one expensive run.
//!
//! ## Why the committed directory is compared rather than written
//!
//! Every assembly happens under `target/pipeline-tests/`, and a test then requires the files
//! already in `data/evidence/m8/optimization/` to be byte-identical to it. A hand-edited
//! evidence file therefore fails the suite, which is a stronger claim than a comment saying
//! the numbers came from a run. `M831_AB_REFRESH=1` makes the same assembly write the
//! directory first, and that is the only way the committed evidence changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_metrics::baseline::stats;
use evm_pipeline::diagnosis::count_stats;

const OPTIMIZATION_DIR: &str = "data/evidence/m8/optimization";
const RAW: &str = "raw";
const CONTROLLED: &str = "controlled";
const BASELINE: &str = "baseline";
const CACHED: &str = "cached";
const RUNS_PER_ARM: usize = 3;

const HEADER_METHOD: &str = "eth_getBlockByNumber";
const STATE_METHODS: [&str; 4] = [
    "eth_getCode",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getStorageAt",
];
/// The boundary's four kinds and the method each one costs when it misses, so the tally's
/// vocabulary (`code`, `balance`) sits beside the wire's (`eth_getCode`, …) and a reader can
/// line the two accounts up without guessing at a mapping.
const KINDS: [(&str, &str); 4] = [
    ("code", "eth_getCode"),
    ("balance", "eth_getBalance"),
    ("nonce", "eth_getTransactionCount"),
    ("storage", "eth_getStorageAt"),
];

const RUN_FILE: &str = "route-run.json";
const TRACES_FILE: &str = "simulation-traces.jsonl";
const RPC_SUMMARY_FILE: &str = "rpc-summary.json";
const CONTROLLED_FILE: &str = "m8.3.1-ab.json";
/// M8.1's latency evidence, filed with the run that wrote it. It times the same span as the
/// trace line's `simulation_duration_ns` from a different clock, so it can check that figure
/// without being part of it.
const LATENCY_DIR: &str = "latency";
const LATENCY_SUMMARY_FILE: &str = "summary.json";
const LATENCY_SIMULATION_KEY: &str = "simulation_duration";

/// §19's filenames, spelled in the one place both the writer and the assertions read.
const AB_FILE: &str = "m8.3.1-ab.json";
const COMPARISON_FILE: &str = "rpc-count-comparison.json";
const CACHE_STATS_FILE: &str = "cache-stats.json";
const README_FILE: &str = "README.md";
/// Four documents plus one file per run per arm — the §19 list, checked rather than assumed.
const EXPECTED_FILE_COUNT: usize = 4 + RUNS_PER_ARM * 2;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn optimization_dir() -> PathBuf {
    workspace_root().join(OPTIMIZATION_DIR)
}

/// A path inside the evidence directory, stated relative to it: an evidence file that names
/// where a number came from has to name it the same way whether the reader opened the copy in
/// the repo or a fresh assembly under `target/`.
fn relative(dir: &Path) -> String {
    let base = optimization_dir()
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: {error}", optimization_dir().display()));
    let absolute = dir
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    absolute
        .strip_prefix(&base)
        .map(|path| path.display().to_string())
        .expect("a run directory read by this assembler is inside the evidence directory")
}

fn read_json(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn controlled_document() -> Value {
    read_json(
        &optimization_dir()
            .join(RAW)
            .join(CONTROLLED)
            .join(CONTROLLED_FILE),
    )
}

fn trace_lines(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}: a line is not json: {error}", path.display()))
        })
        .collect()
}

/// The session directory a route run left inside its own evidence directory — the one
/// subdirectory holding `route-run.json`. `--rpc-output` sits beside it rather than inside it,
/// so this is found by the file it must contain instead of by position.
fn session_dir(run_dir: &Path) -> String {
    let entries =
        std::fs::read_dir(run_dir).unwrap_or_else(|error| panic!("{}: {error}", run_dir.display()));
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("{}: {error}", run_dir.display()));
        let path = entry.path();
        if path.is_dir() && path.join(RUN_FILE).is_file() {
            found.push(
                path.file_name()
                    .expect("a session directory has a name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    match found.as_slice() {
        [one] => one.clone(),
        other => panic!(
            "{}: expected exactly one session directory holding {RUN_FILE}, found {}",
            run_dir.display(),
            other.len()
        ),
    }
}

/// One live route run, as its own files describe it.
struct Run {
    arm: &'static str,
    label: String,
    /// Where the run's files are, relative to the evidence directory.
    dir: String,
    session: String,
    git_revision: String,
    record: Value,
    lines: Vec<Value>,
    /// The same run's simulation span as M8.1's latency trace measured it — a second clock,
    /// in a file this assembler does not otherwise read.
    second_clock_ns: u64,
}

/// The run's own latency evidence: one source, one sample, and the simulation span it timed.
///
/// Read from M8.1's file rather than M8.2's because the two milestones instrument the same
/// span from opposite ends — the route run's stage clock and the RPC boundary's wall clock.
/// A §15 duration that both of them agree on is a measured one; a figure only one clock ever
/// produced is a figure with no cross-check available.
fn second_clock(run_dir: &Path, session: &str) -> u64 {
    let path = run_dir
        .join(LATENCY_DIR)
        .join(session)
        .join(LATENCY_SUMMARY_FILE);
    let summary = read_json(&path);
    let sources = summary["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("{}: no `sources`", path.display()));
    let [source] = sources.as_slice() else {
        panic!(
            "{}: {} sources timed this run; a route run is one",
            path.display(),
            sources.len()
        );
    };
    let samples = source["sample_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("{}: `sample_count` is not a number", path.display()));
    if samples != 1 {
        panic!(
            "{}: this file timed {samples} simulations; the run traced one",
            path.display()
        );
    }
    source["latencies_ns"][LATENCY_SIMULATION_KEY]["min_ns"]
        .as_u64()
        .unwrap_or_else(|| {
            panic!(
                "{}: {LATENCY_SIMULATION_KEY} was not measured",
                path.display()
            )
        })
}

impl Run {
    fn read(arm: &'static str, label: &str) -> Self {
        let run_dir = optimization_dir().join(RAW).join(arm).join(label);
        let session = session_dir(&run_dir);
        let diagnosis = run_dir.join("diagnosis").join(&session);
        let record = read_json(&run_dir.join(&session).join(RUN_FILE));
        let summary = read_json(&diagnosis.join(RPC_SUMMARY_FILE));
        let git_revision = summary["git_revision"]
            .as_str()
            .unwrap_or_else(|| panic!("{}: no git_revision recorded", diagnosis.display()))
            .to_string();
        let second_clock_ns = second_clock(&run_dir, &session);
        Self {
            arm,
            label: label.to_string(),
            dir: relative(&run_dir),
            session,
            git_revision,
            record,
            lines: trace_lines(&diagnosis.join(TRACES_FILE)),
            second_clock_ns,
        }
    }

    /// The run's recorded simulation window. A route run traces one simulation today, and two
    /// would mean the run priced a second opportunity — silently taking the first line would
    /// then be a number this file chose rather than one it read.
    fn line(&self) -> &Value {
        match self.lines.as_slice() {
            [one] => one,
            other => panic!(
                "{}: {} simulations were traced; this assembler reads a route run's one",
                self.dir,
                other.len()
            ),
        }
    }

    /// A field of this run's trace line.
    fn field(&self, path: &[&str]) -> usize {
        let mut value = self.line();
        for key in path {
            value = &value[key];
        }
        value
            .as_u64()
            .unwrap_or_else(|| panic!("{}: {} is not a count", self.dir, path.join(".")))
            as usize
    }
}

fn read_arm(arm: &'static str) -> Vec<Run> {
    (1..=RUNS_PER_ARM)
        .map(|index| Run::read(arm, &format!("run-{index:03}")))
        .collect()
}

/// Per-method call counts, recounted from the individual calls in the trace line.
///
/// Deliberately a second derivation: `methods` in the same line is the analysis the run wrote,
/// and an assembler that trusted only that field would inherit any mistake in it. The tests
/// compare the two.
fn counts_from_calls(line: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for call in line["calls"].as_array().into_iter().flatten() {
        let method = call["method"]
            .as_str()
            .unwrap_or_else(|| panic!("{line}: a call carries no method"))
            .to_string();
        *counts.entry(method).or_default() += 1;
    }
    counts
}

fn declared_counts(line: &Value) -> BTreeMap<String, usize> {
    line["methods"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            (
                row["method"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{line}: a method row carries no name"))
                    .to_string(),
                row["count"].as_u64().unwrap_or_default() as usize,
            )
        })
        .collect()
}

/// A duration in both units a reader may want, as integers. `ms` is a truncation and says so:
/// 14 358 245 625 ns is 14 358 ms, not 14 358.2, and whoever compares the two figures needs to
/// know which one lost digits.
fn duration(ns: u64) -> Value {
    json!({ "ns": ns, "ms": ns / 1_000_000, "ms_is_truncated": true })
}

/// §14's per-simulation row, in the field names §14 asks for.
fn simulation_row(run: &Run) -> Value {
    let counts = counts_from_calls(run.line());
    let header = counts.get(HEADER_METHOD).copied().unwrap_or_default();
    let state_rpc: usize = STATE_METHODS
        .iter()
        .map(|method| counts.get(*method).copied().unwrap_or_default())
        .sum();
    let cache = &run.line()["state_read_cache"];
    let line = run.line();
    json!({
        "block": line["block_number"],
        "simulation_id": line["simulation_id"],
        "arm": run.arm,
        "rpc_count": line["call_count"],
        "rpc_count_by_method": counts,
        "state_rpc_count": state_rpc,
        "header_rpc_count": header,
        "cache_hit": cache["cache_hits"],
        "cache_miss": cache["cache_misses"],
        "cache_by_kind": KINDS.iter().map(|(kind, method)| json!({
            "kind": kind,
            "method": method,
            "hits": cache[kind]["hits"],
            "misses": cache[kind]["misses"],
        })).collect::<Vec<_>>(),
        "duplicate_reads": line["duplicates"]["duplicate_state_reads"],
        "unique_reads": line["duplicates"]["unique_state_reads"],
        "simulation_duration": duration(run.field(&["simulation_duration_ns"]) as u64),
        "rpc_wall_duration": duration(run.field(&["rpc", "rpc_wall_duration_ns"]) as u64),
        "state_read_tally_scope": cache["tally_scope"],
        "result": {
            "status": run.record["simulation"]["status"],
            "completed": run.record["simulation"]["completed"],
            "outcome": run.record["simulation"]["outcome"]
                .as_object()
                .map(|row| row.keys().cloned().collect::<Vec<_>>()),
            "fingerprint": run.record["simulation"]["fingerprint"],
            "gas_used": run.record["simulation"]["gas_used"],
            "read_from": format!("{}/{}/{RUN_FILE}: simulation", run.dir, run.session),
        },
    })
}

/// One run as a file of its own — §19's `baseline/run-001.json` and its three siblings.
fn run_file(run: &Run) -> Value {
    let traced_ns = run.field(&["simulation_duration_ns"]) as u64;
    json!({
        "schema": "m8.3.1-live-run/v1",
        "milestone": "M8.3.1",
        "arm": run.arm,
        "run_label": run.label,
        "state_read_reuse": run.record["state_read_reuse"],
        "read_from": {
            "directory": run.dir,
            "run_record": format!("{}/{}/{RUN_FILE}", run.dir, run.session),
            "simulation_traces": format!("{}/diagnosis/{}/{}", run.dir, run.session, TRACES_FILE),
            "latency_summary": format!("{}/{LATENCY_DIR}/{}/{LATENCY_SUMMARY_FILE}", run.dir, run.session),
        },
        "session_id": run.record["session_id"],
        "git_revision": run.git_revision,
        "chain_id": run.record["chain_id"],
        "pinned_block": run.record["pinned_block"],
        "execution_mode": run.record["mode"],
        "market": run.record["market"],
        "successful_real_arbitrage": run.record["successful_real_arbitrage"],
        "refusal": run.record["refusal"],
        // The two clocks this milestone has for one span: M8.2's, which every §15 duration in
        // this directory is read from, and M8.1's, which timed the same stage from the route
        // run's own stage list. The difference is printed rather than smoothed away.
        "second_clock": {
            "simulation_duration_ns": run.second_clock_ns,
            "traced_by_rpc_boundary_ns": traced_ns,
            "difference_ns": traced_ns.abs_diff(run.second_clock_ns),
            "key": format!("sources[].latencies_ns.{LATENCY_SIMULATION_KEY}.min_ns"),
        },
        "simulations": [simulation_row(run)],
    })
}

/// One figure named the way §15 names it: the per-run values, their sum, and the distribution
/// M8.1's sample policy allows over that many samples. A total without its per-run column is
/// how `240` ends up looking like one run that cost 240 calls.
fn metric(unit: &str, values: &[u64]) -> Value {
    json!({
        "unit": unit,
        "per_run": values,
        "total": values.iter().sum::<u64>(),
        "distribution": if unit == "ns" { stats(values) } else { count_stats(values) },
    })
}

/// The reduction between two arms, in integer terms: §13 forbids a float here and §15 still
/// asks for a reduction, so it is reported as what was removed out of what the baseline paid,
/// plus a per-mille integer for anyone who wants the proportion in one number.
fn reduction(baseline: u64, optimized: u64) -> Value {
    let removed = baseline.saturating_sub(optimized);
    json!({
        "baseline_total_ns": baseline,
        "optimized_total_ns": optimized,
        "removed_ns": removed,
        "integer_terms": {
            "numerator": removed,
            "denominator": baseline,
            "notation": "integer terms, no float (§13)",
        },
        "removed_per_mille": (removed * 1_000)
            .checked_div(baseline)
            .map(|per_mille| json!(per_mille))
            .unwrap_or(Value::Null),
    })
}

fn values(runs: &[Run], path: &[&str]) -> Vec<u64> {
    runs.iter()
        .map(|run| run.field(path) as u64)
        .collect::<Vec<_>>()
}

fn call_counts(runs: &[Run]) -> Vec<u64> {
    runs.iter()
        .map(|run| counts_from_calls(run.line()).values().sum::<usize>() as u64)
        .collect::<Vec<_>>()
}

fn duplicate_total(runs: &[Run]) -> u64 {
    runs.iter()
        .map(|run| run.field(&["duplicates", "duplicate_state_reads"]) as u64)
        .sum()
}

/// §15's headline document: one place that answers 「快了多少」 without a reader adding two
/// files together, with the controlled stub A/B embedded as that test wrote it.
fn ab_file(baseline: &[Run], cached: &[Run], controlled: &Value) -> Value {
    let baseline_wall = values(baseline, &["rpc", "rpc_wall_duration_ns"]);
    let cached_wall = values(cached, &["rpc", "rpc_wall_duration_ns"]);
    let baseline_sim = values(baseline, &["simulation_duration_ns"]);
    let cached_sim = values(cached, &["simulation_duration_ns"]);
    let sum = |values: &[u64]| values.iter().sum::<u64>();
    json!({
        "schema": "m8.3.1-ab/v1",
        "milestone": "M8.3.1",
        "variable": "state read reuse, and nothing else. Both arms are the same command line \
                     with one switch added (§6); the arm a run was given is read out of that \
                     run's own record rather than assumed from the directory it was filed \
                     under.",
        "git_revisions": {
            "live_runs": baseline
                .iter()
                .chain(cached)
                .map(|run| run.git_revision.clone())
                .collect::<Vec<_>>(),
            "controlled_stub": controlled["generated_by"],
        },
        "arms": {
            "baseline": {
                "state_read_reuse": false,
                "switch": "--no-state-read-reuse",
                "runs": baseline.iter().map(|run| run.dir.clone()).collect::<Vec<_>>(),
            },
            "cached": {
                "state_read_reuse": true,
                "switch": "the default: no flag",
                "runs": cached.iter().map(|run| run.dir.clone()).collect::<Vec<_>>(),
            },
        },
        "metrics": {
            "baseline_rpc_count": metric("count", &call_counts(baseline)),
            "optimized_rpc_count": metric("count", &call_counts(cached)),
            "baseline_duplicate_reads": metric("count", &values(baseline, &["duplicates", "duplicate_state_reads"])),
            "optimized_duplicate_reads": metric("count", &values(cached, &["duplicates", "duplicate_state_reads"])),
            "cache_hits": {
                "baseline_arm": metric("count", &values(baseline, &["state_read_cache", "cache_hits"])),
                "cached_arm": metric("count", &values(cached, &["state_read_cache", "cache_hits"])),
            },
            "cache_misses": {
                "baseline_arm": metric("count", &values(baseline, &["state_read_cache", "cache_misses"])),
                "cached_arm": metric("count", &values(cached, &["state_read_cache", "cache_misses"])),
            },
            "baseline_rpc_wall_duration": metric("ns", &baseline_wall),
            "optimized_rpc_wall_duration": metric("ns", &cached_wall),
            "baseline_simulation_duration": metric("ns", &baseline_sim),
            "optimized_simulation_duration": metric("ns", &cached_sim),
            "rpc_time_reduction": reduction(sum(&baseline_wall), sum(&cached_wall)),
            "simulation_time_reduction": reduction(sum(&baseline_sim), sum(&cached_sim)),
            "result_equal": {
                "controlled_stub_ab": controlled["comparison"]["result_equal"],
                "controlled_stub_ab_read_from": format!("raw/{CONTROLLED}/{CONTROLLED_FILE}: comparison.result_equal"),
                "live_arms": Value::Null,
                "live_arms_reason": "the two arms ran against the live head, and a head \
                     advances: each run pins the block the node was at when it asked, so no \
                     live pair shares §6's pinned block and no live pair has a result that \
                     should be equal. Each run's fingerprint is recorded anyway, below, so a \
                     reader can see what each arm actually computed. Equality of two \
                     simulations of one pinned state is the controlled stub A/B's claim, at \
                     block 37191169, read from the file named above.",
                "per_run_fingerprints": {
                    "baseline": baseline.iter().map(|run| json!({
                        "run": run.label,
                        "block": run.record["pinned_block"],
                        "fingerprint": run.record["simulation"]["fingerprint"],
                    })).collect::<Vec<_>>(),
                    "cached": cached.iter().map(|run| json!({
                        "run": run.label,
                        "block": run.record["pinned_block"],
                        "fingerprint": run.record["simulation"]["fingerprint"],
                    })).collect::<Vec<_>>(),
                },
            },
        },
        "sample_policy": "M8.1's rule, unchanged: min and max from one sample, p50 from two, \
                          p90 from ten, p95 from twenty, p99 from a hundred. Three runs per \
                          arm therefore report min, p50 and max and null above that — §15 says \
                          样本不足不要算 p95/p99, and the ranks that need more runs say so in \
                          the file rather than in this sentence.",
        "totals_are_sums_over_runs": "every `total` here is the sum over the runs listed under \
                          `arms`; each run's own figures are in `per_run` and in \
                          baseline/run-00N.json and cached/run-00N.json.",
        "controlled_stub_ab": controlled,
        "files": [AB_FILE, COMPARISON_FILE, CACHE_STATS_FILE, README_FILE, "baseline/", "cached/"],
    })
}

/// §7's requirement that the arms' *method sequences* be compared rather than only counted.
fn comparison_file(baseline: &[Run], cached: &[Run], controlled: &Value) -> Value {
    let rows = |arm: &str, runs: &[Run]| -> Vec<Value> {
        runs.iter()
            .map(|run| {
                json!({
                    "run": run.label,
                    "arm": arm,
                    "block": run.record["pinned_block"],
                    "session_id": run.record["session_id"],
                    "total_calls": counts_from_calls(run.line()).values().sum::<usize>(),
                    "counts": counts_from_calls(run.line()),
                })
            })
            .collect()
    };
    let totals = |runs: &[Run]| -> BTreeMap<String, usize> {
        let mut totals = BTreeMap::new();
        for run in runs {
            for (method, count) in counts_from_calls(run.line()) {
                *totals.entry(method).or_default() += count;
            }
        }
        totals
    };
    let baseline_totals = totals(baseline);
    let cached_totals = totals(cached);
    let removed = |method: &&str| -> usize {
        baseline_totals
            .get(*method)
            .copied()
            .unwrap_or_default()
            .saturating_sub(cached_totals.get(*method).copied().unwrap_or_default())
    };
    json!({
        "schema": "m8.3.1-rpc-count-comparison/v1",
        "measured_by": "the request counter at the RPC boundary — the same events M8.2's \
                       diagnosis records, counted per method here rather than per key (§7). \
                       Nothing in this file is inferred from the cache statistics.",
        "baseline_runs": rows(BASELINE, baseline),
        "cached_runs": rows(CACHED, cached),
        "totals": {
            "baseline": baseline_totals,
            "cached": cached_totals,
            "per_method": STATE_METHODS.iter().chain([HEADER_METHOD].iter()).map(|method| json!({
                "method": *method,
                "baseline": baseline_totals.get(*method).copied().unwrap_or_default(),
                "cached": cached_totals.get(*method).copied().unwrap_or_default(),
                "removed": removed(method),
            })).collect::<Vec<_>>(),
            "state_reads_removed": STATE_METHODS.iter().map(|method| removed(method) as u64).sum::<u64>(),
            "note": "these are sums over the runs each arm recorded, and the per-run columns \
                     are above; §19 forbids a figure that is not in a run's own files.",
        },
        "duplicates": {
            "baseline_duplicate_reads": duplicate_total(baseline),
            "cached_duplicate_reads": duplicate_total(cached),
            "note": "a duplicate read is one §3 identity asked of the same pinned block twice \
                     inside a single simulation. That is M8.2's measurement and a different \
                     account from the cache tally: this counts arrivals at the endpoint, the \
                     tally counts lookups at the boundary.",
        },
        "method_sequence": {
            "live_arms": "not compared. The arms ran at different heads, so their call lists \
                          are not the same questions in the same order, and a subsequence check \
                          across them would be comparing chain state rather than behaviour.",
            "controlled_stub_ab_per_method_removed": controlled["comparison"]["per_method_removed"],
            "controlled_stub_ab_note": "the sequence check itself — the cached arm's calls are \
                          the baseline arm's in the same order with whole calls removed — is \
                          asserted where both arms share one pinned block, by \
                          crates/simulation/tests/state_reuse_ab.rs.",
        },
    })
}

/// §12's table: hits and misses per kind per arm, each with that kind's wire count beside it.
fn cache_stats_file(baseline: &[Run], cached: &[Run]) -> Value {
    let per_kind = |runs: &[Run]| -> Value {
        KINDS
            .iter()
            .map(|(kind, method)| {
                let hits: usize = runs
                    .iter()
                    .map(|run| run.field(&["state_read_cache", kind, "hits"]))
                    .sum();
                let misses: usize = runs
                    .iter()
                    .map(|run| run.field(&["state_read_cache", kind, "misses"]))
                    .sum();
                let wire: usize = runs
                    .iter()
                    .map(|run| {
                        counts_from_calls(run.line())
                            .get(*method)
                            .copied()
                            .unwrap_or_default()
                    })
                    .sum();
                json!({
                    "kind": kind,
                    "method": method,
                    "hits": hits,
                    "misses": misses,
                    "lookups": hits + misses,
                    "wire_calls": wire,
                    "misses_equal_wire": misses == wire,
                })
            })
            .collect()
    };
    json!({
        "schema": "m8.3.1-cache-stats/v1",
        "source": format!("each run's {TRACES_FILE} line, `state_read_cache` — the boundary's \
                   own counter, incremented at the lookup that made it true and never derived \
                   from the call list."),
        "cached_arm": {
            "runs": cached.iter().map(|run| run.label.clone()).collect::<Vec<_>>(),
            "per_kind": per_kind(cached),
            "alignment": "misses_equal_wire is §12's check. In the reuse arm every read the \
                          boundary did not already hold went to the node, and nothing reached \
                          the node that the boundary had not been asked about. A kind where \
                          that is false is a read that bypassed the boundary — §16's hidden \
                          problem, caught by an arithmetic check rather than by an audit.",
        },
        "baseline_arm": {
            "runs": baseline.iter().map(|run| run.label.clone()).collect::<Vec<_>>(),
            "per_kind": per_kind(baseline),
            "why_the_account_kinds_carry_nothing": "with reuse off, balance, nonce and the \
                          bytecode an account read asks for are never looked up, so they have \
                          neither hits nor misses to report and every one of their reads is a \
                          wire call. Only the two kinds this boundary already reused at M8.2's \
                          HEAD — a direct bytecode read, and storage — carry numbers here, \
                          which is why baseline's `wire_calls` exceed its lookups.",
        },
    })
}

/// The directory's README: what each file holds and which key of which source file a reader
/// should check. No measured figure appears in it, on purpose — a number copied into prose is
/// a number that can go quietly wrong when the runs are re-assembled.
fn readme(baseline: &[Run], cached: &[Run]) -> String {
    let listed = |runs: &[Run]| -> String {
        runs.iter()
            .map(|run| {
                format!(
                    "- `{}` — session `{}`, pinned block {}, git revision `{}`",
                    run.dir,
                    run.session,
                    run.record["pinned_block"].as_u64().unwrap_or_default(),
                    run.git_revision
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# M8.3.1 — state read reuse A/B evidence\n\n\
         One milestone's single variable, measured twice on the live chain: the same route run \n\
         with state read reuse on and with it off, plus a controlled A/B over one pinned block \n\
         where the two arms' results can be compared rather than only described.\n\n\
         Nothing in this directory was typed in. Every file here is assembled by \n\
         `crates/pipeline/tests/reuse_ab_evidence.rs` from the runs under `raw/`, and the suite \n\
         requires these committed files to be byte-identical to a fresh assembly of those same \n\
         runs — so an edited number fails a test. Regenerate with `M831_AB_REFRESH=1 cargo test \n\
         -p evm-pipeline --test reuse_ab_evidence`.\n\n\
         ## What is here\n\n\
         - `m8.3.1-ab.json` — §15's headline metrics, each with its per-run figures beside its \n\
           total, the reduction in integer terms, and the controlled stub A/B embedded as it was \n\
           written.\n\
         - `rpc-count-comparison.json` — §7's per-method request counts for both arms, the calls \n\
           removed per method, and the duplicate tallies.\n\
         - `cache-stats.json` — §12's hits and misses per kind per arm, each beside that kind's \n\
           wire count, so `misses_equal_wire` can be checked without trusting either number.\n\
         - `baseline/run-00N.json`, `cached/run-00N.json` — one file per run, carrying §14's \n\
           per-simulation fields: `block`, `simulation_id`, `arm`, `rpc_count`, `cache_hit`, \n\
           `cache_miss`, `simulation_duration`, `result`.\n\
         - `raw/` — what the runs themselves wrote: each run's evidence directory, its \n\
           `diagnosis/` directory from `--rpc-trace`, and its `latency/` directory from \n\
           `--latency-trace`, plus `raw/controlled/m8.3.1-ab.json` from the stub A/B. Every \n\
           figure above is read out of these files.\n\n\
         ## Where each number comes from\n\n\
         | figure | file | key |\n\
         | --- | --- | --- |\n\
         | `rpc_count` | `raw/<arm>/run-00N/diagnosis/<session>/simulation-traces.jsonl` | `call_count`, re-derived from `calls[]` and compared |\n\
         | per-method counts | the same line | `methods[].count`, recounted from `calls[]` |\n\
         | `cache_hit`, `cache_miss` | the same line | `state_read_cache.cache_hits`, `.cache_misses` |\n\
         | duplicate reads | the same line | `duplicates.duplicate_state_reads` |\n\
         | `simulation_duration` | the same line | `simulation_duration_ns` |\n\
         | the same span, timed again | `raw/<arm>/run-00N/latency/<session>/summary.json` | `sources[].latencies_ns.simulation_duration.min_ns` |\n\
         | `rpc_wall_duration` | the same line | `rpc.rpc_wall_duration_ns` |\n\
         | which arm a run was | `<session>/route-run.json` | `state_read_reuse` |\n\
         | `result` | `<session>/route-run.json` | `simulation.status`, `.completed`, `.outcome`, `.fingerprint` |\n\
         | result equality | `raw/controlled/m8.3.1-ab.json` | `comparison.result_equal` |\n\n\
         ## Two accounts of one boundary\n\n\
         The wire count and the cache tally are independent measurements of the same thing: one \n\
         counts calls that reached the node, the other counts lookups the boundary answered. In \n\
         the reuse arm they reconcile exactly — every miss is a wire call and every wire call is \n\
         a miss (`misses_equal_wire`). In the baseline arm the account kinds carry no lookups at \n\
         all, because the switch does not let the boundary be asked about them.\n\n\
         ## What these runs did and did not do\n\n\
         Both arms ran `--execution-mode build-only`: no ETH was spent, nothing was signed, \n\
         nothing was broadcast (§14). Each run's simulation completed; what happened after the \n\
         simulation belongs to M7's lifecycle ladder and is recorded per run in `refusal` and \n\
         `successful_real_arbitrage`, untouched by this milestone.\n\n\
         ## Runs read\n\n\
         baseline:\n{baseline}\n\ncached:\n{cached}\n",
        baseline = listed(baseline),
        cached = listed(cached),
    )
}

/// Every file the assembler writes, keyed by its name in the directory (with `baseline/` and
/// `cached/` for the per-run files).
fn assemble(baseline: &[Run], cached: &[Run], controlled: &Value) -> BTreeMap<String, Value> {
    let mut files = BTreeMap::new();
    files.insert(AB_FILE.to_string(), ab_file(baseline, cached, controlled));
    files.insert(
        COMPARISON_FILE.to_string(),
        comparison_file(baseline, cached, controlled),
    );
    files.insert(
        CACHE_STATS_FILE.to_string(),
        cache_stats_file(baseline, cached),
    );
    for (arm, runs) in [(BASELINE, baseline), (CACHED, cached)] {
        for (index, run) in runs.iter().enumerate() {
            files.insert(format!("{arm}/run-{:03}.json", index + 1), run_file(run));
        }
    }
    files
}

fn render(value: &Value) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("the assembled evidence is serializable")
    )
}

/// `M831_AB_REFRESH` names no directory — its presence is the instruction to write the
/// evidence directory in the repo. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M831_AB_REFRESH").is_some()
}

fn write_files(dir: &Path, files: &BTreeMap<String, Value>, readme: &str) {
    for (name, value) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
        }
        std::fs::write(&path, render(value))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    let path = dir.join(README_FILE);
    std::fs::write(&path, readme).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// The assembly, as a directory. `wipe` deletes the directory first, which is what a scratch
/// directory under `target/` wants; the committed evidence directory is never wiped, because
/// `raw/` — the runs this file reads — lives inside it.
fn assemble_to(dir: &Path, wipe: bool) -> Vec<String> {
    let baseline = read_arm(BASELINE);
    let cached = read_arm(CACHED);
    let files = assemble(&baseline, &cached, &controlled_document());
    let text = readme(&baseline, &cached);
    if wipe {
        let _ = std::fs::remove_dir_all(dir);
    }
    write_files(dir, &files, &text);
    let mut names = files.keys().cloned().collect::<Vec<_>>();
    names.push(README_FILE.to_string());
    names.sort();
    names
}

fn fresh_assembly(name: &str) -> (PathBuf, Vec<String>) {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    let names = assemble_to(&dir, true);
    (dir, names)
}

/// §19's 「不要手写数字」 as a gate rather than a promise: the committed directory has to be a
/// rendering of the runs, byte for byte. Only this test writes the repo's evidence tree, only
/// when told to, and the comparison still runs afterwards so a refresh cannot fail silently.
#[test]
fn the_committed_evidence_is_a_byte_for_byte_rendering_of_the_runs() {
    let (fresh, names) = fresh_assembly("m8.3.1-ab-check");
    let committed = optimization_dir();
    if refreshing() {
        assemble_to(&committed, false);
        eprintln!("refreshed {OPTIMIZATION_DIR}");
    }
    let mut diffs = Vec::new();
    for name in &names {
        let expected = std::fs::read(fresh.join(name)).expect("freshly written file");
        match std::fs::read(committed.join(name)) {
            Ok(actual) if actual == expected => {}
            Ok(_) => diffs.push(name.clone()),
            Err(error) => diffs.push(format!("{name} (missing: {error})")),
        }
    }
    assert!(
        diffs.is_empty(),
        "{} of the {} files under {} differ from a fresh assembly of the runs in `raw/`. \
         Regenerate them with `M831_AB_REFRESH=1 cargo test -p evm-pipeline --test \
         reuse_ab_evidence` rather than editing a figure: {diffs:?}",
        diffs.len(),
        names.len(),
        OPTIMIZATION_DIR,
    );
    assert_eq!(
        names.len(),
        EXPECTED_FILE_COUNT,
        "§19 asks for three summary files, a README, and one file per run per arm"
    );
}

/// §14 and §7 together: three runs per arm exist, each one records the switch its directory
/// says it was given, each stayed inside build-only, and the trace line's two accounts of its
/// own calls agree.
#[test]
fn each_arm_has_three_live_runs_and_each_records_which_arm_it_was() {
    for (arm, reuse) in [(BASELINE, false), (CACHED, true)] {
        let runs = read_arm(arm);
        assert_eq!(runs.len(), RUNS_PER_ARM, "§14 wants at least three");
        for run in &runs {
            assert_eq!(
                run.record["state_read_reuse"].as_bool(),
                Some(reuse),
                "{} filed under `{arm}` but records the other switch",
                run.dir
            );
            assert_eq!(
                run.record["mode"].as_str(),
                Some("build-only"),
                "{}: §14's 0 ETH / 0 signature / 0 broadcast means this run never left \
                 build-only",
                run.dir
            );
            assert_eq!(
                run.record["successful_real_arbitrage"].as_bool(),
                Some(false),
                "{} traded nothing",
                run.dir
            );
            assert_eq!(
                run.record["simulation"]["completed"].as_bool(),
                Some(true),
                "{}: a run whose simulation did not complete measures reuse on nothing",
                run.dir
            );
            assert_eq!(
                counts_from_calls(run.line()),
                declared_counts(run.line()),
                "{}: the per-method table and the call list it was built from disagree",
                run.dir
            );
            assert_eq!(
                counts_from_calls(run.line()).values().sum::<usize>(),
                run.field(&["call_count"]),
                "{}: {} calls counted against {} recorded",
                run.dir,
                counts_from_calls(run.line()).values().sum::<usize>(),
                run.line()["call_count"]
            );
        }
    }
}

/// §12's alignment stated as the falsifiable check it is: in the reuse arm a miss *is* a wire
/// call, per kind and in total.
#[test]
fn in_the_reuse_arm_every_miss_is_a_request_and_every_request_is_a_miss() {
    for run in read_arm(CACHED) {
        assert_eq!(
            run.line()["state_read_cache"]["reuse"].as_bool(),
            Some(true),
            "{}: the boundary says it was not reusing",
            run.dir
        );
        for (kind, method) in KINDS {
            assert_eq!(
                run.field(&["state_read_cache", kind, "misses"]),
                counts_from_calls(run.line())
                    .get(method)
                    .copied()
                    .unwrap_or_default(),
                "{}: {kind} — the boundary's misses and its {method} requests differ",
                run.dir
            );
        }
        let state_wire: usize = STATE_METHODS
            .iter()
            .map(|method| {
                counts_from_calls(run.line())
                    .get(*method)
                    .copied()
                    .unwrap_or_default()
            })
            .sum();
        assert_eq!(
            run.field(&["state_read_cache", "cache_misses"]),
            state_wire,
            "{}: {} state reads reached the node while the boundary reports {} misses",
            run.dir,
            state_wire,
            run.field(&["state_read_cache", "cache_misses"])
        );
    }
}

/// §16 from the other side: only the account kinds move between arms. Storage and the header
/// read cost the same in both, because they were never this milestone's variable — and the
/// calls the reuse arm did not make are exactly the duplicate account reads M8.2 counted.
#[test]
fn only_the_account_kinds_move_between_the_two_arms() {
    let baseline = read_arm(BASELINE);
    let cached = read_arm(CACHED);
    for index in 0..RUNS_PER_ARM {
        let baseline_counts = counts_from_calls(baseline[index].line());
        let cached_counts = counts_from_calls(cached[index].line());
        assert_eq!(
            baseline_counts[HEADER_METHOD],
            cached_counts[HEADER_METHOD],
            "run-{:03}: eth_getBlockByNumber moved, and §1 leaves it alone",
            index + 1
        );
        assert_eq!(
            baseline_counts["eth_getStorageAt"],
            cached_counts["eth_getStorageAt"],
            "run-{:03}: storage moved, and it already had a full key at M8.2's HEAD",
            index + 1
        );
        for method in ["eth_getCode", "eth_getBalance", "eth_getTransactionCount"] {
            assert!(
                cached_counts[method] < baseline_counts[method],
                "run-{:03}: {method} cost {} in the reuse arm where it cost {} — reuse \
                 answered nothing this milestone exists to answer",
                index + 1,
                cached_counts[method],
                baseline_counts[method]
            );
        }
        let removed: usize = STATE_METHODS
            .iter()
            .map(|method| {
                baseline_counts.get(*method).copied().unwrap_or_default()
                    - cached_counts.get(*method).copied().unwrap_or_default()
            })
            .sum();
        assert_eq!(
            removed,
            baseline[index].field(&["duplicates", "duplicate_state_reads"]),
            "run-{:03}: removing {removed} calls did not equal the {} duplicate reads M8.2's \
             analysis counted",
            index + 1,
            baseline[index].line()["duplicates"]["duplicate_state_reads"]
        );
    }
}

/// A reader should be able to check the headline table without reading this test, so the
/// headline is compared against sums drawn straight from the runs' own call lists.
#[test]
fn the_headline_metrics_are_the_runs_and_not_a_second_tradition() {
    let baseline = read_arm(BASELINE);
    let cached = read_arm(CACHED);
    let controlled = controlled_document();
    let document = ab_file(&baseline, &cached, &controlled);
    let metrics = &document["metrics"];
    let sum_calls = |runs: &[Run]| -> u64 {
        runs.iter()
            .map(|run| run.field(&["call_count"]) as u64)
            .sum()
    };
    assert_eq!(
        metrics["baseline_rpc_count"]["total"],
        sum_calls(&baseline),
        "baseline_rpc_count is not the sum of the runs' own call counts"
    );
    assert_eq!(
        metrics["optimized_rpc_count"]["total"],
        sum_calls(&cached),
        "optimized_rpc_count is not the sum of the runs' own call counts"
    );
    assert_eq!(
        metrics["baseline_duplicate_reads"]["total"],
        duplicate_total(&baseline),
        "baseline_duplicate_reads is not the duplicate count in the traces"
    );
    assert_eq!(
        metrics["optimized_duplicate_reads"]["total"], 0,
        "the reuse arm still has a duplicate read — the one thing this milestone claims ended"
    );
    assert_eq!(
        metrics["rpc_time_reduction"]["removed_ns"],
        metrics["baseline_rpc_wall_duration"]["total"]
            .as_u64()
            .unwrap_or_default()
            .saturating_sub(
                metrics["optimized_rpc_wall_duration"]["total"]
                    .as_u64()
                    .unwrap_or_default()
            ),
        "the reduction is not the two duration totals subtracted"
    );
    assert_eq!(
        metrics["result_equal"]["controlled_stub_ab"], controlled["comparison"]["result_equal"],
        "the embedded controlled result and the headline figure are two different claims"
    );
    assert!(
        metrics["result_equal"]["live_arms"].is_null(),
        "a live pair cannot be reported as equal results: the arms pin different blocks"
    );
    // §7's shape, in the runs' own words: the account kinds each lose whole calls, while the
    // two kinds this milestone left alone cost the same in both arms.
    let per_method =
        comparison_file(&baseline, &cached, &controlled)["totals"]["per_method"].clone();
    for entry in per_method.as_array().expect("a table") {
        let method = entry["method"].as_str().expect("a named method");
        if method == HEADER_METHOD || method == "eth_getStorageAt" {
            assert_eq!(entry["removed"], json!(0), "{method} moved between arms");
        } else {
            assert!(
                entry["removed"].as_u64().unwrap_or_default() > 0,
                "{method} removed nothing"
            );
        }
    }
}

/// Three samples are not a distribution. §15 forbids a p95/p99 over so few runs and M8.1 owns
/// the rule, so the evidence carries the nulls with their reasons instead of a number dressed
/// up as a percentile.
#[test]
fn three_runs_report_min_p50_and_max_and_no_percentile_above_that() {
    let (fresh, _) = fresh_assembly("m8.3.1-sample-policy");
    let document = read_json(&fresh.join(AB_FILE));
    for name in [
        "baseline_rpc_wall_duration",
        "optimized_rpc_wall_duration",
        "baseline_simulation_duration",
        "optimized_simulation_duration",
    ] {
        let row = &document["metrics"][name]["distribution"];
        assert_eq!(row["samples"], json!(RUNS_PER_ARM), "{name}");
        assert!(
            row["min_ns"].is_number(),
            "{name}: min is measurable from one sample"
        );
        assert!(
            row["p50_ns"].is_number(),
            "{name}: p50 is measurable from two"
        );
        assert!(
            row["p90_ns"].is_null() && row["p90_reason"]["reason"] == json!("insufficient_sample"),
            "{name}: a p90 over {} runs is not a percentile",
            RUNS_PER_ARM
        );
        assert!(row["p95_ns"].is_null(), "{name}");
        assert!(row["p99_ns"].is_null(), "{name}");
    }
}

/// The same runs assembled twice must put the same bytes on disk: evidence a reader cannot
/// reproduce from the runs it names is not evidence.
#[test]
fn two_assemblies_of_the_same_runs_write_byte_identical_files() {
    let (first, names) = fresh_assembly("m8.3.1-determinism-a");
    let (second, _) = fresh_assembly("m8.3.1-determinism-b");
    for name in &names {
        let a = std::fs::read(first.join(name)).expect("first assembly");
        let b = std::fs::read(second.join(name)).expect("second assembly");
        assert_eq!(
            a, b,
            "{name} differs between two assemblies of the same runs"
        );
        assert!(!a.is_empty(), "{name} was written empty");
    }
}

/// The controlled A/B is read, not retyped, and it stays a separate account from the live
/// arms — including the fact that a local stub's wire is too fast for a duration there to mean
/// anything.
#[test]
fn the_controlled_ab_is_embedded_as_the_stub_wrote_it() {
    let controlled = controlled_document();
    assert_eq!(controlled["schema"], json!("m8.3.1-ab-stub/v1"));
    assert_eq!(
        controlled["comparison"]["state_reads_removed"],
        json!(41),
        "the controlled A/B no longer removes the 41 duplicate reads §7 and §12 name, so the \
         live comparison is describing a different change"
    );
    assert_eq!(controlled["comparison"]["result_equal"], json!(true));
    let baseline = read_arm(BASELINE);
    let cached = read_arm(CACHED);
    assert_eq!(
        ab_file(&baseline, &cached, &controlled)["controlled_stub_ab"],
        controlled,
        "the embedded document is not the file the stub wrote"
    );
    assert_eq!(
        comparison_file(&baseline, &cached, &controlled)["method_sequence"]
            ["controlled_stub_ab_per_method_removed"],
        controlled["comparison"]["per_method_removed"]
    );
}

/// §15's two durations are read from M8.2's clock, and M8.1 timed the very same span from the
/// route run's stage list. The two disagree by microseconds because they start and stop a few
/// instructions apart, never by a second, and a saving that only one clock ever saw would be a
/// saving with no cross-check — so this test bounds the disagreement and fails if the second
/// clock sees the arms going the other way.
#[test]
fn the_second_clock_times_the_same_span() {
    const ONE_MILLISECOND_NS: u64 = 1_000_000;
    let mut first_total = 0u64;
    let mut second_total = 0u64;
    for (arm, expected_shorter) in [(BASELINE, false), (CACHED, true)] {
        for run in read_arm(arm) {
            let traced = run.field(&["simulation_duration_ns"]) as u64;
            let second = run.second_clock_ns;
            assert!(
                traced.abs_diff(second) < ONE_MILLISECOND_NS,
                "{}: the two clocks of one simulation span differ by {} ns — more than a \
                 bookkeeping gap, so one of them is timing a different span",
                run.dir,
                traced.abs_diff(second)
            );
            if expected_shorter {
                second_total += second;
            } else {
                first_total += second;
            }
        }
    }
    assert!(
        second_total < first_total,
        "the reuse arm's simulations take longer on M8.1's clock ({second_total} ns) than the \
         baseline arm's ({first_total} ns), which contradicts every figure in this directory"
    );
}

/// A figure that no run produced stays an absence rather than becoming a zero: with no runs in
/// either arm the totals are empty populations, `measured: false`, and no reduction to quote.
#[test]
fn an_absent_run_is_an_absence_and_not_a_zero() {
    let document = ab_file(&[], &[], &Value::Null);
    let metrics = &document["metrics"];
    assert_eq!(
        metrics["baseline_rpc_count"]["per_run"],
        json!([] as [u64; 0])
    );
    assert_eq!(
        metrics["baseline_rpc_wall_duration"]["distribution"]["measured"],
        json!(false),
        "a duration population with no samples has no min to print as 0"
    );
    assert!(metrics["rpc_time_reduction"]["removed_per_mille"].is_null());
    assert_eq!(
        metrics["result_equal"]["controlled_stub_ab"],
        Value::Null,
        "with no controlled document there is no result equality to report"
    );
    let stats = cache_stats_file(&[], &[]);
    assert!(
        stats["cached_arm"]["per_kind"]
            .as_array()
            .expect("four kinds")
            .iter()
            .all(|row| row["misses_equal_wire"] == json!(true)),
        "an empty arm reconciles trivially, which is the only thing it can claim"
    );
}
