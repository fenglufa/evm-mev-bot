//! M8.3.2 §19: `data/evidence/m8/post-reuse/`, assembled from the three runs that measured it.
//!
//! §19's hard rule is the one M8.3.1 wrote first — 「证据必须来自实际运行。不要手写数字。」 — so this
//! file contains no measured figure. It reads the three live route runs §16 required (each in
//! `data/evidence/m8/post-reuse/raw/<session>/`) and replays their trace lines through
//! [`DiagnosisEvidence`], the very writer a run uses. Every merged table is therefore produced by
//! the same code as the per-run ones, and the only arithmetic here is comparison: a merged figure
//! against the sum of the raw figures it came from.
//!
//! ## Why replay rather than re-aggregation
//!
//! The alternative to replaying is to fold the three runs' JSON together in this file, which would
//! put a second implementation of §3's percentile tables, §6's address grouping, §9's matrix,
//! §12's gap sweep and §25's A–G rules in a test — correct only while nobody changes the real one.
//! The replay is also the check that a trace line is a complete account rather than a report of
//! one: a decoded line re-emits byte-identically, so nothing the run wrote was missing when the
//! assembly rebuilt it.
//!
//! ## What cannot be pooled, and is not
//!
//! A `started_ns` is nanoseconds since *its own run's* monotonic origin. Per-simulation figures —
//! a gap, a serial wait, an overlap — are differences inside one simulation's window, so three runs
//! contribute three whole simulations and no two clocks are mixed. One figure in this directory
//! does span a run's clock rather than a simulation's: §14's stage spans. They stay with the run
//! that stamped them, in `outside-simulation-rpc.json`'s `assembled_from`, and every pooled row
//! names the run it was classified in.
//!
//! ## Why the committed directory is compared rather than written
//!
//! Assembly always happens under `target/pipeline-tests/`; a test then requires the files in
//! `data/evidence/m8/post-reuse/` to be byte-identical to it, so a hand-edited evidence file fails
//! the suite. `M832_POST_REUSE_REFRESH=1` copies a fresh assembly over the committed directory and
//! is the only way it changes. `raw/` — the runs themselves — lives inside the same directory and
//! nothing here writes or deletes it.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::diagnosis::{
    outside_simulation_table_assembled, DiagnosisEvidence, ACCOUNT_MATRIX_FILE, BOTTLENECK_FILE,
    DUPLICATES_FILE, OUTSIDE_FILE, README_FILE, RPC_GAPS_FILE, RPC_SUMMARY_FILE,
    SIMULATION_SUMMARY_FILE, STORAGE_BREAKDOWN_FILE, TRACES_FILE,
};

const POST_REUSE_DIR: &str = "data/evidence/m8/post-reuse";
const RAW: &str = "raw";

/// §16: three live runs, each with `state_read_reuse` on. Fewer is not a smaller sample of the
/// same finding — it is a milestone that has not run, and says so.
const RUNS_REQUIRED: usize = 3;

/// §3's baseline as the task book declares it, not as anything this file measured: one simulation
/// of this route is 39 calls — 20 storage, 6 code, 6 balance, 6 nonce, 1 header. The merged tables
/// have to come out at three times this shape, per method.
const BASELINE_PER_RUN: [(&str, u64); 5] = [
    ("eth_getStorageAt", 20),
    ("eth_getCode", 6),
    ("eth_getBalance", 6),
    ("eth_getTransactionCount", 6),
    ("eth_getBlockByNumber", 1),
];
const BASELINE_CALLS: u64 = 39;

/// What the directory has to hold: §19's nine names — including `simulation-traces.jsonl`, which
/// is the tenth file a listing counts, and `duplicate-reads.json`, which M8.2's instrumentation
/// already writes and §4's 「不要复制一套新的 RPC recorder」 means this stage reuses rather than
/// drops. In listing order, because a directory read gives names sorted.
const DIRECTORY_FILES: [&str; 10] = [
    README_FILE,
    ACCOUNT_MATRIX_FILE,
    BOTTLENECK_FILE,
    DUPLICATES_FILE,
    OUTSIDE_FILE,
    RPC_GAPS_FILE,
    RPC_SUMMARY_FILE,
    SIMULATION_SUMMARY_FILE,
    TRACES_FILE,
    STORAGE_BREAKDOWN_FILE,
];
const TRACES: &str = TRACES_FILE;

/// `M832_POST_REUSE_REFRESH` names no directory: its presence is the instruction to copy a fresh
/// assembly over the committed evidence. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M832_POST_REUSE_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn post_reuse_dir() -> PathBuf {
    workspace_root().join(POST_REUSE_DIR)
}

