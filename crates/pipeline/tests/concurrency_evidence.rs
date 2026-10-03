//! M8.3.3 §29 and §41: `data/evidence/m8/concurrency/`, assembled from the runs that measured it.
//!
//! §19's rule that §41 makes executable is the one M8.3.2 wrote first — 「证据必须来自实际运行。不要
//! 手写数字。」 — so this file contains no measured figure of its own. It reads the nine live route runs
//! §19 required (each in `runs/<name>/`), replays their trace lines through [`DiagnosisEvidence`] —
//! the very writer a run uses — and then asks whether the committed directory is that assembly,
//! byte for byte. §14's 「不要重新创建第二套 recorder」 is why the replay is the whole mechanism: the
//! per-arm table and the dependency map are two functions in `diagnosis.rs` fed by the same lines,
//! so no second arithmetic exists here to disagree with the first.
//!
//! ## Which gates live here and which do not
//!
//! §37's matrix splits by where a fact can be checked at all:
//!
//! - **Scheduler / dependency / cache / error rows** live in the code they grade — the unit tests in
//!   `crates/pipeline/src/diagnosis.rs` and `crates/simulation/src/acquisition.rs`, plus the
//!   fixed-block arms in `crates/simulation/tests/concurrency_abc.rs`. A test of a live directory
//!   cannot show that a failing chunk is awaited in full.
//! - **Evidence and instrumentation rows** live here, because they are facts about these nine runs:
//!   §10's baseline closure, §11/§12/§36's "concurrency actually happened", §16's "the instrument
//!   asked for nothing extra", §18's pinned block on every read, §21's sample limits, §23/§25's call
//!   and attempt counts, §30's provenance, §31's README, §34's correctness gate, §38's safety scan,
//!   §41's byte-reproducibility.
//!
//! ## What cannot be pooled, and is not
//!
//! A `started_ns` is nanoseconds since *its own run's* monotonic origin. Per-simulation figures — a
//! gap, a serial wait, an overlap — are differences inside one simulation's window, so nine runs
//! contribute nine whole simulations and no two clocks are mixed. §20 lets those runs sit on
//! different blocks, so the arms publish their own integers and nothing in this file subtracts one
//! arm from another; the controlled subtraction across bounds is the fixed-block pair, which is one
//! block and lives in `fixed-block/`.
//!
//! ## Why the README is generated rather than written
//!
//! §31 asks the README fourteen questions, most of which have a number for an answer. A hand-kept
//! README drifts from the tables beside it, and a reader cannot tell. So the file is produced by
//! [`build_readme`] out of the assembled tables and is compared with the committed one like any
//! other artifact: a hand-edited number in it fails the suite.
//!
//! ## Why the committed directory is compared rather than written
//!
//! Assembly always happens under `target/pipeline-tests/`; a test then requires the files in
//! `data/evidence/m8/concurrency/` to be byte-identical to it. `M833_CONCURRENCY_REFRESH=1` copies a
//! fresh assembly over the committed files and is the only way they change. `runs/`, `route-runs/`,
//! `fixed-block/` and `correctness-comparison.json` — the runs themselves and the fixture's output —
//! live in the same directory and nothing here writes or deletes them.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::diagnosis::{
    concurrency_summary_assembled, dependency_map_assembled, outside_simulation_table_assembled,
    DiagnosisEvidence, ACCOUNT_MATRIX_FILE, BOTTLENECK_FILE, DUPLICATES_FILE, OUTSIDE_FILE,
    README_FILE, RPC_GAPS_FILE, RPC_SUMMARY_FILE, SIMULATION_SUMMARY_FILE, STORAGE_BREAKDOWN_FILE,
    TRACES_FILE,
};

const CONCURRENCY_DIR: &str = "data/evidence/m8/concurrency";
/// §10's starting line: the milestone before this one, read from its own committed file rather
/// than restated as constants. The constants below are the same figures and are the stop rule; this
/// path is what makes them a *comparison* instead of an assertion.
const POST_REUSE_DIR: &str = "data/evidence/m8/post-reuse";

const RUNS: &str = "runs";
const ROUTE_RUNS: &str = "route-runs";
const FIXED_BLOCK: &str = "fixed-block";
const ROUTE_RUN_FILE: &str = "route-run.json";
const SIGNED_FILE: &str = "signed-transactions.jsonl";
const SUBMISSIONS_FILE: &str = "submissions.jsonl";

const CONCURRENCY_SUMMARY_FILE: &str = "concurrency-summary.json";
const DEPENDENCY_MAP_FILE: &str = "dependency-map.json";
const CORRECTNESS_FILE: &str = "correctness-comparison.json";

/// §2's arms as the task book names them. C8 is absent on purpose: §13 allows it only after C2, C4
/// and the correctness gate pass, and it was not run — which §31's README and §14's limitations both
/// have to say rather than leave a reader to infer from an empty table.
const ARMS: [(&str, u64); 3] = [("c1", 1), ("c2", 2), ("c4", 4)];
const RUNS_PER_ARM: usize = 3;
const RUNS_REQUIRED: usize = 9;

/// §3 and §10's baseline: one simulation of this route is 39 calls — 20 storage, 6 code, 6 balance,
/// 6 nonce, 1 header — and a run whose count moved has changed the request volume rather than the
/// scheduling of it.
const BASELINE_PER_RUN: [(&str, u64); 5] = [
    ("eth_getStorageAt", 20),
    ("eth_getCode", 6),
    ("eth_getBalance", 6),
    ("eth_getTransactionCount", 6),
    ("eth_getBlockByNumber", 1),
];
const BASELINE_CALLS: u64 = 39;
const BASELINE_ATTEMPTS: u64 = 39;
const BASELINE_HITS: u64 = 77;
const BASELINE_MISSES: u64 = 38;
/// The reuse boundary this experiment sits on top of, in the same shape §10 names: kind, hits,
/// misses. A concurrent path that quietly changed how often a read was answered from the
/// simulation's own cache would move one of these.
const BASELINE_CACHE: [(&str, u64, u64); 4] = [
    ("balance", 12, 6),
    ("code", 27, 6),
    ("nonce", 12, 6),
    ("storage", 26, 20),
];

/// Everything §29's directory holds that a re-assembly of the nine runs writes: the writer's ten
/// pooled files — its generic M8.2 README included, then overwritten by the §31 one — plus this
/// milestone's two tables. In listing order, because a directory read gives names sorted.
const GENERATED_FILES: [&str; 12] = [
    README_FILE,
    ACCOUNT_MATRIX_FILE,
    BOTTLENECK_FILE,
    CONCURRENCY_SUMMARY_FILE,
    DEPENDENCY_MAP_FILE,
    DUPLICATES_FILE,
    OUTSIDE_FILE,
    RPC_GAPS_FILE,
    RPC_SUMMARY_FILE,
    SIMULATION_SUMMARY_FILE,
    TRACES,
    STORAGE_BREAKDOWN_FILE,
];
/// A file §29's directory holds and no assembly writes: the fixed-block fixture's own correctness
/// record, from `crates/simulation/tests/concurrency_abc.rs`, committed beside the tables that
/// quote it in the same way `fixed-block/` and `runs/` are. The byte-for-byte gate covers the
/// twelve generated files; that record's freshness is the fixture's gate to hold, and an assembly
/// that invented it would be publishing a correctness claim no run measured.
const CARRIED_FILES: [&str; 1] = [CORRECTNESS_FILE];

/// The directory's file set: generated and carried together, sorted, which is what a listing
/// returns.
fn committed_file_set() -> Vec<String> {
    let mut names: Vec<String> = GENERATED_FILES
        .iter()
        .chain(CARRIED_FILES.iter())
        .map(|name| (*name).to_string())
        .collect();
    names.sort();
    names
}
/// The three directories §29's layout puts beside them: nine runs, their route halves, and the
/// fixed-block arms.
const COMMITTED_DIRS: [&str; 3] = [FIXED_BLOCK, ROUTE_RUNS, RUNS];
const TRACES: &str = TRACES_FILE;

/// `M833_CONCURRENCY_REFRESH` names no directory: its presence is the instruction to copy a fresh
/// assembly over the committed evidence. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M833_CONCURRENCY_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn concurrency_dir() -> PathBuf {
    workspace_root().join(CONCURRENCY_DIR)
}

/// A scratch assembly directory, emptied first because the evidence writer appends: a second
/// assembly into a directory holding a first would double every line rather than reproduce it.
fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target/pipeline-tests")
        .join(format!("m8.3.3-{name}"));
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