/// A scratch assembly directory, emptied first because the evidence writer appends: a second
/// assembly into a directory holding a first would double every line rather than reproduce it.
fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target/pipeline-tests")
        .join(format!("m8.3.2-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn read_json(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
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

/// The run directories, in listing order. `raw/` holds runs and nothing else; a directory in it
/// without a trace file is this file's own mistake and is reported as one rather than skipped.
fn run_names() -> Vec<String> {
    let root = post_reuse_dir().join(RAW);
    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    for name in &names {
        assert!(
            root.join(name).join(TRACES).exists(),
            "{POST_REUSE_DIR}/{RAW}/{name} has no `{TRACES}`: a directory inside the evidence \
             root that is not a run would be silently left out of the assembly"
        );
    }
    names.retain(|name| root.join(name).join(TRACES).exists());
    names.sort();
    names
}

/// Everything the assembly reads out of one run — held as the run's own files, not as a summary of
/// them, so a figure the assembly did not read cannot appear in its output.
struct Run {
    name: String,
    lines: Vec<Value>,
    outside: Value,
    rpc_summary: Value,
    simulation_summary: Value,
    duplicates: Value,
    gaps: Value,
}

impl Run {
    fn read(name: &str) -> Self {
        let dir = post_reuse_dir().join(RAW).join(name);
        let at = |file: &str| dir.join(file);
        Self {
            name: name.to_string(),
            lines: trace_lines(&at(TRACES)),
            outside: read_json(&at(OUTSIDE_FILE)),
            rpc_summary: read_json(&at(RPC_SUMMARY_FILE)),
            simulation_summary: read_json(&at(SIMULATION_SUMMARY_FILE)),
            duplicates: read_json(&at(DUPLICATES_FILE)),
            gaps: read_json(&at(RPC_GAPS_FILE)),
        }
    }

    /// Calls this run's simulations recorded, summed over its sources.
    fn calls(&self) -> u64 {
        sum_per_source(&self.rpc_summary, "total_calls")
    }

    fn simulations(&self) -> u64 {
        self.simulation_summary["simulations"].as_u64().unwrap_or(0)
    }

    fn git_revision(&self) -> &str {
        self.rpc_summary["git_revision"].as_str().unwrap_or("?")
    }

    fn execution_mode(&self) -> &str {
        self.rpc_summary["execution_mode"].as_str().unwrap_or("?")
    }

    fn generated_at(&self) -> u64 {
        self.rpc_summary["generated_at_unix_ms"]
            .as_u64()
            .unwrap_or(0)
    }

    /// §19's provenance row: what the directory says about where its figures came from, in the
    /// words the run itself used.
    fn provenance(&self) -> Value {
        json!({
            "run": self.name,
            "directory": format!("{POST_REUSE_DIR}/{RAW}/{}", self.name),
            "simulations": self.simulations(),
            "simulation_calls": self.calls(),
            "outside_simulation_calls": self.outside["calls"].clone(),
            "git_revision": self.git_revision(),
            "execution_mode": self.execution_mode(),
            "generated_at_unix_ms": self.generated_at(),
            "endpoint_id": self.outside["endpoint_id"].clone(),
        })
    }
}

fn read_runs() -> Vec<Run> {
    run_names().iter().map(|name| Run::read(name)).collect()
}

/// A figure folded out of a summary's per-source rows.
fn sum_per_source(summary: &Value, key: &str) -> u64 {
    summary["per_source"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row[key].as_u64().unwrap_or(0)).sum())
        .unwrap_or(0)
}

/// The assembly: replay every run's lines through one switched-on writer, and hand that writer the
/// §14 half pooled from the runs' own rows.
fn assemble_to(dir: &Path, runs: &[Run]) -> Vec<String> {
    assert!(
        !runs.is_empty(),
        "there is nothing to assemble: {POST_REUSE_DIR}/{RAW} holds no run directory"
    );
    let revisions: Vec<&str> = runs.iter().map(Run::git_revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] == pair[1]),
        "§16's runs have to be one build run three times, and these are {revisions:?}"
    );
    let modes: Vec<&str> = runs.iter().map(Run::execution_mode).collect();
    assert!(
        modes.windows(2).all(|pair| pair[0] == pair[1]),
        "§16's runs have to be one mode run three times, and these are {modes:?}"
    );

    // The one wall-clock stamp in the directory becomes the latest of the runs' own rather than
    // the moment of assembly: a re-assembly of the same runs then writes the same bytes, which is
    // the gate below, and no duration in these files reads a wall clock anyway (§7).
    let generated_at = runs.iter().map(Run::generated_at).max().unwrap_or(0);
    let simulation_calls: u64 = runs.iter().map(Run::calls).sum();
    let outside_runs: Vec<Value> = runs
        .iter()
        .map(|run| {
            let mut row = run.outside.clone();
            if let Some(object) = row.as_object_mut() {
                object.insert("run".to_string(), json!(run.name));
            }
            row
        })
        .collect();

    let mut evidence =
        DiagnosisEvidence::open(dir, runs[0].git_revision(), runs[0].execution_mode(), true)
            .expect("the assembly directory opens");
    let provenance: Vec<Value> = runs.iter().map(Run::provenance).collect();
    evidence.assemble_from(generated_at, provenance);
    for run in runs {
        for line in &run.lines {
            evidence
                .replay(line)
                .unwrap_or_else(|error| panic!("{}: {error}", run.name));
        }
    }
    evidence.attach_outside(outside_simulation_table_assembled(
        &outside_runs,
        simulation_calls,
    ));
    evidence.finish().expect("the assembly writes");

    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// A fresh assembly of the runs as committed, and its directory listing.