fn read_text(path: &Path) -> String {
    String::from_utf8(read_bytes(path)).unwrap_or_else(|_| panic!("{}: not utf-8", path.display()))
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

/// A JSONL file's whole content as line count, with the file required to exist: an evidence
/// directory that has quietly lost `signed-transactions.jsonl` would make "0 signatures" true for
/// the wrong reason.
fn jsonl_lines(path: &Path) -> usize {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// The nine run directories, in listing order. `runs/` holds runs and nothing else; a directory in
/// it without a trace file is this file's own mistake and is reported as one rather than skipped —
/// §30's 「不允许不同实验配置混入同一个 pooled record」 starts as "every run in the tree got counted".
fn run_names() -> Vec<String> {
    let root = concurrency_dir().join(RUNS);
    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    for name in &names {
        assert!(
            root.join(name).join(TRACES).exists(),
            "{CONCURRENCY_DIR}/{RUNS}/{name} has no `{TRACES}`: a directory inside the evidence \
             root that is not a run would be silently left out of the assembly"
        );
    }
    names.retain(|name| root.join(name).join(TRACES).exists());
    names.sort();
    names
}

/// The bound a run name says it was given: `c4-02` is arm 4, run 2 of that arm.
fn bound_of(name: &str) -> u64 {
    for (prefix, bound) in ARMS {
        if name.starts_with(prefix) {
            return bound;
        }
    }
    panic!("{name}: a run outside §2's arms — the name says which bound it was given")
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
    route_run: Value,
}

impl Run {
    fn read(name: &str) -> Self {
        let dir = concurrency_dir().join(RUNS).join(name);
        let at = |file: &str| dir.join(file);
        Self {
            name: name.to_string(),
            lines: trace_lines(&at(TRACES)),
            outside: read_json(&at(OUTSIDE_FILE)),
            rpc_summary: read_json(&at(RPC_SUMMARY_FILE)),
            simulation_summary: read_json(&at(SIMULATION_SUMMARY_FILE)),
            duplicates: read_json(&at(DUPLICATES_FILE)),
            gaps: read_json(&at(RPC_GAPS_FILE)),
            route_run: read_json(
                &concurrency_dir()
                    .join(ROUTE_RUNS)
                    .join(name)
                    .join(ROUTE_RUN_FILE),
            ),
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

    fn endpoint_id(&self) -> &str {
        self.outside["endpoint_id"].as_str().unwrap_or("?")
    }

    /// The line this run wrote: one simulation, so one line, and the assembly's identity fields all
    /// read it.
    fn line(&self) -> &Value {
        &self.lines[0]
    }

    fn block(&self) -> u64 {
        self.line()["block_number"].as_u64().unwrap_or(0)
    }

    fn configured(&self) -> u64 {
        self.line()["state_read_concurrency"]["configured"]
            .as_u64()
            .unwrap_or(0)
    }

    /// §30's provenance row: what the directory says about where this run's figures came from, in
    /// the words the run itself used, plus the bound it was given — the field §30 says may not be
    /// blended across a pooled record.
    fn provenance(&self) -> Value {
        json!({
            "run": self.name,
            "directory": format!("{CONCURRENCY_DIR}/{RUNS}/{}", self.name),
            "simulations": self.simulations(),
            "simulation_calls": self.calls(),
            "outside_simulation_calls": self.outside["calls"].clone(),
            "block_number": self.line()["block_number"],
            "configured_concurrency": self.configured(),
            "state_read_reuse": self.line()["state_read_cache"]["reuse"],
            "git_revision": self.git_revision(),
            "execution_mode": self.execution_mode(),
            "generated_at_unix_ms": self.generated_at(),
            "endpoint_id": self.endpoint_id(),
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

/// The arm tables this milestone asked for and M8.2's writer does not make: one row per run of
/// trace lines, grouped by the bound the run was configured with.
fn concurrency_summary(runs: &[Run]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .map(|run| {
            json!({
                "run": run.name,
                "git_revision": run.git_revision(),
                "execution_mode": run.execution_mode(),
                "endpoint_id": run.endpoint_id(),
                "generated_at_unix_ms": run.generated_at(),
                "rows": run.lines,
            })
        })
        .collect();
    concurrency_summary_assembled(&rows)
}

fn dependency_map(runs: &[Run]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .map(|run| {
            json!({
                "run": run.name,
                "git_revision": run.git_revision(),
                "execution_mode": run.execution_mode(),
                "endpoint_id": run.endpoint_id(),
                "generated_at_unix_ms": run.generated_at(),
                "lines": run.lines,
            })
        })
        .collect();
    dependency_map_assembled(&rows)
}

/// A JSON table as it is committed: two-space pretty-printing and a trailing newline, which is what
/// every other evidence file in this repository uses. serde_json without `preserve_order` sorts
/// object keys, which is why a re-assembly of the same runs is byte-reproducible.
fn write_table(path: &Path, table: &Value) {
    let mut text = serde_json::to_string_pretty(table)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.push('\n');
    write_text(path, &text);
}

/// The whole assembly: nine runs' lines replayed through the switched-on writer, §14's lifecycle
/// half pooled from the runs' own rows, and this milestone's two tables and README beside them.
fn assemble_to(dir: &Path, runs: &[Run]) -> Vec<String> {
    assert!(
        !runs.is_empty(),
        "there is nothing to assemble: {CONCURRENCY_DIR}/{RUNS} holds no run directory"
    );
    let revisions: Vec<&str> = runs.iter().map(Run::git_revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] == pair[1]),
        "§19's nine runs have to be one build run nine times, and these are {revisions:?}"
    );
    let modes: Vec<&str> = runs.iter().map(Run::execution_mode).collect();
    assert!(
        modes.windows(2).all(|pair| pair[0] == pair[1]),
        "§19's runs have to be one mode, and these are {modes:?}"
    );

    // The one wall-clock stamp in the directory becomes the latest of the runs' own rather than the
    // moment of assembly: a re-assembly of the same runs then writes the same bytes, which is the
    // gate below, and no duration in these files reads a wall clock anyway (§7).
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

    let summary = concurrency_summary(runs);
    write_table(&dir.join(CONCURRENCY_SUMMARY_FILE), &summary);
    let map = dependency_map(runs);
    write_table(&dir.join(DEPENDENCY_MAP_FILE), &map);

    let merged = read_json(&dir.join(RPC_SUMMARY_FILE));
    let correctness = read_json(&concurrency_dir().join(CORRECTNESS_FILE));
    let (signed, submissions) = route_run_counts();
    write_text(
        &dir.join(README_FILE),
        &build_readme(
            runs,
            &merged,
            &summary,
            &map,
            &correctness,
            signed,
            submissions,
        ),
    );

    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// §38's two counts, read out of the route halves of the same nine runs the tables come from.
fn route_run_counts() -> (usize, usize) {
    let root = concurrency_dir().join(ROUTE_RUNS);
    let (mut signed, mut submissions) = (0, 0);
    for name in run_names() {
        let dir = root.join(&name);
        signed += jsonl_lines(&dir.join(SIGNED_FILE));
        submissions += jsonl_lines(&dir.join(SUBMISSIONS_FILE));
    }
    (signed, submissions)
}

/// A fresh assembly of the runs as committed, and its file listing.
fn fresh_assembly(name: &str) -> (PathBuf, Vec<String>) {
    let dir = scratch(name);
    let names = assemble_to(&dir, &read_runs());
    (dir, names)
}

/// §29's generated file set — what one assembly of the nine runs writes, no more and no less. An
/// extra file is an artifact nothing asked for and a missing one a question §31 says this directory
/// has to answer.
fn assert_the_listing(dir: &Path, names: &[String]) {
    let expected: Vec<String> = GENERATED_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(
        names,
        &expected,
        "{}: not the file set one assembly of the nine runs writes",
        dir.display()
    );
}

/// The committed directory's own top-level entries, files and directories apart.
fn committed_listing() -> (Vec<String>, Vec<String>) {
    let root = concurrency_dir();
    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    (files, dirs)
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

/// A run's outside-simulation calls as a multiset, sorted so two runs can be compared as sets.
/// §16's claim covers these reads too: a bound that bought its speed by asking the node for
/// something different on the way into the simulation would show up here rather than in the 39.
fn outside_multiset(run: &Run) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = run.outside["per_method"]
        .as_array()
        .expect("a per-method list")
        .iter()
        .map(|row| {
            (
                row["method"].as_str().unwrap_or("?").to_string(),
                row["calls"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The reads an execution lane made through its own adapter, counted out of the provenance line
/// its funding snapshot carries. They are in neither the 39 nor the outside table, so the only
/// place a reader can check them is the run's own `route-run.json`.
fn lane_reads(run: &Run) -> usize {
    run.route_run["execution"]["before"]["provenance"]
        .as_str()
        .map(|text| {
            text.matches("eth_getBalance(").count() + text.matches("eth_call balanceOf(").count()
        })
        .unwrap_or(0)
}

/// A wei amount as an integer, whichever of the two forms this repository's evidence prints it in:
/// preflight writes decimal, an asset snapshot writes hex. Comparing the texts would have called
/// one balance two numbers.
fn as_wei(text: &str) -> u128 {
    match text.strip_prefix("0x") {
        Some(hex) => u128::from_str_radix(hex, 16),
        None => text.parse::<u128>(),
    }
    .unwrap_or_else(|_| panic!("unparseable wei amount {text:?}"))
}

/// A figure as the README prints it: an absent one stays the word `null`, because a table that
/// printed an absent p99 as a blank cell would read as a zero.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(values) => values.iter().map(shown).collect::<Vec<String>>().join(","),
        Value::Object(_) => "{…}".to_string(),
    }
}

/// Every field of an object printed by its own JSON name, so a reader can look the claim up in the
/// file instead of taking the sentence for it. `shown` cannot do this: an object flattened to its
/// values would read as a list of bare `true`s with no way back to which field said one.
fn named_fields(value: &Value) -> String {
    match value {
        Value::Object(entries) => entries
            .iter()
            .map(|(key, field)| format!("{}={}", key, shown(field)))
            .collect::<Vec<String>>()
            .join(", "),
        other => shown(other),
    }
}

/// One table row, in the shape every table in this README uses.
fn table_row(lines: &mut Vec<String>, cells: &[&str]) {
    lines.push(format!("| {} |", cells.join(" | ")));
}

/// §31's README, drawn entirely from the tables beside it. Every number here is read out of
/// `merged`, `summary`, `map` or `correctness`; the prose names the §-rule each section answers.
fn build_readme(
    runs: &[Run],
    merged: &Value,
    summary: &Value,
    map: &Value,
    correctness: &Value,
    signed: usize,
    submissions: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let arms = summary["per_arm"].as_array().cloned().unwrap_or_default();
    let per_simulation = summary["per_simulation"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total_calls: u64 = arms
        .iter()
        .filter_map(|arm| arm["totals"]["calls"].as_u64())
        .sum();
    let methods: Vec<String> = BASELINE_PER_RUN
        .iter()
        .map(|(method, _)| format!("{method} {}", method_row(merged, method)["count"]))
        .collect();

    lines.push("# M8.3.3 — controlled RPC parallelism experiment".to_string());
    lines.push(String::new());
    lines.push(format!(
        "{runs} live runs on one commit ({commit}), one fixed block replayed at three bounds, all of \
         them build-only against the endpoint this repository records as `{endpoint}` on chain \
         {chain}. Nothing in this directory is a production default: the concurrency bound a run \
         was given came from a command line, the build's own default is 1, and §3 forbids \
         promoting this experiment's result into one.",
        runs = runs.len(),
        commit = runs[0].git_revision(),
        endpoint = runs[0].endpoint_id(),
        chain = runs[0].line()["chain_id"],
    ));
    lines.push(String::new());
    lines.push(format!(
        "```text\n{}\n```",
        "在不改变 state、simulation、risk、execution 语义的前提下，受控提高 state RPC 并发度，\
         是否可以显著降低 simulation latency？"
    ));
    lines.push(String::new());

    // 1 and 2 — what the directory is, and the line it starts from.
    lines.push("## 1. What this directory is for".to_string());
    lines.push(
        "An experiment, not an optimization: three concurrency bounds measured against each other \
         on the same route, with the semantics of the run held still by `fixed-block/` (§1). The \
         tables are the measurements; this file is the index. The two figures §15 insists on \
         staying apart are in every table — `configured_concurrency` is what a run was told and \
         `observed_max_concurrency` is what its scheduler saw outstanding."
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## 2. Where the baseline comes from".to_string());
    lines.push(format!(
        "§10 makes M8.3.2 the starting line and the stop rule: `{POST_REUSE_DIR}/` measured this \
         route after the reuse boundary went in, and a C1 run here has to match it field for field \
         before any louder arm means anything. The three post-reuse simulations and the {arm_count} \
         C1 runs in this directory all report {calls} calls, {dup} duplicate state reads, a wire \
         max concurrency of {wire}, `{serial}` and an overlap of {overlap} ns, with the cache's \
         {hits} hits and {misses} misses beside them. So the rule was checked and did not fire.",
        arm_count = runs
            .iter()
            .filter(|run| run.configured() == 1)
            .count(),
        calls = BASELINE_CALLS,
        dup = 0,
        wire = 1,
        serial = "serial",
        overlap = 0,
        hits = BASELINE_HITS,
        misses = BASELINE_MISSES,
    ));
    lines.push(String::new());

    // 3 — the arms, and the runs that did not happen.
    lines.push("## 3. The arms".to_string());
    lines.push("| arm | bound asked for | runs | simulations | blocks | scheduler peaks | wire peaks | simulations with an overlap on the wire | never above its bound |".to_string());
    lines.push("|---|---|---|---|---|---|---|---|---|".to_string());
    for arm in &arms {
        table_row(
            &mut lines,
            &[
                &format!("C{}", shown(&arm["configured_concurrency"])),
                &shown(&arm["configured_concurrency"]),
                &shown(&arm["runs"]),
                &shown(&arm["simulations"]),
                &shown(&arm["blocks"]),
                &shown(&arm["observed_max_concurrency"]["per_simulation"]),
                &shown(&arm["wire_max_concurrency"]["per_simulation"]),
                &shown(&arm["simulations_with_an_overlap_on_the_wire"]),
                &shown(&arm["observed_max_concurrency"]["never_above_configured"]),
            ],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "C8 was not run. §13 allows it only after C2, C4 and the correctness gate pass, and it was \
         not asked for; nothing in this directory is evidence about eight concurrent reads. The \
         sentence printed once for the whole directory and repeated on every line is: {}",
        summary["configured_is_never_observed"]
            .as_str()
            .unwrap_or("")
    ));
    lines.push(String::new());

    // 4 and 5 — the run list, each run's block, and its RPC count.
    lines.push("## 4. The runs, and what each one measured".to_string());
    lines.push("| run | bound | block | calls | duplicate state reads | attempts | retried | cache hits | cache misses | scheduler peak | wire peak | wire | simulation duration ns | RPC union ns | RPC sum ns | RPC overlap ns |".to_string());
    lines.push("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|".to_string());
    for one in &per_simulation {
        table_row(
            &mut lines,
            &[
                shown(&one["run"]).as_str(),
                &shown(&one["configured_concurrency"]),
                &shown(&one["block_number"]),
                &shown(&one["calls"]),
                &shown(&one["duplicate_state_reads"]),
                &shown(&one["attempts"]),
                &shown(&one["retried_calls"]),
                &shown(&one["cache_hits"]),
                &shown(&one["cache_misses"]),
                &shown(&one["observed_max_concurrency"]),
                &shown(&one["wire_max_concurrency"]),
                &shown(&one["serial_or_overlap_on_the_wire"]),
                &shown(&one["simulation_duration_ns"]),
                &shown(&one["rpc_union_duration_ns"]),
                &shown(&one["rpc_sum_duration_ns"]),
                &shown(&one["rpc_overlap_duration_ns"]),
            ],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "§30's provenance, per run and in the same order: {}.",
        runs.iter()
            .map(|run| format!(
                "`{}` — block {}, bound {}, reuse {}, mode `{}`, commit {}, endpoint {}",
                run.name,
                run.block(),
                run.configured(),
                shown(&run.line()["state_read_cache"]["reuse"]),
                run.execution_mode(),
                run.git_revision(),
                run.endpoint_id(),
            ))
            .collect::<Vec<String>>()
            .join("; ")
    ));
    lines.push(String::new());
    lines.push(format!(
        "The route half of each run — the pipeline's own `route-run.json`, `metrics.json`, the \
         latency session and the execution lane's empty files — is in `{ROUTE_RUNS}/<name>/`, named \
         for the same run. Every run's `route-run.json` records the session id its latency \
         directory was opened under."
    ));
    lines.push(String::new());

    // 6 — RPC count, and what did not count.
    lines.push("## 5. RPC count per run (§23, §25, §16)".to_string());
    let funded: Vec<&Run> = runs
        .iter()
        .filter(|run| !run.route_run["execution"]["before"].is_null())
        .collect();
    let outside_methods: Vec<String> = outside_multiset(&runs[0])
        .iter()
        .map(|(method, count)| format!("{method} {count}"))
        .collect();
    let outside_total: u64 = runs
        .iter()
        .map(|run| run.outside["calls"].as_u64().unwrap_or(0))
        .sum();
    let lane_total: usize = funded.iter().map(|run| lane_reads(run)).sum();
    let wallet_runs: Vec<&Run> = runs
        .iter()
        .filter(|run| {
            run.route_run["preflight"]["input_asset"]["source"]
                .as_str()
                .unwrap_or("")
                .contains("eth_getBalance(")
        })
        .collect();
    lines.push(format!(
        "Every one of the {runs} runs made {calls} calls inside its simulation and {total} across \
         the directory — {methods}. The multiset is unchanged between arms, which is the shape §23 \
         asks for: a bound that bought speed by asking for less would not be a scheduling \
         experiment. `attempts` is {attempts} per run with {retried} retried, so no call was made \
         twice anywhere in this sample, and a retry would have been one record with two attempts \
         rather than two records (crates/chain/tests/rpc_trace_safety.rs).",
        runs = runs.len(),
        calls = BASELINE_CALLS,
        total = total_calls,
        methods = methods.join(", "),
        attempts = BASELINE_ATTEMPTS,
        retried = 0,
    ));
    lines.push(String::new());
    lines.push(format!(
        "Two other sets of reads sit beside that count, and this directory neither folds them into \
         it nor drops them to a zero. The first is measured, just not here: the {outside_each} \
         reads each run's route lifecycle made outside its simulation — {outside_methods}, \
         {outside_total} across the directory, the same multiset in every arm — held in \
         `{OUTSIDE_FILE}`, which also writes down that the simulation's {core} belong to the \
         other file so that no call is in both lists. The second is measured by nothing in this \
         directory, and `{OUTSIDE_FILE}`'s `unreachable_reads` names its three sources: \
         `eth_chainId`, asked before an adapter exists for a sink to attach to; the WebSocket \
         transport, which a route run's HTTP lifecycle never reaches; and the execution lane's own \
         second adapter — head, nonce, balance, fee parameters, receipts, submissions. That last \
         one is what read the wallet: an `eth_getBalance` line is quoted in the preflight record \
         of {wallet_read} of the {all} runs, and the {funded_n} that reached `status = built` \
         each quoted {lane_each} reads of its funding snapshot besides, {lane_total} in all. Those \
         {quoted} quoted reads appear in no table above, and whatever else that untraced socket \
         asked is not even quoted — §12 carries both as limitations, not as zeros.",
        core = BASELINE_CALLS,
        outside_each = runs[0].outside["calls"].as_u64().unwrap_or(0),
        outside_methods = outside_methods.join(", "),
        outside_total = outside_total,
        all = runs.len(),
        wallet_read = wallet_runs.len(),
        funded_n = funded.len(),
        lane_each = funded.first().map_or(0, |run| lane_reads(run)),
        lane_total = lane_total,
        quoted = wallet_runs.len() + lane_total,
    ));
    lines.push(String::new());

    // 7 and 8 — concurrency and duration per arm.
    lines.push("## 6. Observed max concurrency, and the time beside it".to_string());
    lines.push("| arm | simulation duration min / p50 / max ns | RPC union p50 ns | RPC sum p50 ns | RPC overlap p50 ns | serial wait p50 ns | gap p50 ns |".to_string());
    lines.push("|---|---|---|---|---|---|---|".to_string());
    for arm in &arms {
        let duration = &arm["simulation_duration_ns"];
        table_row(
            &mut lines,
            &[
                &format!("C{}", shown(&arm["configured_concurrency"])),
                &format!(
                    "{} / {} / {}",
                    shown(&duration["min_ns"]),
                    shown(&duration["p50_ns"]),
                    shown(&duration["max_ns"])
                ),
                &shown(&arm["rpc_union_duration_ns"]["p50_ns"]),
                &shown(&arm["rpc_sum_duration_ns"]["p50_ns"]),
                &shown(&arm["rpc_overlap_duration_ns"]["p50_ns"]),
                &shown(&arm["serial_wait_duration_ns"]["p50_ns"]),
                &shown(&arm["rpc_gap_duration_ns"]["p50_ns"]),
            ],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "Each arm has {RUNS_PER_ARM} samples, so `p50_ns` is a measured median and `p90_ns`, \
         `p95_ns`, `p99_ns` are `null` with an `insufficient_sample` reason and its minimum beside \
         them (§21). Nothing here subtracts one arm from another, and the table says why in its own \
         words: {note}",
        note = summary["arms_are_not_subtracted"]
            .as_str()
            .unwrap_or(""),
    ));
    lines.push(String::new());

    // 9 — correctness.
    lines.push("## 7. Correctness (§17, §34)".to_string());
    lines.push(format!(
        "`{CORRECTNESS_FILE}` compares the three fixed-block arms at block {} on one commit ({}), \
         and the answer is that they are the same run: {} fields compared, of which {} are \
         `identical_across_bounds`, a whole-result fingerprint identical at `{}`, and every gate \
         in it true ({}). The comparison is field by field — outcome, revert, gas used, gas charge, \
         logs, return data, net profit, state changes, plan summary, slippage, measurements and \
         the rest — and not a profit-only one.",
        shown(&correctness["block"]),
        shown(&correctness["commit"]),
        correctness["fields"].as_object().map(|m| m.len()).unwrap_or(0),
        correctness["fields"]
            .as_object()
            .map(|fields| {
                fields
                    .values()
                    .filter(|entry| entry["identical_across_bounds"] == json!(true))
                    .count()
            })
            .unwrap_or(0),
        correctness["whole_result"]["fingerprint"],
        named_fields(&correctness["gates"]),
    ));
    lines.push(String::new());
    lines.push(format!(
        "Its own witnesses are in `fixed-block/{{c1,c2,c4}}.json`: bounds {}, {} and {}, run walls \
         {} ns, {} ns and {} ns, with wire overlap {} ns, {} ns and {} ns on the same block. A \
         bound of 1 is serial there and `gates.every_bound_serial_at_1` is the field that says so.",
        correctness["concurrency"][0]["bound"],
        correctness["concurrency"][1]["bound"],
        correctness["concurrency"][2]["bound"],
        read_json(&concurrency_dir().join(FIXED_BLOCK).join("c1.json"))["run_wall_ns"],
        read_json(&concurrency_dir().join(FIXED_BLOCK).join("c2.json"))["run_wall_ns"],
        read_json(&concurrency_dir().join(FIXED_BLOCK).join("c4.json"))["run_wall_ns"],
        correctness["concurrency"][0]["witnesses"]["wire_overlap_duration_ns"],
        correctness["concurrency"][1]["witnesses"]["wire_overlap_duration_ns"],
        correctness["concurrency"][2]["witnesses"]["wire_overlap_duration_ns"],
    ));
    lines.push(String::new());
    // §7's own arithmetic, said out loud before a reader compares it with §5. Two figures called
    // 39 come from two definitions here, and the fixture's is the one that leaves the header out.
    let fixture = read_json(&concurrency_dir().join(FIXED_BLOCK).join("c1.json"));
    let asked = |method: &str| -> u64 {
        fixture["method_counts"][method]["requests"]
            .as_u64()
            .unwrap_or(0)
    };
    let fixture_state_reads = asked("eth_getStorageAt")
        + asked("eth_getCode")
        + asked("eth_getBalance")
        + asked("eth_getTransactionCount");
    let live = runs
        .first()
        .expect("at least one run to quote a live method count from");
    let live_count = |method: &str| -> u64 {
        live.rpc_summary["per_method"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["method"] == json!(method))
                    .and_then(|row| row["count"].as_u64())
            })
            .unwrap_or(0)
    };
    lines.push(format!(
        "The fixture's call surface is its own, and it is not §5's: at block {} one fixture arm \
         makes {} requests — {} `eth_getStorageAt`, {} `eth_getCode`, {} `eth_getBalance`, {} \
         `eth_getTransactionCount`, {} `eth_getBlockByNumber` — where one live run makes {} ({} / \
         {} / {} / {} / {} in the same order). The equality a reader will notice is between two \
         different sets: `call_counts.state_reads_per_bound` is the four chain-state methods with \
         the header left out ({}), while §5's {} is every call the simulation's sink saw, the \
         header included. The fixture is one route instance replayed on a historical block, and no \
         figure in §5 depends on it.",
        shown(&correctness["block"]),
        fixture["rpc_count"],
        asked("eth_getStorageAt"),
        asked("eth_getCode"),
        asked("eth_getBalance"),
        asked("eth_getTransactionCount"),
        asked("eth_getBlockByNumber"),
        BASELINE_CALLS,
        live_count("eth_getStorageAt"),
        live_count("eth_getCode"),
        live_count("eth_getBalance"),
        live_count("eth_getTransactionCount"),
        live_count("eth_getBlockByNumber"),
        fixture_state_reads,
        BASELINE_CALLS,
    ));
    lines.push(String::new());

    // 10, 11, 12 — the three safety questions §31 asks by name.
    lines.push(
        "## 8. Did this run sign anything, broadcast anything, spend any ETH? (§19, §38)"
            .to_string(),
    );
    // §38's control for "the ETH never moved" is every native balance the directory quotes, from
    // both places a run quotes one. The two are not the same text — preflight prints decimal wei,
    // the funding snapshot prints a hex word — so they are normalised to one integer before being
    // compared, and printed as both forms so a reader can check either against its own file.
    let mut balances: Vec<u128> = Vec::new();
    for run in runs {
        for form in [
            run.route_run["preflight"]["input_asset"]["available_wei"].as_str(),
            run.route_run["execution"]["before"]["native_wei"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            balances.push(as_wei(form));
        }
    }
    balances.sort_unstable();
    balances.dedup();
    let hex_form = format!("{:#x}", balances.first().copied().unwrap_or(0));
    let balance = if balances.is_empty() {
        "No run read the wallet it would have funded.".to_string()
    } else {
        format!(
            "Every run read the wallet it would have funded, and read it twice where it got far \
             enough to build a sequence: {wallet_read} preflight input-asset checks, plus one \
             native read in each of the {funded_n} funding snapshots of {who}. All {reads} reads \
             returned the same balance — `{hex}` = {decimal} wei — each of them an \
             `eth_getBalance`, with `eth_call balanceOf` on the route's two tokens beside it in a \
             snapshot: a read is not a spend, and the field a spend would move does not move \
             across any of them.",
            wallet_read = runs.len(),
            funded_n = funded.len(),
            reads = runs.len() + funded.len(),
            who = funded
                .iter()
                .map(|run| format!("`{}`", run.name))
                .collect::<Vec<String>>()
                .join(", "),
            hex = &hex_form,
            decimal = balances[0],
        )
    };
    let stopped_before_built = runs.len() - funded.len();
    lines.push(format!(
        "No, no, and no. `{SIGNED_FILE}` holds {signed} lines across the {runs} route runs and \
         `{SUBMISSIONS_FILE}` holds {submissions}. The submission method a broadcast would use \
         appears in no recorded call and in no file of this directory. Every run is \
         `execution_mode = {mode}`, every `route-run.json` says `successful_real_arbitrage = \
         {arb}`, and every execution record has `completed = false`, an empty transaction list, \
         `after = null`, `cost = null` and an empty delta audit — so the asset difference that \
         would show ETH moving is not merely zero, it was never taken. {balance} {other} runs \
         stopped one stage earlier, at the fee check that runs before any transaction is built: \
         the base fee read at their block came back above the ceiling the plan was priced \
         against, which is a market verdict about this route at this second and says nothing \
         about how long the state reads took — all {runs} of them ran their simulation, and that \
         is what §19 asks of a run. A signing key enters this build through one door, the \
         `GIWA_EXECUTION_PRIVATE_KEY` environment variable, and §38's scan is the one the \
         directory can carry: no long hex word appears in these files that a run did not write \
         into its own evidence first.",
        signed = signed,
        submissions = submissions,
        runs = runs.len(),
        mode = runs[0].execution_mode(),
        arb = shown(&runs[0].route_run["successful_real_arbitrage"]),
        balance = balance,
        other = stopped_before_built,
    ));
    lines.push(String::new());

    // 13 — how to regenerate.
    lines.push("## 9. How to regenerate this directory".to_string());
    lines.push(
        r#"```bash
# 1. the build, with the recipe this repository needs on this machine
CC=clang CXX=clang++ CXXFLAGS="-include cstdint" GIT_REVISION=$(git rev-parse HEAD) \
  cargo build --bin evm-mev-bot

# 2. the fixed-block arms (§17) and their comparison, on the recorded dump at block 37191169
M833_ABC_EVIDENCE=data/evidence/m8/concurrency \
  cargo test -p evm-simulation --test concurrency_abc

# 3. one live run of one arm (§19): three per bound, bound 1 then 2 then 4. `GIWA_RPC_URL` holds
#    the endpoint; the run opens a session directory under each root, which is then renamed to the
#    arm's run name below.
./target/debug/evm-mev-bot arbitrage \
  --rpc-url "$GIWA_RPC_URL" --execution-mode build-only \
  --sender <the test sender> --input-token 0x4200000000000000000000000000000000000006 \
  --candidate-mid <the router> --candidate-pool <pool A> --candidate-pool <pool B> \
  --input-wei 100000000000000 --fee-num 997 --fee-den 1000 \
  --fee-evidence data/evidence/m7/candidate-fee-measurement.json \
  --market real-market \
  --market-evidence "reserves and blockTimestampLast read at the pinned live head by this run" \
  --latency-trace --rpc-trace --diagnose-state-acquisition \
  --state-read-concurrency <1 | 2 | 4> \
  --evidence-dir data/evidence/m8/concurrency/route-runs \
  --rpc-output data/evidence/m8/concurrency/runs

# 4. the pooled tables, this README, and the byte-for-byte check
M833_CONCURRENCY_REFRESH=1 cargo test -p evm-pipeline --test concurrency_evidence
cargo test -p evm-pipeline --test concurrency_evidence
```

Step 4 is the only thing that writes the twelve files in this directory, and its second command is
what fails if any of them was edited by hand. `GIWA_RPC_URL` is set in the environment rather than
repeated here: an endpoint is not evidence, and the directory names it as a digest, printed under
this block. Step 3's `<the test sender>` and `<the router>` are the sender and router this route
uses, named in every run's `route-run.json`."#
            .to_string(),
    );
    lines.push(String::new());
    lines.push(format!(
        "The endpoint those runs went to is recorded as `{}` — a digest, not a URL.",
        runs[0].endpoint_id()
    ));
    lines.push(String::new());

    // 14 — provenance, already printed per run and per file.
    lines.push("## 10. Provenance".to_string());
    lines.push(format!(
        "One git revision ({revision}) and one execution mode ({mode}) across all {count} runs; an \
         `assembled_from` block naming every run and the figures it contributed is in the header of \
         each pooled table, including the two this milestone added. \
         `{CONCURRENCY_SUMMARY_FILE}` groups by the bound a run was configured with and counts runs \
         whose line carries no dispatch report separately ({unreported}, of {count}); \
         `{DEPENDENCY_MAP_FILE}` records the dependency each state read has and what the runs did \
         with it.",
        revision = runs[0].git_revision(),
        mode = runs[0].execution_mode(),
        count = runs.len(),
        unreported = summary["simulations_without_a_dispatch_report"],
    ));
    lines.push(String::new());

    // §18's rule and the map's verdict, in the README rather than only in the table.
    lines.push("## 11. Which block every read was taken at (§18)".to_string());
    lines.push(format!(
        "{} of the {runs} simulations carry state reads, over {} state calls; {} of those calls \
         name a pinned chain and block in both their outgoing parameter and their dedup key, and {} \
         name this run's own block in both. The block parameters ever seen on the wire are {}. \
         A `latest`, `pending` or absent tag would appear in that last list as text rather than be \
         argued away.",
        map["block_identity"]["simulations_with_state_calls"],
        map["block_identity"]["state_calls_in_those_simulations"],
        map["block_identity"]["calls_naming_a_pinned_chain_and_block"],
        map["block_identity"]["every_state_key_named_the_run_block"],
        shown(&map["block_identity"]["wire_block_fields_seen"]),
        runs = runs.len(),
    ));
    lines.push(String::new());
    lines.push("## 12. What this directory does not say (§20, §35)".to_string());
    lines.push(format!(
        "The nine live runs sit on nine different blocks, so a difference between two arms' \
         medians is as much a difference between blocks as between bounds — the same-block \
         comparison is `fixed-block/`, and it is the only pair of walls in this directory that \
         subtracts cleanly. Concurrency here is simulation-local and bounded: it says nothing \
         about batch JSON-RPC, multicall, a connection pool, a prefetch, or a cache shared across \
         simulations, none of which this build has. The RPC surface is wider than the two tables \
         that count it: the execution lane opens its own sink-less adapter, and `unreachable_reads` \
         lists what can go through it unseen — head, nonce, balance, fee parameters, receipts, \
         submissions — of which only the {quoted} reads the {runs} `route-run.json` files quote \
         have a line anywhere here. So 「39 + {outside_each}」 is a run's watched surface, not its \
         total. And an arm that got faster on a live \
         block is not a production default: §3's 「尤其禁止：把本轮实验结果直接升级成默认生产并发\
         实现」 is why the CLI's bound stays 1 when the flag is absent.",
        quoted = wallet_runs.len() + lane_total,
        runs = runs.len(),
        outside_each = runs[0].outside["calls"].as_u64().unwrap_or(0),
    ));
    lines.push(String::new());
    lines.join("\n")
}

/// §19 and §30: the nine runs are three arms of one build, each with reuse on, each build-only,
/// each source `live`, and each named for the bound it was given.
#[test]
fn nine_live_runs_of_one_build_are_the_input() {
    let runs = read_runs();
    assert_eq!(
        runs.len(),
        RUNS_REQUIRED,
        "§19 asks for {RUNS_REQUIRED} live runs ({RUNS_PER_ARM} per arm over the bounds {}) and \
         `{RUNS}/` holds {}: the arms would be a different sample than the milestone names",
        ARMS.iter()
            .map(|(_, bound)| bound.to_string())
            .collect::<Vec<String>>()
            .join("/"),
        runs.len()
    );
    let revisions: Vec<&str> = runs.iter().map(Run::git_revision).collect();
    let one_revision = revisions[0];
    assert!(
        revisions.iter().all(|value| *value == one_revision),
        "§30's provenance: one build, and this is {revisions:?}"
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
            "{}: §19 is 0 ETH / 0 signing / 0 broadcast",
            run.name
        );
        let line = run.line();
        assert_eq!(
            line["source"],
            json!("live"),
            "{}: §19 is a node answering",
            run.name
        );
        assert_eq!(line["diagnosis_schema"], json!(2));
        assert_eq!(
            line["state_read_cache"]["reuse"],
            json!(true),
            "{}: §19 requires reuse on in every arm — an arm with reuse off is a different \
             experiment and belongs in another directory",
            run.name
        );
        assert_eq!(
            run.route_run["state_read_reuse"],
            json!(true),
            "{}: the run's own record says the same thing",
            run.name
        );
        // §30: the name, the line and the route record agree on the bound. Three sources for one
        // fact, because a mislabelled directory is the cheapest way to blend two arms.
        let bound = bound_of(&run.name);
        assert_eq!(run.configured(), bound, "{}: §15's configured", run.name);
        assert_eq!(
            run.route_run["state_read_concurrency"],
            json!(bound),
            "{}: the run's own record names the same bound",
            run.name
        );
        assert_eq!(run.route_run["mode"], json!("build-only"), "{}", run.name);
        assert_eq!(
            run.route_run["pinned_block"], line["block_number"],
            "{}: the block the pipeline pinned is the block the state reads name",
            run.name
        );
    }
    for (_, bound) in ARMS {
        let in_arm = runs.iter().filter(|run| run.configured() == bound).count();
        assert_eq!(
            in_arm, RUNS_PER_ARM,
            "bound {bound} has {in_arm} runs and §19 requires {RUNS_PER_ARM}"
        );
    }
    // §20's runs are on different blocks; an identical block across a whole arm would mean the
    // runs were not what they are named for.
    let blocks: Vec<u64> = runs.iter().map(Run::block).collect();
    let distinct: std::collections::BTreeSet<u64> = blocks.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        runs.len(),
        "the nine runs sit on {blocks:?} — §20 expects one block per run, and a repeated block \
         would mean two arms shared a head"
    );
}

/// §10, and the stop rule it writes: 「如果 concurrency=1 都无法复现 baseline：停止，不继续 C2/C4」.
/// The baseline is read out of M8.3.2's own committed traces rather than quoted, so this compares
/// two measurements instead of checking one against a constant someone typed.
#[test]
fn concurrency_one_reproduces_the_baseline_this_experiment_starts_from() {
    let before_source = trace_lines(&workspace_root().join(POST_REUSE_DIR).join(TRACES));
    let before: Vec<&Value> = before_source.iter().collect();
    assert!(
        !before.is_empty(),
        "{POST_REUSE_DIR}/{TRACES} is §10's starting line and holds nothing"
    );
    // §10's reproduction, in the fields that are semantics rather than timing: how many calls the
    // route made, whether any of them was a duplicate, how many were outstanding at once, what the
    // sweep called that, and how long they overlapped. The two duration figures a serial run also
    // carries (union, sum) are compared *within* a run, never across runs — the two sides sit on
    // different blocks, and §20 says a live block is not a constant, so requiring M8.3.2's wall
    // time here would be asking for a number no second run can produce.
    let baseline = |line: &Value| -> (u64, u64, u64, String, u64) {
        let rpc = &line["rpc"];
        (
            rpc["total_calls"].as_u64().unwrap_or(0),
            line["duplicates"]["duplicate_state_reads"]
                .as_u64()
                .unwrap_or(0),
            rpc["max_concurrency"].as_u64().unwrap_or(0),
            rpc["serial_or_overlap"]
                .as_str()
                .unwrap_or("<absent>")
                .to_string(),
            rpc["rpc_overlap_duration_ns"].as_u64().unwrap_or(0),
        )
    };
    // And the arithmetic a serial run owes: the wire was busy with one call at a time, so the
    // union of the call windows is their sum.
    let union_equals_sum = |line: &Value, name: &str| {
        let rpc = &line["rpc"];
        let (union, sum) = (
            rpc["rpc_union_duration_ns"].as_u64().unwrap_or(0),
            rpc["rpc_sum_duration_ns"].as_u64().unwrap_or(0),
        );
        assert_eq!(
            union, sum,
            "{name}: a serial run's union and sum are the same number, and these are {union} and \
             {sum}"
        );
    };
    for line in &before {
        let one = baseline(line);
        assert_eq!(
            one,
            (BASELINE_CALLS, 0, 1, "serial".to_string(), 0),
            "M8.3.2's own baseline is {} at block {}, and the constants this gate carries are \
             {BASELINE_CALLS} / 0 / 1 / serial / 0 — the stop rule and the starting line disagree",
            one.0,
            line["block_number"]
        );
        union_equals_sum(line, "M8.3.2's baseline");
    }

    let runs = read_runs();
    let c1: Vec<&Run> = runs.iter().filter(|run| run.configured() == 1).collect();
    assert_eq!(c1.len(), RUNS_PER_ARM);
    for run in c1 {
        let here = baseline(run.line());
        let theirs = baseline(before[0]);
        assert_eq!(
            here, theirs,
            "{}: §10 says a bound of 1 has to reproduce the post-reuse baseline exactly — this run \
             reads {here:?} and M8.3.2 reads {theirs:?}. This is the stop condition: C2 and C4 \
             cannot be read until it holds.",
            run.name
        );
        union_equals_sum(run.line(), &run.name);
        let cache = &run.line()["state_read_cache"];
        assert_eq!(
            (cache["cache_hits"].as_u64(), cache["cache_misses"].as_u64()),
            (Some(BASELINE_HITS), Some(BASELINE_MISSES)),
            "{}: the reuse boundary under this experiment is the one M8.3.2 measured, so its \
             hits and misses are part of the baseline",
            run.name
        );
        for (kind, hits, misses) in BASELINE_CACHE {
            assert_eq!(
                (cache[kind]["hits"].as_u64(), cache[kind]["misses"].as_u64()),
                (Some(hits), Some(misses)),
                "{}: {kind} reuse is {hits} hits over {misses} misses in the baseline",
                run.name
            );
        }
        // §10 names a fourth figure the louder arms are graded against: at bound 1 the scheduler
        // held a peak of 1 and ran serially.
        let report = &run.line()["state_read_concurrency"];
        assert_eq!(report["observed_peak"], json!(1), "{}", run.name);
        assert_eq!(report["serial"], json!(true), "{}", run.name);
    }
}

/// §11, §12, §15 and §36 together: a louder arm is only one if the trace says two or four reads
/// were actually outstanding and the wire was busy with them — in every run of the arm, not one.
///
/// The per-simulation rows carry the figures; an arm's stats table cannot answer "did all three
/// overlap", because a median hides the run that did not.
#[test]
fn the_louder_arms_put_reads_on_the_wire_together() {
    let summary = concurrency_summary(&read_runs());
    let arms = summary["per_arm"].as_array().expect("one arm per bound");
    assert_eq!(
        arms.len(),
        ARMS.len(),
        "one arm for each of §2's three bounds"
    );
    let per_simulation = summary["per_simulation"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for arm in arms {
        let configured = arm["configured_concurrency"].as_u64().unwrap_or(0);
        if configured == 1 {
            continue;
        }
        let rows: Vec<&Value> = per_simulation
            .iter()
            .filter(|row| row["configured_concurrency"].as_u64() == Some(configured))
            .collect();
        assert_eq!(
            rows.len(),
            RUNS_PER_ARM,
            "bound {configured}: its arm holds {} of the {} runs",
            rows.len(),
            per_simulation.len()
        );
        for row in &rows {
            let peak = row["wire_max_concurrency"].as_u64().unwrap_or(0);
            assert_eq!(
                peak, configured,
                "{}: the wire reached {peak} concurrent reads against a bound of {configured} — \
                 §15 says a run that never went above 1 cannot be called a C{configured} \
                 experiment, whatever its flag said",
                row["run"]
            );
            let overlap = row["rpc_overlap_duration_ns"].as_u64().unwrap_or(0);
            assert!(
                overlap > 0,
                "{}: bound {configured} with an overlap of {overlap} ns. §12 files that as \
                 `status = experiment_failed`, and §44 forbids reading it as 并发没有收益 — the \
                 experiment did not achieve concurrency, which is a different sentence",
                row["run"]
            );
            assert_eq!(
                row["serial_or_overlap_on_the_wire"],
                json!("overlap"),
                "{}: the overlap duration and the sweep's own label disagree",
                row["run"]
            );
            let union = row["rpc_union_duration_ns"].as_u64().unwrap_or(0);
            let sum = row["rpc_sum_duration_ns"].as_u64().unwrap_or(0);
            assert!(
                union < sum,
                "{}: union {union} is not below sum {sum} — §36's second half of \"real \
                 concurrency\", the same fact as the overlap in other words",
                row["run"]
            );
            assert_eq!(
                row["observed_max_concurrency"].as_u64(),
                Some(configured),
                "{}: the scheduler's own bracket disagrees with the wire's",
                row["run"]
            );
        }
        assert_eq!(
            arm["simulations_with_an_overlap_on_the_wire"].as_u64(),
            arm["simulations"].as_u64(),
            "bound {configured}: every row above overlapped and the arm's count says otherwise"
        );
        assert!(
            arm["observed_max_concurrency"]["never_above_configured"]
                .as_bool()
                .unwrap_or(false),
            "bound {configured}: a simulation held more reads outstanding than it was handed, and \
             §11's `max_concurrency <= {configured}` is about exactly that"
        );
        assert_eq!(
            arm["wire_max_concurrency"]["min"].as_u64(),
            Some(configured),
            "bound {configured}: its lowest run came under the bound the others reached"
        );
    }
    // The bound of 1 is in the same table and has to say it stayed serial.
    let serial: Vec<&Value> = per_simulation
        .iter()
        .filter(|row| row["configured_concurrency"].as_u64() == Some(1))
        .collect();
    for row in &serial {
        assert_eq!(
            row["rpc_overlap_duration_ns"].as_u64(),
            Some(0),
            "{}",
            row["run"]
        );
        assert_eq!(
            row["rpc_union_duration_ns"], row["rpc_sum_duration_ns"],
            "{}: §10's serial shape",
            row["run"]
        );
    }
}

/// §21: per-arm count, min, median, max and the two tail ranks — and a rank the sample cannot
/// support published as absent, with its minimum beside it rather than extrapolated.
#[test]
fn each_arm_states_its_sample_size_and_refuses_a_rank_it_cannot_support() {
    let summary = concurrency_summary(&read_runs());
    for arm in summary["per_arm"].as_array().expect("one arm per bound") {
        let bound = arm["configured_concurrency"].as_u64().unwrap_or(0);
        assert_eq!(
            arm["simulations"].as_u64(),
            Some(RUNS_PER_ARM as u64),
            "bound {bound}"
        );
        for key in [
            "simulation_duration_ns",
            "rpc_wall_duration_ns",
            "rpc_union_duration_ns",
            "rpc_overlap_duration_ns",
            "serial_wait_duration_ns",
            "rpc_gap_duration_ns",
        ] {
            let table = &arm[key];
            assert_eq!(
                table["samples"].as_u64(),
                Some(RUNS_PER_ARM as u64),
                "bound {bound} {key}: the sample count is the arm's own simulations"
            );
            assert!(
                table["measured"].as_bool().unwrap_or(false),
                "bound {bound} {key}: three samples measure a minimum, a median and a maximum"
            );
            assert!(
                table["min_ns"].is_number() && table["max_ns"].is_number(),
                "{key}"
            );
            assert!(table["p50_ns"].is_number(), "{key}");
            assert!(
                table["min_ns"].as_u64() <= table["max_ns"].as_u64(),
                "bound {bound} {key}: a minimum above its own maximum is not a measurement"
            );
            for (rank, minimum) in [(90u64, 10u64), (95, 20), (99, 100)] {
                let value = &table[format!("p{rank}_ns")];
                assert!(
                    value.is_null(),
                    "bound {bound} {key}: p{rank} over {RUNS_PER_ARM} samples is {} — §21's 「不得伪造」 \
                     means an unsupported rank is absent, not estimated",
                    value
                );
                let reason = &table[format!("p{rank}_reason")];
                assert_eq!(
                    (
                        reason["reason"].as_str(),
                        reason["minimum_samples"].as_u64()
                    ),
                    (Some("insufficient_sample"), Some(minimum)),
                    "bound {bound} {key}: p{rank}"
                );
            }
        }
        // §15's two figures, published apart, with the wire's own peak as a third — three lists of
        // the arm's own length, so a reader can line a run's bound up against both measurements.
        for key in ["observed_max_concurrency", "wire_max_concurrency"] {
            let per_simulation = arm[key]["per_simulation"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                per_simulation.len(),
                RUNS_PER_ARM,
                "bound {bound} {key}: one peak per simulation"
            );
            assert!(
                arm[key]["min"].is_number() && arm[key]["max"].is_number(),
                "bound {bound} {key}"
            );
        }
    }
    assert_eq!(
        summary["simulations_without_a_dispatch_report"].as_u64(),
        Some(0),
        "every run of this experiment scheduled something"
    );
    assert_eq!(summary["arms_are_never_blended"], json!(true));
    assert_eq!(summary["sources_are_never_blended"], json!(true));
    assert_eq!(summary["unit"], json!("ns"));
}

/// §16 and §23 and §25 as one gate: the instrument asked the node for nothing. A call count that
/// moved, an extra method, a second header read or a second attempt all mean the same verdict in
/// §16's words — 本轮实验失败.
#[test]
fn instrumentation_asked_the_node_for_nothing_extra() {
    let runs = read_runs();
    let expected = outside_multiset(&runs[0]);
    for run in &runs {
        assert_eq!(
            run.calls(),
            BASELINE_CALLS,
            "{}: §23's count is {BASELINE_CALLS} calls in every arm — a bound that adds a request \
             changed the volume, not the schedule",
            run.name
        );
        assert_eq!(
            run.line()["call_count"].as_u64(),
            Some(BASELINE_CALLS),
            "{}",
            run.name
        );
        let rpc = &run.line()["rpc"];
        assert_eq!(
            rpc["total_calls"].as_u64(),
            rpc["successful_calls"].as_u64(),
            "{}: a failed call is a different story from an extra one, and §16 counts both",
            run.name
        );
        assert_eq!(
            rpc["failed_calls"].as_u64(),
            Some(0),
            "{}: §24 — an error that reached the summary would have to be in the report",
            run.name
        );
        assert_eq!(
            rpc["total_attempts"].as_u64(),
            Some(BASELINE_ATTEMPTS),
            "{}: §25 — attempts equal calls exactly when nothing retried",
            run.name
        );
        assert_eq!(rpc["retried_calls"].as_u64(), Some(0), "{}", run.name);
        // §25's shape comes from the recorder's own test: a call that failed and was retried is one
        // record whose `attempts` list has two entries, so `total_attempts` above is the number that
        // moves under a retry while `total_calls` stays at 39
        // (crates/chain/tests/rpc_trace_safety.rs::a_retried_call_is_one_record_with_two_attempts).
        assert_eq!(
            run.line()["calls"]
                .as_array()
                .expect("a call list")
                .iter()
                .map(|call| call["attempts"].as_array().map(Vec::len).unwrap_or(0))
                .sum::<usize>(),
            BASELINE_ATTEMPTS as usize,
            "{}: the call list's own attempt entries, not the summary's folded count",
            run.name
        );
        assert_eq!(
            run.line()["duplicates"]["duplicate_state_reads"].as_u64(),
            Some(0),
            "{}: §23's duplicate count",
            run.name
        );
        for (method, count) in BASELINE_PER_RUN {
            assert_eq!(
                method_row(&run.rpc_summary, method)["count"].as_u64(),
                Some(count),
                "{}: {method} is {count} call(s) in the baseline shape §23 fixes",
                run.name
            );
        }
        // The two methods §16 names by name, counted out of the run's own call list rather than
        // out of a per-method table a reader has to trust.
        let mut methods: Vec<&str> = Vec::new();
        for call in run.line()["calls"].as_array().expect("a call list") {
            methods.push(call["method"].as_str().unwrap_or("?"));
        }
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "eth_blockNumber")
                .count(),
            0,
            "{}: §16 — an `eth_blockNumber` this run did not need is this experiment failing",
            run.name
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| **method == "eth_getBlockByNumber")
                .count(),
            1,
            "{}: §16 — exactly one header read per simulation, as in the baseline",
            run.name
        );
        assert_eq!(methods.len(), BASELINE_CALLS as usize, "{}", run.name);
        // §24's second half belongs to the instrument too: a refusal is published, never skipped.
        assert_eq!(
            run.rpc_summary["diagnosis_refusals"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "{}: the assembly refused nothing, so it also skipped nothing it could not read",
            run.name
        );
        assert_eq!(
            run.rpc_summary["instrumentation_issues_no_requests"],
            json!(true),
            "{}: the writer's own claim, restated per run in the pooled table",
            run.name
        );
        // §16 covers the whole lifecycle, not just the simulation: the calls this file does not
        // count have to be the same multiset in every arm too, or a louder bound bought its speed
        // by asking the node for something different on the way in.
        assert_eq!(
            outside_multiset(run),
            expected,
            "{}: the outside-simulation multiset moved between arms",
            run.name
        );
        assert_eq!(
            run.outside["core_state_reads_not_included"]
                ["simulation_calls_recorded_elsewhere"]
                .as_u64(),
            Some(BASELINE_CALLS),
            "{}: this file disclaims the same {BASELINE_CALLS} calls every arm counted, so the two \
             lists cannot be describing different runs",
            run.name
        );
    }
    let (fresh, _) = fresh_assembly("instrumentation");
    let merged = read_json(&fresh.join(RPC_SUMMARY_FILE));
    let mut expected_total = 0u64;
    for (method, count) in BASELINE_PER_RUN {
        let expected = count * runs.len() as u64;
        expected_total += expected;
        assert_eq!(
            method_row(&merged, method)["count"].as_u64(),
            Some(expected),
            "the pooled table holds {expected} {method} call(s)"
        );
    }
    assert_eq!(
        sum_per_source(&merged, "total_calls"),
        expected_total,
        "and the source total is the per-method totals, not a fourth count"
    );
    assert_eq!(expected_total, BASELINE_CALLS * runs.len() as u64);
    assert_eq!(sum_per_source(&merged, "total_attempts"), expected_total);
    assert_eq!(sum_per_source(&merged, "retried_calls"), 0);
    assert_eq!(sum_per_source(&merged, "failed_calls"), 0);
}

/// §18: no concurrent path reached for a moving head. Checked twice — once as the map reports it,
/// once directly out of the nine lines — because §8 makes state consistency the highest priority
/// and a gate that reads only its own output proves nothing.
#[test]
fn every_state_read_named_the_block_its_run_pinned() {
    const STATE_METHODS: [&str; 4] = [
        "eth_getBalance",
        "eth_getCode",
        "eth_getStorageAt",
        "eth_getTransactionCount",
    ];
    let runs = read_runs();
    let mut wire_blocks: Vec<String> = Vec::new();
    for run in &runs {
        let line = run.line();
        let pinned = line["block_number"].as_u64().unwrap_or(0);
        let mut state_calls = 0;
        for call in line["calls"].as_array().expect("a call list") {
            let method = call["method"].as_str().unwrap_or("?");
            if !STATE_METHODS.contains(&method) {
                continue;
            }
            state_calls += 1;
            let block = call["block"].as_str().unwrap_or_else(|| {
                panic!(
                    "{}: {method} carries no block parameter — §8 forbids a concurrent path that \
                     reads `latest` and a silent absence is how one would hide",
                    run.name
                )
            });
            assert!(
                !matches!(
                    block,
                    "latest" | "pending" | "safe" | "finalized" | "earliest"
                ),
                "{}: {method} asked for `{block}` — §8's 「禁止任何并发路径偷偷使用 latest / pending / unsafe latest」",
                run.name
            );
            assert_eq!(
                block.parse::<u64>().ok(),
                Some(pinned),
                "{}: {method} went out at block {block} while the run pinned {pinned}",
                run.name
            );
            let key = call["dedup_key"].as_str().unwrap_or_else(|| {
                panic!("{}: {method} has no dedup key to check against", run.name)
            });
            let parts: Vec<&str> = key.split('|').collect();
            assert_eq!(
                (
                    parts.get(1).and_then(|raw| raw.parse::<u64>().ok()),
                    parts.get(2).and_then(|raw| raw.parse::<u64>().ok())
                ),
                (line["chain_id"].as_u64(), Some(pinned)),
                "{}: {method}'s key `{key}` is not this run's chain and block",
                run.name
            );
            wire_blocks.push(block.to_string());
        }
        assert_eq!(
            state_calls,
            BASELINE_CALLS as usize - 1,
            "{}: 38 of the 39 calls are state reads; the header read is the other",
            run.name
        );
    }
    let map = dependency_map(&runs);
    let identity = &map["block_identity"];
    assert_eq!(
        identity["every_state_key_named_the_run_block"].as_u64(),
        Some(runs.len() as u64),
        "§18: the map itself has to reach the same verdict the lines do"
    );
    assert_eq!(
        identity["simulations_that_did_not"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "and name no simulation that disagreed"
    );
    let seen = identity["wire_block_fields_seen"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        seen.iter().filter(|value| value.is_string()).count(),
        runs.len(),
        "the only block forms ever seen on the wire are the {} pinned numbers: {seen:?}",
        runs.len()
    );
    let mut sorted = wire_blocks;
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        runs.len(),
        "one decimal block per run, as above"
    );
}

/// §5–§7: the map publishes what the code proved, keeps what it did not as `unknown`, and does not
/// let a batch member list stand in for a dependency argument.
#[test]
fn the_dependency_map_separates_what_was_proved_from_what_was_not() {
    let map = dependency_map(&read_runs());
    assert_eq!(
        map["dependency_states"].as_array().map(Vec::len),
        Some(2),
        "independent and unknown — §7's two answers, with no third one invented for storage"
    );
    let rows = map["per_method"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 4, "the four state methods");
    let row = |method: &str| -> Value {
        rows.iter()
            .find(|entry| entry["method"] == json!(method))
            .cloned()
            .unwrap_or_else(|| panic!("no {method} row"))
    };
    /// The phrase each independent row has to contain. They are not the same phrase because the
    /// arguments are not the same argument: balance and nonce are independent because each is a
    /// fact about the run's pinned block, and code is independent because the address set is
    /// closed before any of the reads is issued (§5, §6). A row that lost its reason would still
    /// print `independent`, so the gate reads the reason.
    const INDEPENDENCE_ANCHORS: [(&str, &str); 3] = [
        ("eth_getBalance", "pinned block"),
        ("eth_getTransactionCount", "pinned number"),
        ("eth_getCode", "address set is decided"),
    ];
    for (method, anchor) in INDEPENDENCE_ANCHORS {
        let entry = row(method);
        assert_eq!(
            entry["dependency"],
            json!("independent"),
            "{method}: §5 names these as proved-independent"
        );
        assert_eq!(entry["may_be_a_batch_member"], json!(true), "{method}");
        assert!(
            entry["basis"].as_str().unwrap_or("").contains(anchor),
            "{method}: the basis has to say why the answer does not feed another read, and the \
             phrase that carries it is `{anchor}`: {}",
            entry["basis"]
        );
        assert!(
            entry["measured"]["batched_reads"].as_u64().unwrap_or(0) > 0,
            "{method}: §7's proof is that the runs actually handed this kind to the scheduler"
        );
    }
    let storage = row("eth_getStorageAt");
    assert_eq!(
        storage["dependency"],
        json!("unknown"),
        "§7: 「如果不能证明，必须保持依赖顺序，并在 evidence 中记录 dependency = unknown」"
    );
    assert_eq!(
        storage["may_be_a_batch_member"],
        json!(false),
        "and 20 of the 39 calls per run are storage reads that keep the caller's order"
    );
    assert_eq!(
        storage["measured"]["batched_reads"].as_u64(),
        Some(0),
        "storage was never a batch member in any run — §5's 'unknown' is the rule the code follows"
    );
    assert_eq!(
        storage["measured"]["calls_in_the_runs"].as_u64(),
        Some(20 * RUNS_REQUIRED as u64),
        "every storage read of every run is counted, including the ones no arm touched"
    );
    assert_eq!(
        storage["measured"]["simulations_where_two_of_this_method_were_outstanding"].as_u64(),
        Some(0),
        "the trace agrees with the map: no simulation ever held two storage reads at once"
    );
    // The batches that were formed name the runs' own blocks and chains and one of §2's limits.
    assert_eq!(
        map["batch_descriptor_blocks"].as_array().map(Vec::len),
        Some(RUNS_REQUIRED),
        "one descriptor block per run: {:?}",
        map["batch_descriptor_blocks"]
    );
    assert_eq!(
        map["batch_descriptor_chain_ids"].as_array().map(Vec::len),
        Some(1)
    );
    let limits: Vec<u64> = map["batch_limits_seen"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_u64())
        .collect();
    assert!(
        limits
            .iter()
            .all(|limit| ARMS.iter().any(|(_, bound)| bound == limit)),
        "batch limits seen: {limits:?} — §2's bounds and nothing else"
    );
    assert_eq!(
        map["lines_without_a_dispatch_report"].as_u64(),
        Some(0),
        "every line reported"
    );
    assert!(
        map["scheduler_bound_is_never_a_dependency_claim"]
            .as_str()
            .unwrap_or("")
            .contains("scheduler"),
        "the map says which of its own columns is a proof and which is a count"
    );
}

/// §38, and the reason a hex-length heuristic is not the gate: the fixture's own log data is longer
/// than a signature, so the claim has to rest on the method list and the two empty files.
#[test]
fn nothing_in_this_directory_signed_broadcast_or_spent() {
    let (signed, submissions) = route_run_counts();
    assert_eq!(
        signed, 0,
        "§38: {signed} lines in `{SIGNED_FILE}` across the nine route runs — a signature would be \
         a record, and this milestone is build-only"
    );
    assert_eq!(
        submissions, 0,
        "§38: {submissions} lines in `{SUBMISSIONS_FILE}` — a broadcast would be a submission"
    );
    let runs = read_runs();
    let mut read_before: Vec<String> = Vec::new();
    let mut preflight_wallet: Vec<String> = Vec::new();
    for run in &runs {
        assert_eq!(
            run.route_run["execution"]["mode"],
            json!("build-only"),
            "{}: the lane that could sign was never entered",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["completed"],
            json!(false),
            "{}: a completed execution would mean the sequence ran",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["transactions"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "{}: §19's 「不得 sign / broadcast」 — no transaction was even carried into the record",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["after"],
            Value::Null,
            "{}",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["cost"],
            Value::Null,
            "{}: a cost is what a submission charges, and none was paid",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["delta_audit"]["lines"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "{}: §38's 0 ETH is an audit that was never taken, and the report has to say so",
            run.name
        );
        assert_eq!(
            run.route_run["execution"]["flows"].as_array().map(Vec::len),
            Some(0),
            "{}",
            run.name
        );
        // `before` is a different kind of record and the honest gate has to treat it as one: a run
        // whose intent passed validation reads the wallet's balances to check it can fund the
        // sequence, and that read happens before the build-only stop. So the claim is not that no
        // asset was looked at — it is that what looked at it were read-only methods, that the
        // snapshot exists only for runs that never submitted anything, and that the wallet's native
        // balance is one number across all of them.
        let before = &run.route_run["execution"]["before"];
        let status = run.route_run["execution"]["status"].as_str();
        if before.is_null() {
            assert_ne!(
                status,
                Some("built"),
                "{}: a run that built its sequence reads the wallet it would have funded, so a \
                 missing `before` beside status {status:?} means this gate stopped matching the \
                 code's behaviour",
                run.name
            );
            continue;
        }
        assert_eq!(
            status,
            Some("built"),
            "{name} took an asset read while its status was {status:?}",
            name = run.name
        );
        let provenance = before["provenance"].as_str().unwrap_or_else(|| {
            panic!(
                "{}: a `before` snapshot with no provenance says where nothing",
                run.name
            )
        });
        for method in ["eth_getBalance", "eth_call"] {
            assert!(
                provenance.contains(method),
                "{}: the funding check is {method}, and its provenance does not name it: \
                 {provenance}",
                run.name
            );
        }
        assert!(
            !provenance.contains("eth_sendRawTransaction"),
            "{}: §38's 「本轮不得调用 eth_sendRawTransaction」, quoted in the record's own \
             provenance: {provenance}",
            run.name
        );
        read_before.push(
            before["native_wei"]
                .as_str()
                .unwrap_or("<absent>")
                .to_string(),
        );
        // The snapshot's `before` is not the only place a run names a wallet read: preflight's
        // input-asset check has one too, in decimal rather than hex. §5 and §8 both count these, so
        // both have to be present, quoted as a balance read, and say the same number.
        let preflight = &run.route_run["preflight"]["input_asset"];
        let preflight_source = preflight["source"].as_str().unwrap_or_else(|| {
            panic!(
                "{}: preflight says which read produced its input asset",
                run.name
            )
        });
        assert!(
            preflight_source.contains("eth_getBalance("),
            "{}: the input-asset check is a balance read and its source does not say so: \
             {preflight_source}",
            run.name
        );
        preflight_wallet.push(
            preflight["available_wei"]
                .as_str()
                .unwrap_or("<absent>")
                .to_string(),
        );
        if !before.is_null() {
            assert_eq!(
                as_wei(before["native_wei"].as_str().unwrap_or("")),
                as_wei(preflight["available_wei"].as_str().unwrap_or("")),
                "{}: the snapshot and the preflight check read the same wallet at the same head \
                 and disagree about it",
                run.name
            );
            assert_eq!(
                before["token_balances"].as_object().map(|m| m.len()),
                Some(2),
                "{}: the route has two tokens and the funding snapshot reads both",
                run.name
            );
        }
        // §5 puts a number in prose — how many reads the lane made through the adapter no sink
        // watches — so that number has to be one shape across the funded runs, not an average of
        // three different ones.
        assert_eq!(
            (
                provenance.matches("eth_getBalance(").count(),
                provenance.matches("eth_call balanceOf(").count()
            ),
            (1, 2),
            "{}: the funding check is one native balance and one read of each of the route's two \
             tokens, and the README counts them out of this line: {provenance}",
            run.name
        );
        for call in run.line()["calls"].as_array().expect("a call list") {
            assert_ne!(
                call["method"].as_str(),
                Some("eth_sendRawTransaction"),
                "{}: §38's 「本轮不得调用 eth_sendRawTransaction」",
                run.name
            );
        }
    }
    // The control for "the balance never moved": the same field, read by every run that read it.
    // The two places a run quotes one print it differently — preflight in decimal wei, the
    // snapshot as a hex word — so they are compared as integers and counted together.
    let mut read: Vec<u128> = read_before
        .iter()
        .chain(preflight_wallet.iter())
        .map(|text| as_wei(text))
        .collect();
    read.sort_unstable();
    read.dedup();
    assert_eq!(
        read.len(),
        1,
        "the {} native balance reads across the nine runs disagree about it — preflight \
         {preflight_wallet:?}, snapshots {read_before:?}",
        read_before.len() + preflight_wallet.len(),
    );

    // Every file in the tree, method tables included: the submission method appears nowhere.
    let root = concurrency_dir();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&root, &mut files);
    assert!(
        files.len() > RUNS_REQUIRED,
        "the scan is over {} files, not one of them",
        files.len()
    );
    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let text = read_text(path);
        if text.contains("eth_sendRawTransaction") {
            hits.push(path.display().to_string());
        }
    }
    assert_eq!(
        hits,
        Vec::<String>::new(),
        "the submission method is named in {hits:?} — §38 is a whole-directory claim"
    );

    // The key scan M8.3.2's gate uses: no long hex word that the runs themselves did not write.
    // A key is 64 hex digits and so is a calldata, so the whitelist is the runs' own files rather
    // than a hand-picked list of shapes.
    let mut allowed: Vec<String> = Vec::new();
    for path in &files {
        if path.starts_with(root.join(RUNS))
            || path.starts_with(root.join(ROUTE_RUNS))
            || path.starts_with(root.join(FIXED_BLOCK))
        {
            for word in hex_runs(&read_text(path)) {
                if word.len() >= 64 && !allowed.contains(&word) {
                    allowed.push(word);
                }
            }
        }
    }
    assert!(
        !allowed.is_empty(),
        "the runs' own long hex — their slots, digests and calldata — is the whitelist"
    );
    let (fresh, names) = fresh_assembly("secrets");
    for name in &names {
        let text = read_text(&fresh.join(name));
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
    // §13's integer-only rule over this milestone's two tables, which no pooled writer produced.
    for name in [
        CONCURRENCY_SUMMARY_FILE,
        DEPENDENCY_MAP_FILE,
        CORRECTNESS_FILE,
    ] {
        let table = read_json(&if name == CORRECTNESS_FILE {
            root.join(CORRECTNESS_FILE)
        } else {
            fresh.join(name)
        });
        assert!(
            !has_a_float(&table),
            "{name}: a float is a division someone did instead of stated"
        );
    }
}

/// §34's hard gate, read as a claim about this directory: the fixed-block arms are one run.
#[test]
fn the_fixed_block_arms_are_the_same_run_at_three_bounds() {
    let correctness = read_json(&concurrency_dir().join(CORRECTNESS_FILE));
    assert_eq!(correctness["milestone"], json!("M8.3.3 §17"));
    assert_eq!(
        correctness["arms"].as_array().map(Vec::len),
        Some(ARMS.len()),
        "the three arms §2 names, and C8 is not among them"
    );
    assert!(
        correctness["whole_result"]["identical"]
            .as_bool()
            .unwrap_or(false),
        "§34: 「如果 fixed-block C1 != C2 或 C1 != C4：M8.3.3 = INCOMPLETE」 — the whole-result \
         comparison says the arms are not one run, and this milestone stops here"
    );
    assert!(
        correctness["whole_result"]["fingerprint_identical"]
            .as_bool()
            .unwrap_or(false),
        "and so does its fingerprint"
    );
    let fields = correctness["fields"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    // §17's 「禁止只比较 profit」 as a count: the compared set is the semantic fields, not one.
    assert!(
        fields.len() >= 20,
        "the comparison covers {} fields; §17 asks for outcome, revert, gas, logs, return data, \
         profit, fingerprint and block identity plus any other semantic field",
        fields.len()
    );
    for (field, entry) in &fields {
        assert_eq!(
            entry["identical_across_bounds"].as_bool(),
            Some(true),
            "{field}: §17 compares field by field, so one that moved is a failed gate rather \
             than a footnote"
        );
    }
    for field in [
        "outcome",
        "revert",
        "reverted",
        "gas_used",
        "gas_charge",
        "net_profit",
        "log_count",
        "state_changes",
        "plan_summary",
        "measurements",
        "fingerprint",
        "block_number",
        "block_hash",
        "chain_id",
        "status",
        "steps",
    ] {
        assert!(
            fields.contains_key(field),
            "§17 names {field} by requirement and the record does not carry it"
        );
    }
    for gate in [
        "every_bound_serial_at_1",
        "no_bound_exceeded",
        "overlap_positive_above_1",
        "every_state_read_named_the_pin",
        "no_retry_in_this_fixture",
    ] {
        assert_eq!(
            correctness["gates"][gate].as_bool(),
            Some(true),
            "§34's fixture-side gate `{gate}` is not true, and the live tables above would be \
             describing an experiment whose semantics were not held still"
        );
    }
    // §17's second sentence: the same state reads at every bound, in the same order — read in the
    // fixture's own units, not the live route's. `state_reads_per_bound` sums the four chain-state
    // methods (crates/simulation/tests/concurrency_abc.rs `STATE_METHODS`), so on the recorded dump
    // it counts 21 storage + 6 code + 6 balance + 6 nonce = 39 and keeps the block-header read
    // beside them: `rpc_count` is 40. The live 39 above is a different set — 20 storage, the same
    // three legs at 6 each, and the header counted in. Two numbers that happen to be equal are not
    // one number, so this gate compares the fixture against the fixture's own record and requires
    // the three bounds to agree with each other.
    let counts = &correctness["call_counts"];
    assert_eq!(
        counts["call_multiset_identical_to_bound_1"].as_bool(),
        Some(true)
    );
    assert_eq!(
        counts["ordered_sequence_identical_to_bound_1"].as_bool(),
        Some(true),
        "§17: the ordered sequence too, not only the multiset"
    );
    let fixture = read_json(&concurrency_dir().join(FIXED_BLOCK).join("c1.json"));
    let requests = |method: &str| -> u64 {
        fixture["method_counts"][method]["requests"]
            .as_u64()
            .unwrap_or(0)
    };
    let fixture_state_reads = [
        "eth_getStorageAt",
        "eth_getCode",
        "eth_getBalance",
        "eth_getTransactionCount",
    ]
    .iter()
    .map(|method| requests(method))
    .sum::<u64>();
    assert_eq!(
        fixture["rpc_count"].as_u64(),
        Some(fixture_state_reads + requests("eth_getBlockByNumber")),
        "the fixture's own request total has to be its state reads plus its header read"
    );
    let bounds = counts["state_reads_per_bound"]
        .as_array()
        .expect("one row per bound");
    assert_eq!(
        bounds.len(),
        3,
        "three bounds were compared, and a missing row is not the same as a matching one"
    );
    for per_bound in bounds {
        assert_eq!(
            per_bound["state_reads"].as_u64(),
            Some(fixture_state_reads),
            "bound {}: the fixture's method counts, not the live route's {}",
            per_bound["bound"],
            BASELINE_CALLS
        );
    }
    // The fixture's concurrency witnesses are the §11/§12 gate on a block nothing else can pin.
    for arm in correctness["concurrency"]
        .as_array()
        .expect("one row per bound")
    {
        let bound = arm["bound"].as_u64().unwrap_or(0);
        let witnesses = &arm["witnesses"];
        assert_eq!(witnesses["configured_concurrency"].as_u64(), Some(bound));
        assert_eq!(witnesses["observed_max_concurrency"].as_u64(), Some(bound));
        assert_eq!(witnesses["wire_max_concurrency"].as_u64(), Some(bound));
        if bound == 1 {
            assert_eq!(witnesses["wire_serial_or_overlap"], json!("serial"));
            assert_eq!(witnesses["wire_overlap_duration_ns"].as_u64(), Some(0));
            assert_eq!(
                witnesses["wire_union_duration_ns"], witnesses["wire_sum_duration_ns"],
                "§10's serial shape in the fixture too"
            );
        } else {
            assert_eq!(witnesses["wire_serial_or_overlap"], json!("overlap"));
            assert!(
                witnesses["wire_overlap_duration_ns"].as_u64().unwrap_or(0) > 0,
                "bound {bound}: no overlap on the fixture's wire means the experiment never \
                 achieved concurrency (§44), which is not the same finding as 并发没有收益"
            );
            assert!(
                witnesses["wire_union_duration_ns"].as_u64().unwrap_or(0)
                    < witnesses["wire_sum_duration_ns"].as_u64().unwrap_or(0),
                "bound {bound}: §36's union below sum"
            );
        }
    }
}

/// §41, and §19's 「不要手写数字」 as one gate: the twelve files an assembly writes are, in this
/// directory, that assembly byte for byte, and the directory holds the files and directories it
/// says it does — the carried fixture record included, and named as carried.
#[test]
fn the_committed_directory_is_a_byte_for_byte_assembly_of_the_runs() {
    let (fresh, names) = fresh_assembly("committed-check");
    assert_the_listing(&fresh, &names);
    let committed = concurrency_dir();
    if refreshing() {
        for name in &names {
            std::fs::copy(fresh.join(name), committed.join(name)).unwrap_or_else(|error| {
                panic!(
                    "{}: a fresh assembly could not replace the committed file: {error}",
                    committed.join(name).display()
                )
            });
        }
        eprintln!("refreshed {CONCURRENCY_DIR}");
    }
    // The listing is read after a refresh for the same reason the copy is allowed at all: on the
    // run that writes the directory, the files this gate checks are the ones being written. On
    // every other run it is the committed set that has to match.
    let (files, dirs) = committed_listing();
    assert_eq!(
        files,
        committed_file_set(),
        "{CONCURRENCY_DIR}: not the file set §29's directory is"
    );
    assert_eq!(
        dirs,
        COMMITTED_DIRS
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<String>>(),
        "{CONCURRENCY_DIR}: an extra directory is an artifact nothing asked for and a missing one \
         a question the milestone cannot answer"
    );
    for name in &names {
        assert_eq!(
            read_bytes(&fresh.join(name)),
            read_bytes(&committed.join(name)),
            "{}: the committed file is not what replaying the nine runs writes — §41 is a \
             byte-comparison of this directory against a re-assembly from its own raw evidence",
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

/// §31's fourteen questions, and §41's rule that the README's run list is the directory's listing.
#[test]
fn the_readme_answers_every_question_the_directory_raises() {
    let runs = read_runs();
    let (fresh, names) = fresh_assembly("readme");
    assert!(names.contains(&README_FILE.to_string()));
    let text = read_text(&fresh.join(README_FILE));
    let committed = read_text(&concurrency_dir().join(README_FILE));
    assert_eq!(
        text, committed,
        "the committed README is not the generated one — see §41's rule in this file's header"
    );

    // The run list is the directory, row for row.
    let listed: Vec<String> = text
        .lines()
        .filter(|line| line.starts_with("| c"))
        .filter_map(|line| line.split('|').nth(1).map(|cell| cell.trim().to_string()))
        .collect();
    let expected: Vec<String> = run_names()
        .into_iter()
        .filter(|name| name.starts_with('c'))
        .collect();
    assert_eq!(
        listed,
        expected,
        "§41: the README names {} runs and the directory holds {}",
        listed.len(),
        expected.len()
    );

    // The fourteen answers, by the question each section exists to answer. A section that lost its
    // number would silently stop answering it, so the gate looks for the heading, not the number.
    for question in [
        "What this directory is for",
        "Where the baseline comes from",
        "The arms",
        "The runs, and what each one measured",
        "RPC count per run",
        "Observed max concurrency",
        "Correctness",
        "Did this run sign anything, broadcast anything, spend any ETH?",
        "How to regenerate this directory",
        "Provenance",
        "Which block every read was taken at",
        "What this directory does not say",
    ] {
        assert!(
            text.contains(&format!("## {question}")) || text.contains(question),
            "§31 asks {question:?} and the README has no section for it"
        );
    }

    // The three numbers a reader of §31 will check by eye, quoted from the tables rather than
    // recomputed here: 39 calls, the bound list, and the fingerprint.
    assert!(
        text.contains(&format!("{BASELINE_CALLS} calls")),
        "the README states the RPC count §23 fixes"
    );
    // The fingerprint as the README prints a JSON value: serde_json's Display, which keeps the
    // quotes the fixture's own string carries.
    assert!(
        text.contains(&format!("`{}`", correctness_fingerprint())),
        "and the fixed-block fingerprint §34 turns on"
    );
    for run in &runs {
        assert!(
            text.contains(&run.name),
            "{} is missing from the run list",
            run.name
        );
        assert!(
            text.contains(&run.block().to_string()),
            "{}: its block is missing, which §31 asks for per run",
            run.name
        );
    }
    assert!(
        text.contains("C8 was not run"),
        "§31's arm list has to name the arm that did not happen"
    );
    assert!(
        text.contains("null"),
        "§21: an unsupported rank is printed as `null`, never as a blank a reader reads as zero"
    );
}

fn correctness_fingerprint() -> Value {
    read_json(&concurrency_dir().join(CORRECTNESS_FILE))["whole_result"]["fingerprint"].clone()
}

/// §30's provenance rule with the directory as its subject: every pooled file names all nine runs
/// with the figures each contributed, and this milestone's two tables name them too.
#[test]
fn every_pooled_file_names_the_nine_runs_it_was_assembled_from() {
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
            assert_eq!(row["git_revision"].as_str(), Some(run.git_revision()));
            assert_eq!(row["execution_mode"].as_str(), Some(run.execution_mode()));
        }
    }

    let summary = concurrency_summary(&runs);
    for row in summary["assembled_from"].as_array().expect("one per run") {
        let run = runs
            .iter()
            .find(|run| json!(run.name) == row["run"])
            .expect("the summary names a run the directory holds");
        assert_eq!(row["simulations"].as_u64(), Some(1), "{}", run.name);
        assert!(
            row["git_revision"].as_u64().is_none(),
            "{}: a revision is a string",
            run.name
        );
    }
    // §30's blend rule, stated in the table that would have to hold the blend.
    for arm in summary["per_arm"].as_array().expect("one arm per bound") {
        let reuse = &arm["state_read_reuse"];
        assert_eq!(
            (
                reuse["on"].as_u64(),
                reuse["off"].as_u64(),
                reuse["unreported"].as_u64()
            ),
            (arm["simulations"].as_u64(), Some(0), Some(0),),
            "bound {}: an arm is one reuse configuration and §19 says it is the on one",
            arm["configured_concurrency"]
        );
        assert_eq!(
            arm["per_source"].as_array().map(Vec::len),
            Some(1),
            "live only"
        );
        assert_eq!(
            arm["totals_fields_missing_from_a_row"].as_u64(),
            Some(0),
            "every totalled field was present in every row, so no total folded an absent figure \
             into a zero"
        );
    }
    // The arm totals are the runs' own rows added, and the three arms add to the directory.
    let mut calls = 0_u64;
    for arm in summary["per_arm"].as_array().expect("one arm per bound") {
        calls += arm["totals"]["calls"].as_u64().unwrap_or(0);
    }
    assert_eq!(calls, BASELINE_CALLS * runs.len() as u64);
}

/// §22 and §23's other half: the gap table and the duplicate table are the runs' own figures, and
/// the definition of a gap did not change when the reads started overlapping.
///
/// This is the row that keeps the milestone honest about "不能只看 simulation wall time": a bound that
/// overlapped two reads turns one gap between them into no gap at all, so the gap *count* is a
/// measurement of the schedule and not a leftover. It is reported per run here, never subtracted
/// between arms — §20's blocks differ.
#[test]
fn the_gap_and_duplicate_tables_are_the_runs_own_figures() {
    let runs = read_runs();
    for run in &runs {
        let line = run.line();
        let gaps = &run.gaps;
        let rpc = &line["rpc"];
        assert_eq!(
            gaps["gap_count"].as_u64(),
            rpc["rpc_gap_count"].as_u64(),
            "{}: the gap table and the simulation line count different stretches",
            run.name
        );
        assert_eq!(
            gaps["gap_total_ns"].as_u64(),
            rpc["rpc_gap_duration_ns"].as_u64(),
            "{}",
            run.name
        );
        assert_eq!(
            gaps["per_simulation"][0]["block_number"], line["block_number"],
            "{}: the gap table is about this run's block",
            run.name
        );
        // §22 keeps M8.3.2's definition rather than inventing one that a concurrent path could
        // satisfy by moving the boundary.
        assert!(
            gaps["definitions"]
                .as_str()
                .unwrap_or("")
                .contains("no call in flight"),
            "the gap definition is M8.3.2's and says what a stretch has to be: {}",
            gaps["definitions"]
        );
        let ratio = &gaps["gap_total_over_call_window_per_mille"];
        assert_eq!(
            ratio["numerator"].as_u64(),
            gaps["gap_total_ns"].as_u64(),
            "{}: a per-mille ratio's numerator is the figure itself, in ns",
            run.name
        );
        assert!(
            !has_a_float(ratio),
            "{}: §13's integers, and a per-mille is a numerator over a denominator",
            run.name
        );

        let duplicates = &run.duplicates;
        assert_eq!(
            sum_per_source(duplicates, "total_state_reads"),
            BASELINE_CALLS,
            "{}: every one of the 39 calls keys, so the duplicate table's denominator is the \
             whole call list",
            run.name
        );
        assert_eq!(
            sum_per_source(duplicates, "duplicate_state_reads"),
            0,
            "{}: §23 — a concurrent path that read the same slot twice would show up here first",
            run.name
        );
    }

    // The pooled tables are those figures added, from the same writer the runs used.
    let (fresh, _) = fresh_assembly("gaps-and-duplicates");
    let pooled_gaps = read_json(&fresh.join(RPC_GAPS_FILE));
    let pooled_duplicates = read_json(&fresh.join(DUPLICATES_FILE));
    let gaps_from_runs: u64 = runs
        .iter()
        .map(|run| run.gaps["gap_count"].as_u64().unwrap_or(0))
        .sum();
    let duplicates_from_runs: u64 = runs
        .iter()
        .map(|run| sum_per_source(&run.duplicates, "duplicate_state_reads"))
        .sum();
    assert_eq!(
        pooled_gaps["gap_count"].as_u64(),
        Some(gaps_from_runs),
        "the pooled gap count is the runs' own counts added"
    );
    assert_eq!(
        pooled_gaps["simulations"].as_u64(),
        Some(runs.len() as u64),
        "and it is over nine simulations, not nine files"
    );
    assert_eq!(
        sum_per_source(&pooled_duplicates, "duplicate_state_reads"),
        duplicates_from_runs,
        "the pooled duplicate tally is the runs' own figures, added"
    );
    assert_eq!(
        sum_per_source(&pooled_duplicates, "total_state_reads"),
        BASELINE_CALLS * runs.len() as u64
    );

    // The gap count did move between the arms, and the direction is the schedule's, not a claim.
    let summary = concurrency_summary(&runs);
    let gaps_of = |bound: u64| -> Vec<u64> {
        summary["per_simulation"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|row| row["configured_concurrency"].as_u64() == Some(bound))
            .map(|row| row["rpc_gap_count"].as_u64().unwrap_or(0))
            .collect()
    };
    let (serial, loud) = (gaps_of(1), gaps_of(4));
    assert_eq!(
        (serial.len(), loud.len()),
        (RUNS_PER_ARM, RUNS_PER_ARM),
        "three gap counts per arm, and the summary has {serial:?} at bound 1 against {loud:?} \
         at bound 4"
    );
    let shortest_serial = serial.iter().min().copied().unwrap_or(0);
    assert!(
        loud.iter().all(|count| *count < shortest_serial),
        "bound 1's runs have {serial:?} gaps and bound 4's have {loud:?} — the overlap removes \
         the stretches between the reads it holds open together; a louder arm whose gaps did not \
         move is an arm that did not overlap anything"
    );
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
    {
        let entry = entry.expect("a readable entry");
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
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