fn fresh_assembly(name: &str) -> (PathBuf, Vec<String>) {
    let dir = scratch(name);
    let names = assemble_to(&dir, &read_runs());
    (dir, names)
}

/// §19's nine names, plus the traces file and M8.2's duplicate table the writer already carries: no
/// more and no less. An extra file is an artifact nothing asked for, a missing one a question the
/// report cannot answer.
fn assert_the_listing(dir: &Path, names: &[String]) {
    let expected: Vec<String> = DIRECTORY_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        names,
        &expected,
        "{}: not the file set §19's directory is",
        dir.display()
    );
}

/// One method's row in a per-method table.
fn method_row(summary: &Value, method: &str) -> Value {
    summary["per_method"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["method"] == json!(method) && row["source"] == json!("live"))
                .cloned()
        })
        .unwrap_or_else(|| panic!("no live {method} row in a per-method table"))
}

/// §19's 「不要手写数字」 as a gate rather than a promise: the committed directory is this
/// assembly, byte for byte.
#[test]
fn the_committed_directory_is_a_byte_for_byte_assembly_of_the_runs() {
    let (fresh, names) = fresh_assembly("committed-check");
    assert_the_listing(&fresh, &names);
    let committed = post_reuse_dir();
    if refreshing() {
        for name in &names {
            std::fs::copy(fresh.join(name), committed.join(name)).unwrap_or_else(|error| {
                panic!(
                    "{}: a fresh assembly could not replace the committed file: {error}",
                    committed.join(name).display()
                )
            });
        }
        eprintln!("refreshed {POST_REUSE_DIR}");
    }
    for name in &names {
        assert_eq!(
            read_bytes(&fresh.join(name)),
            read_bytes(&committed.join(name)),
            "{}: the committed file is not what replaying the runs writes",
            committed.join(name).display()
        );
    }
}

/// Byte-reproducibility of the assembly itself, which is what the pinned stamp buys: two
/// assemblies of one set of runs cannot differ by anything a reader might mistake for a measurement.
#[test]
fn two_assemblies_of_the_same_runs_write_byte_identical_files() {
    let (first, names) = fresh_assembly("repeat-a");
    let (second, _) = fresh_assembly("repeat-b");
    assert_the_listing(&second, &names);
    for name in &names {
        assert_eq!(
            read_bytes(&first.join(name)),
            read_bytes(&second.join(name)),
            "{name} differs between two assemblies of the same runs"
        );
    }
}

/// §16: three live runs, one simulation each, every line reuse on and build-only. Anything else
/// and the tables below are not the §16 sample.
#[test]
fn three_live_runs_each_with_reuse_on_are_the_input() {
    let runs = read_runs();
    assert_eq!(
        runs.len(),
        RUNS_REQUIRED,
        "§16 asks for {RUNS_REQUIRED} live runs and `raw/` holds {}: the merged tables would be a \
         different sample than the milestone names",
        runs.len()
    );
    for run in &runs {
        assert_eq!(
            run.simulations(),
            1,
            "{}: one route run, one simulation",
            run.name
        );
        assert_eq!(run.lines.len(), 1, "{}: one line per simulation", run.name);
        assert_eq!(
            run.execution_mode(),
            "build-only",
            "{}: §16 is 0 ETH / 0 signing / 0 broadcast",
            run.name
        );
        for line in &run.lines {
            assert_eq!(
                line["source"],
                json!("live"),
                "{}: §16 is a node answering",
                run.name
            );
            assert_eq!(line["diagnosis_schema"], json!(2));
            assert_eq!(
                line["state_read_cache"]["reuse"],
                json!(true),
                "{}: this is the post-reuse directory — a line with reuse off belongs in another \
                 one, and mixing the arms here would be §6's forbidden blend",
                run.name
            );
        }
    }
}

/// §3: the baseline re-derived rather than cited — 39 calls per run in the 20/6/6/6/1 shape, and
/// three runs' worth per method in the merged table, with the percentile ranks the pooled sample
/// can support and none it cannot.
#[test]
fn the_39_call_baseline_holds_in_each_run_and_multiplies_into_the_merged_tables() {
    let runs = read_runs();
    for run in &runs {
        assert_eq!(run.calls(), BASELINE_CALLS, "{}: §3's 39", run.name);
        for line in &run.lines {
            assert_eq!(
                line["call_count"].as_u64(),
                Some(BASELINE_CALLS),
                "{}",
                run.name
            );
        }
        for (method, count) in BASELINE_PER_RUN {
            assert_eq!(
                method_row(&run.rpc_summary, method)["count"].as_u64(),
                Some(count),
                "{}: {method} is {count} call(s) in §3's baseline",
                run.name
            );
        }
    }

    let (fresh, _) = fresh_assembly("baseline-merge");
    let merged = read_json(&fresh.join(RPC_SUMMARY_FILE));
    let runs_wide = runs.len() as u64;
    let mut expected_total = 0u64;
    for (method, count) in BASELINE_PER_RUN {
        let expected = count * runs_wide;
        expected_total += expected;
        let row = method_row(&merged, method);
        assert_eq!(
            row["count"].as_u64(),
            Some(expected),
            "the merged table holds {expected} {method} call(s): {runs_wide} runs of {count}"
        );
        let distribution = &row["distribution_ns"];
        assert_eq!(distribution["samples"].as_u64(), Some(expected), "{method}");
        assert_eq!(distribution["min_ns"], row["min_duration_ns"], "{method}");
        assert_eq!(distribution["max_ns"], row["max_duration_ns"], "{method}");
        assert!(
            distribution["min_ns"].as_u64() <= distribution["max_ns"].as_u64(),
            "{method}: a minimum above its own maximum is not a measurement"
        );
        assert!(
            distribution["p50_ns"].is_number(),
            "{method}: p50 needs 2 samples and there are {expected}: {}",
            distribution["p50_ns"]
        );
        // §3 asks for median, p95 and p99. A rank the sample cannot support is `null` with its
        // minimum beside it — an absent figure, not an extrapolated one.
        for (rank, minimum) in [(90u64, 10u64), (95, 20), (99, 100)] {
            let value = &distribution[format!("p{rank}_ns")];
            let supported = expected >= minimum;
            assert_eq!(
                value.is_number(),
                supported,
                "{method}: p{rank} needs {minimum} samples, there are {expected}, so it is {} \
                 and not an estimate",
                if supported { "measured" } else { "null" }
            );
            if !supported {
                let reason = &distribution[format!("p{rank}_reason")];
                assert_eq!(
                    reason["reason"],
                    json!("insufficient_sample"),
                    "{method} p{rank}"
                );
                assert_eq!(
                    reason["minimum_samples"],
                    json!(minimum),
                    "{method} p{rank}"
                );
            }
        }
        assert!(
            row["total_duration_ns"].as_u64()
                >= Some(expected * distribution["min_ns"].as_u64().unwrap_or(0)),
            "{method}: the summed duration cannot be below every call's own minimum"
        );
    }
    assert_eq!(
        sum_per_source(&merged, "total_calls"),
        expected_total,
        "the source total is the per-method totals, not a fourth count"
    );
    assert_eq!(expected_total, BASELINE_CALLS * runs_wide);
}

/// §12 and §19's regression test in one figure: with reuse on there should be no duplicate state
/// read. If there is one, M8.3.1 has regressed — which is what §19 says this file is for.
#[test]
fn the_merged_duplicate_tally_is_the_runs_added_and_reads_zero() {
    let runs = read_runs();
    let from_runs: u64 = runs
        .iter()
        .map(|run| sum_per_source(&run.duplicates, "duplicate_state_reads"))
        .sum();
    let keyed_from_runs: u64 = runs
        .iter()
        .map(|run| sum_per_source(&run.duplicates, "total_state_reads"))
        .sum();
    let (fresh, _) = fresh_assembly("duplicates");
    let merged = read_json(&fresh.join(DUPLICATES_FILE));
    assert_eq!(
        sum_per_source(&merged, "duplicate_state_reads"),
        from_runs,
        "the merged duplicate tally is the runs' own figures, added"
    );
    assert_eq!(
        sum_per_source(&merged, "total_state_reads"),
        keyed_from_runs,
        "and so is its denominator, which is the 39 per run because every one of them keys"
    );
    assert_eq!(
        merged["per_source"].as_array().map(|rows| rows.len()),
        Some(1),
        "three runs of one source is one bucket, not three"
    );
    assert_eq!(
        merged["per_source"][0]["simulations"].as_u64(),
        Some(runs.len() as u64),
        "the bucket says how many simulations it is a tally over"
    );
    if from_runs > 0 {
        panic!(
            "{from_runs} duplicate state read(s) with reuse on: §19 reads this as an M8.3.1 \
             regression, not a finding to average into a baseline"
        );
    }
}

/// §14's half of the lifecycle sweep, pooled: the runs' rows and counts, each row still naming the
/// run whose clock classified it, and the check that no call was watched by both sinks.
#[test]
fn the_lifecycle_half_pools_rows_and_keeps_each_run_on_its_own_clock() {
    let runs = read_runs();
    let from_runs: u64 = runs
        .iter()
        .map(|run| run.outside["calls"].as_u64().unwrap_or(0))
        .sum();
    let (fresh, _) = fresh_assembly("outside");
    let outside = read_json(&fresh.join(OUTSIDE_FILE));
    assert_eq!(outside["calls"].as_u64(), Some(from_runs));
    assert_eq!(
        outside["rows"].as_array().map(Vec::len),
        Some(from_runs as usize),
        "the file lists the rows it aggregated"
    );
    assert_eq!(
        outside["assembled_from"].as_array().map(Vec::len),
        Some(runs.len()),
        "one provenance entry per run"
    );
    assert_eq!(
        outside["core_state_reads_not_included"]["simulation_calls_recorded_elsewhere"].as_u64(),
        Some(BASELINE_CALLS * runs.len() as u64),
        "the state reads are named here as being in the simulation lines, not in this file"
    );
    for row in outside["assembled_from"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let name = row["run"].as_str().unwrap_or("?");
        assert!(
            row["stage_spans"].as_array().map(Vec::len).unwrap_or(0) > 0,
            "{name}: a run with no spans of its own cannot have classified its rows"
        );
        assert_eq!(
            row["calls"].as_u64(),
            runs.iter()
                .find(|run| run.name == name)
                .and_then(|run| run.outside["calls"].as_u64()),
            "{name}: its own count, as its own file wrote it"
        );
    }
    assert!(
        outside.get("stage_spans").is_none(),
        "spans from two runs' monotonic origins are not one list to pool"
    );
    let tagged: usize = outside["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["assembled_from_run"].is_string())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(
        tagged, from_runs as usize,
        "every pooled row names the run whose stage spans classified it"
    );
    let class_calls: u64 = outside["per_class"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["calls"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    assert_eq!(
        class_calls, from_runs,
        "§14's six classes together hold every pooled row"
    );

    let bottleneck = read_json(&fresh.join(BOTTLENECK_FILE));
    let check = &bottleneck["sink_disjointness_check"];
    assert_eq!(check["lifecycle_table_present"], json!(true));
    assert_eq!(
        check["simulation_state_calls"].as_u64(),
        Some(0),
        "a call the lifecycle sink also saw would be counted in two tables (§14's rule)"
    );
    assert_eq!(check["regression"], json!(false));
}

/// §6: every storage read listed with the five fields §6 names, traceable to one simulation, and
/// the by-address aggregate a sum over exactly those rows.
#[test]
fn every_storage_read_traces_to_a_simulation_block_address_slot_and_duration() {
    let runs = read_runs();
    let rows_from_runs: u64 = runs
        .iter()
        .flat_map(|run| run.lines.iter())
        .map(|line| line["storage_read_count"].as_u64().unwrap_or(0))
        .sum();
    let (fresh, _) = fresh_assembly("storage");
    let table = read_json(&fresh.join(STORAGE_BREAKDOWN_FILE));
    assert_eq!(
        table["reads_total"].as_u64(),
        Some(rows_from_runs),
        "§6's list is the runs' storage rows, added"
    );
    let addresses = table["addresses"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        table["address_count"].as_u64(),
        Some(addresses.len() as u64),
        "the address count is the groups listed"
    );
    let grouped: u64 = addresses
        .iter()
        .map(|row| row["reads"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        grouped, rows_from_runs,
        "the by-address aggregate is a sum over the same rows, not a second count"
    );
    for address in &addresses {
        let listed: u64 = address["slots"]
            .as_array()
            .map(|slots| {
                slots
                    .iter()
                    .map(|slot| slot["reads"].as_u64().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        assert_eq!(
            listed,
            address["reads"].as_u64().unwrap_or(0),
            "an address group's slots add up to its own read count: {address}"
        );
        assert_eq!(
            address["slot_count"].as_u64(),
            address["slots"]
                .as_array()
                .map(Vec::len)
                .map(|len| len as u64),
            "and the slot count is the slots listed"
        );
        assert!(
            address["simulations"].as_array().map(Vec::len).unwrap_or(0) > 0,
            "§19 asks a row to trace to a simulation: {address}"
        );
        assert!(address["blocks"].as_array().map(Vec::len).unwrap_or(0) > 0);
        assert!(address["chain_ids"].as_array().map(Vec::len).unwrap_or(0) > 0);
        assert_eq!(
            address["semantic"].as_str(),
            Some("unknown"),
            "§7: a slot is labelled only where the code already maps it, and this build maps none"
        );
        assert!(
            table["reads_without_address"].as_u64().unwrap_or(0) == 0
                || address["address"].is_null(),
            "an unaddressable read is a row with its address null, not a row dropped"
        );
    }
    // The per-row traceability §19 names is in the lines the table was built from.
    for line in trace_lines(&fresh.join(TRACES)) {
        for row in line["storage"].as_array().cloned().unwrap_or_default() {
            for key in [
                "simulation_id",
                "block",
                "address",
                "slot",
                "duration_ns",
                "chain_id",
                "rpc_id",
                "required_by",
            ] {
                assert_ne!(
                    row[key],
                    Value::Null,
                    "§6 and §19 name {key} as a field every storage row carries: {row}"
                );
            }
        }
    }
    assert_eq!(
        table["mergeability"]["status"].as_str(),
        Some("candidate_only_not_implemented"),
        "§8: batch JSON-RPC is a candidate here and nothing more"
    );
    assert!(
        table["mergeability"]["per_address"]
            .as_array()
            .map(|rows| !rows.is_empty())
            .unwrap_or(false),
        "the candidate is stated per address, so a later stage can read which groups would have \
         been batchable and which were not"
    );
}

/// §9 and §10: the account matrix keeps address / code / balance / nonce, its kind totals add to
/// its rows, and `required_by` is a label the counts agree with rather than one inferred.
#[test]
fn the_account_matrix_keeps_four_columns_and_a_required_by_the_counts_agree_with() {
    let runs = read_runs();
    let rows_from_runs: u64 = runs
        .iter()
        .flat_map(|run| run.lines.iter())
        .map(|line| line["account_read_count"].as_u64().unwrap_or(0))
        .sum();
    let (fresh, _) = fresh_assembly("account");
    let table = read_json(&fresh.join(ACCOUNT_MATRIX_FILE));
    assert_eq!(
        table["reads_total"].as_u64(),
        Some(rows_from_runs),
        "§9's matrix is the runs' account rows, added"
    );
    let addresses = table["addresses"].as_array().cloned().unwrap_or_default();
    let mut legs = 0u64;
    for address in &addresses {
        assert!(
            address["address"].is_string(),
            "§19 names address: {address}"
        );
        for leg in ["code", "balance", "nonce"] {
            assert!(
                address[leg].is_number(),
                "§19 names {leg} as a column of the matrix: {address}"
            );
            legs += address[leg].as_u64().unwrap_or(0);
        }
    }
    assert_eq!(
        legs, rows_from_runs,
        "the three legs counted per address are the rows the matrix holds"
    );
    let by_kind: u64 = table["totals"]
        .as_object()
        .map(|kinds| {
            kinds
                .values()
                .map(|kind| kind["reads"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    assert_eq!(
        by_kind, rows_from_runs,
        "the kind totals add to the rows listed, so no row is in one and not the other"
    );
    let labels: Vec<&str> = table["required_by"]
        .as_object()
        .map(|rows| rows.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert!(
        labels
            .iter()
            .all(|label| ["simulation", "orchestration", "unknown"].contains(label)),
        "§10's three labels are the whole closed set and a fourth would be an invention: {labels:?}"
    );
    let judged: u64 = table["required_by"]
        .as_object()
        .map(|rows| rows.values().filter_map(Value::as_u64).sum())
        .unwrap_or(0);
    assert_eq!(
        judged, rows_from_runs,
        "every row got a label, and the labels are counted over the rows rather than invented \
         beside them"
    );
    assert_eq!(
        table["required_by"]["simulation"].as_u64(),
        Some(rows_from_runs),
        "§10's judgement is the wire count against the boundary's miss count: with reuse on and \
         the two agreeing, every account read here is one the simulation went to get"
    );
    assert!(
        table["same_account_all_three_legs"]["addresses"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "§9's question is whether these are the same accounts asked three things, and the file \
         has to answer it with a count rather than leave the column uncounted"
    );
}

/// §12: the idle stretches between calls, pooled over simulations. The runs' own gap tables are the
/// expected values; nothing here restates a duration.
#[test]
fn the_gap_table_pools_every_simulations_waits_and_adds_up() {
    let runs = read_runs();
    let gaps_from_runs: u64 = runs
        .iter()
        .map(|run| run.gaps["gap_count"].as_u64().unwrap_or(0))
        .sum();
    let ns_from_runs: u64 = runs
        .iter()
        .map(|run| run.gaps["gap_total_ns"].as_u64().unwrap_or(0))
        .sum();
    let (fresh, _) = fresh_assembly("gaps");
    let table = read_json(&fresh.join(RPC_GAPS_FILE));
    assert_eq!(table["simulations"].as_u64(), Some(runs.len() as u64));
    assert_eq!(
        table["simulations_with_calls"].as_u64(),
        Some(runs.len() as u64),
        "and every one of them recorded a call to be in between"
    );
    assert_eq!(
        table["gap_count"].as_u64(),
        Some(gaps_from_runs),
        "§12's wait count is the runs' own waits, added — a merged figure a reader cannot trace \
         back to the runs is a second measurement rather than a pooled one"
    );
    assert_eq!(table["gap_total_ns"].as_u64(), Some(ns_from_runs));
    for key in ["gap_min_ns", "gap_median_ns", "gap_max_ns"] {
        assert!(
            table[key].is_number(),
            "{key} is a rank the pooled sample can support"
        );
    }
    assert!(
        table["gap_min_ns"].as_u64() <= table["gap_median_ns"].as_u64(),
        "a minimum above the median is not a distribution"
    );
    assert!(
        table["gap_median_ns"].as_u64() <= table["gap_max_ns"].as_u64(),
        "a median above the maximum is not a distribution"
    );
    let listed: u64 = table["per_simulation"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["rpc_gap_count"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    assert_eq!(
        listed, gaps_from_runs,
        "the per-simulation list and the pooled count are the same waits, counted twice over"
    );
    assert!(
        table["gap_median_is_p50"]
            .as_str()
            .unwrap_or("")
            .contains("p50"),
        "§12 asks for a median, and the file has to say which rank it used rather than leave the \
         reader to assume one: {}",
        table["gap_median_is_p50"]
    );
}

/// §13 and §8, and the standing rule that an endpoint is not evidence: integers only, a provider
/// named by its digest, and no long hex word in the directory that the runs did not themselves
/// write. The whitelist is therefore the runs' own files, not a hand-picked list of shapes: a key
/// is 64 hex digits and so is a calldata, and the only difference between them is whether a run
/// measured it.
#[test]
fn the_directory_holds_no_floats_no_endpoint_url_and_no_hex_the_runs_did_not_write() {
    let runs = read_runs();
    let mut allowed: Vec<String> = Vec::new();
    for run in &runs {
        let dir = post_reuse_dir().join(RAW).join(&run.name);
        for name in DIRECTORY_FILES {
            let text = String::from_utf8(read_bytes(&dir.join(name))).unwrap_or_default();
            for word in hex_runs(&text) {
                if word.len() >= 64 && !allowed.contains(&word) {
                    allowed.push(word);
                }
            }
        }
    }
    assert!(
        !allowed.is_empty(),
        "the runs' own long hex — their storage slots and their calldata — is the whitelist"
    );

    let (fresh, names) = fresh_assembly("secrets");
    for name in &names {
        let text = String::from_utf8(read_bytes(&fresh.join(name)))
            .unwrap_or_else(|_| panic!("{name} is utf-8"));
        if name.ends_with(".json") {
            let value = serde_json::from_str::<Value>(&text).expect("a table is JSON");
            assert!(
                !has_a_float(&value),
                "{name}: §13 forbids a float in these figures, where a ratio is a numerator over \
                 a denominator"
            );
        }
        for forbidden in ["https://", "http://", "ws://", "wss://", "PRIVATE KEY"] {
            assert!(
                !text.contains(forbidden),
                "{name}: contains `{forbidden}` — an endpoint is not something an evidence file \
                 carries, and neither is a key"
            );
        }
        for word in hex_runs(&text) {
            if word.len() >= 64 {
                assert!(
                    allowed.contains(&word),
                    "{name}: a {len}-digit hex word no run wrote anywhere in its own files would \
                     have to be key material the assembly invented",
                    len = word.len()
                );
            }
        }
    }
    let outside = read_json(&fresh.join(OUTSIDE_FILE));
    let digest = outside["endpoint_id"].as_str().unwrap_or("");
    assert!(
        digest.starts_with("rpc-") && digest.len() == 20,
        "§8's provider identity is a digest and the digest is the whole of what is printed: \
         `{digest}`"
    );
    for run in &runs {
        assert_eq!(
            run.outside["endpoint_id"].as_str(),
            Some(digest),
            "{}: the assembly printed one digest for runs that went to different providers",
            run.name
        );
    }
    for line in trace_lines(&fresh.join(TRACES)) {
        assert_eq!(line["endpoint_id"], json!(digest));
    }
}

/// §19's provenance in the header of every file rather than in one file a reader has to know to
/// open, with each run named beside the figures it contributed.
#[test]
fn every_file_in_the_directory_names_the_runs_it_was_assembled_from() {
    let runs = read_runs();
    let (fresh, names) = fresh_assembly("provenance");
    for name in names.iter().filter(|name| name.ends_with(".json")) {
        let table = read_json(&fresh.join(name));
        let listed: Vec<Value> = table["assembled_from"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            listed.len(),
            runs.len(),
            "{name}: one provenance row per run"
        );
        for run in &runs {
            let row = listed
                .iter()
                .find(|row| row["run"] == json!(run.name))
                .unwrap_or_else(|| panic!("{name} does not name {}", run.name));
            assert_eq!(row["simulation_calls"].as_u64(), Some(run.calls()));
            assert_eq!(
                row["outside_simulation_calls"].as_u64(),
                run.outside["calls"].as_u64()
            );
            assert_eq!(row["simulations"].as_u64(), Some(1));
            assert_eq!(row["git_revision"].as_str(), Some(run.git_revision()));
            assert_eq!(row["execution_mode"].as_str(), Some(run.execution_mode()));
            assert_eq!(
                row["generated_at_unix_ms"].as_u64(),
                Some(run.generated_at())
            );
        }
        assert_eq!(
            table["generated_at_unix_ms"].as_u64(),
            runs.iter().map(Run::generated_at).max(),
            "{name}: the directory's stamp is the last measurement's, not the assembly's — which \
             is what lets the same runs be assembled twice to the same bytes"
        );
        assert_eq!(table["diagnosis_schema"].as_u64(), Some(2), "{name}");
        assert_eq!(table["unit"], json!("ns"), "{name}");
    }
    let readme = std::fs::read_to_string(fresh.join(README_FILE)).expect("the README is readable");
    assert!(readme.contains("## How this directory was assembled"));
    assert!(readme.contains("This is not one run's directory"));
    for run in &runs {
        assert!(
            readme.contains(&run.name),
            "the README has to name {} as a source",
            run.name
        );
    }
}

/// The replay cannot add a request and the assembly cannot lose one: the merged lines are the
/// runs' lines byte for byte, and the two halves of the lifecycle add to what the runs reported.
#[test]
fn the_assembly_adds_no_call_and_loses_none() {
    let runs = read_runs();
    let raw_lines: Vec<Value> = runs.iter().flat_map(|run| run.lines.clone()).collect();
    let (fresh, _) = fresh_assembly("no-extra-calls");
    let merged_lines = trace_lines(&fresh.join(TRACES));
    assert_eq!(merged_lines.len(), raw_lines.len());
    for (merged, raw) in merged_lines.iter().zip(&raw_lines) {
        assert_eq!(
            serde_json::to_string(merged).expect("a merged line serializes"),
            serde_json::to_string(raw).expect("a run's line serializes"),
            "a replayed line is not the line the run wrote"
        );
    }
    let calls_in_lines: u64 = raw_lines
        .iter()
        .map(|line| line["call_count"].as_u64().unwrap_or(0))
        .sum();
    let outside_from_runs: u64 = runs
        .iter()
        .map(|run| run.outside["calls"].as_u64().unwrap_or(0))
        .sum();
    let outside = read_json(&fresh.join(OUTSIDE_FILE));
    assert_eq!(
        calls_in_lines + outside["calls"].as_u64().unwrap_or(0),
        calls_in_lines + outside_from_runs,
        "the two halves of the lifecycle add to the two halves the runs reported"
    );
    let merged = read_json(&fresh.join(RPC_SUMMARY_FILE));
    assert_eq!(
        sum_per_source(&merged, "total_calls").checked_add(outside["calls"].as_u64().unwrap_or(0)),
        Some(calls_in_lines + outside_from_runs),
        "and neither half changed size on the way in: a call counted twice or dropped in the \
         assembly would move one of these sums"
    );
    assert_eq!(
        merged["diagnosis_refusals"].as_array().map(Vec::len),
        Some(0),
        "the assembly refused nothing, so it also skipped nothing it could not read"
    );
}

/// §30's rule checked where it applies rather than asserted in a comment: a float anywhere in a
/// table is a division someone did instead of stated.
fn has_a_float(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.is_f64(),
        Value::Array(values) => values.iter().any(has_a_float),
        Value::Object(entries) => entries.values().any(has_a_float),
        _ => false,
    }
}

/// Maximal runs of hex digits in a text, lowercased — the shape a key, a slot or a digest has, so
/// a scan can ask how long each one is.
fn hex_runs(text: &str) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_hexdigit() {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}
