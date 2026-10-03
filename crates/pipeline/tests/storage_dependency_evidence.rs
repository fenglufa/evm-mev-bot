//! M8.4.1 §21 and §27: `data/evidence/m8/storage-dependency/`, assembled from the runs that
//! measured it.
//!
//! §6's rule — 「所有 aggregate 必须能够从 raw evidence 重建」 — is what this file exists to make
//! unfalsifiable by accident. The run directories under `runs/` (§15's live arm) and `fixed-block/`
//! (§14's) hold the raw: each simulation's trace line, and [`PIPELINE_CALLS_FILE`]'s row per call
//! of both sinks. The root's tables are those rows fed back through the same functions the runs
//! used ([`dependency_tables_of`]), and the gate below asks whether the committed root *is* that
//! re-assembly, byte for byte. There is no measured figure in this file and no arithmetic either:
//! §19's 「不要手写数字」 is enforced by having nothing here that could disagree with `diagnosis.rs`.
//!
//! ## Where each of §3's questions is graded
//!
//! Q1 (is there a real dependency between the 20 storage reads) and Q3 (where does a live run's
//! non-simulation RPC time go) are facts about these runs, so they are graded here. Q2 — §16's
//! 「instrumentation 不能改变结果」 — is graded here for the record the experiment wrote and in
//! `crates/simulation/tests/storage_dependency_experiment.rs` for the rule that a difference would
//! have been caught. What cannot be graded from a directory at all — that an `unknown` may never be
//! promoted, that a different slot is not evidence of independence — lives in the unit tests of
//! `diagnosis.rs`, where a wrong rule can be made to happen on purpose.
//!
//! ## Two arms, one question, and what may not cross between them
//!
//! The fixture arm says what the *code* proves about the order of reads with the network out of the
//! picture; the live arm says the same verdict survives a real node. §27 asks that they agree, and
//! [`the_fixture_arm_and_the_live_arm_reach_the_same_dependency_verdict`] compares verdicts rather
//! than numbers — the fixture's route touches one more slot, so its counts differ by one read and
//! that difference is not a finding.
//!
//! A `started_ns` is nanoseconds since *its own run's* monotonic origin, so a union, an overlap, a
//! gap and a stage span never cross a run boundary: each run's durations stay in that run's row of
//! [`PIPELINE_SUMMARY_FILE`]'s `per_run`, and what the pooled file holds is counts and durations
//! summed or listed per run. The three live runs sit on three different blocks (§15 allows it), so
//! no subtraction between two live runs appears anywhere here either.
//!
//! ## Why the README is generated
//!
//! For the reason M8.3.3 gave: most of what a reader asks about this directory has a number for an
//! answer, and a hand-kept README drifts from the tables beside it invisibly. [`build_readme`]
//! draws it out of the assembled tables and the byte gate covers it.
//!
//! ## How these files change
//!
//! Assembly happens under `target/pipeline-tests/`; `M841_STORAGE_DEPENDENCY_REFRESH=1` copies a
//! fresh assembly over the committed root files, which is the only way they change. `runs/`,
//! `route-runs/`, `fixed-block/` and `correctness/` are the runs' own output — written by
//! `crates/cli` and by the simulation crate's experiment — and nothing here writes or deletes them.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::diagnosis::{
    dependency_tables_of, outside_simulation_table_assembled, DiagnosisEvidence,
    ACCOUNT_MATRIX_FILE, BOTTLENECK_FILE, DEPENDENCY_MAP_FILE, DEPENDENCY_SUMMARY_FILE,
    DUPLICATES_FILE, OUTSIDE_FILE, PIPELINE_CALLS_FILE, PIPELINE_SUMMARY_FILE, README_FILE,
    RPC_GAPS_FILE, RPC_SUMMARY_FILE, SIMULATION_SUMMARY_FILE, STAGE_SUMMARY_FILE,
    STORAGE_BREAKDOWN_FILE, STORAGE_READS_FILE, TRACES_FILE, USES_PRIOR_RESPONSE_NOT_DETERMINED,
    USES_PRIOR_RESPONSE_POSSIBLE,
};

const EVIDENCE_DIR: &str = "data/evidence/m8/storage-dependency";
/// §15's live arm, §14's fixed-block arm, the route runs beside them, and §16's correctness pair.
const RUNS: &str = "runs";
const FIXED_BLOCK: &str = "fixed-block";
const ROUTE_RUNS: &str = "route-runs";
const CORRECTNESS: &str = "correctness";
const COMPARISON_FILE: &str = "comparison.json";
const ROUTE_RUN_FILE: &str = "route-run.json";
const SIGNED_FILE: &str = "signed-transactions.jsonl";
const SUBMISSIONS_FILE: &str = "submissions.jsonl";

/// §15's 「至少 3 次」 and §14's three. Three, because §17's ranks need more than three to say
/// anything at all, and a fourth run of a diagnosis-only milestone buys a rank nothing here claims.
const LIVE_RUNS: usize = 3;
const FIXTURE_RUNS: usize = 3;

/// §12's control, read from the previous milestone's own committed file rather than restated as a
/// constant: this milestone added a stage and a caller stamp to a call, and a stamp that made a run
/// ask for something extra would show up as a method count that no longer matches the build it came
/// out of.
const PREVIOUS_MILESTONE: &str = "data/evidence/m8/concurrency";
const BASELINE_RUN: &str = "c1-01";

/// §12's live baseline: 39 calls of one simulation, 38 of them state reads and one header read.
const SIMULATION_CALLS: u64 = 39;

/// The four methods that read state, in §4's own list. §13's 「禁止 latest / pending」 is about
/// these; the head read — whose whole job is to ask what height there is — is deliberately not one.
const STATE_METHODS: [&str; 4] = [
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getTransactionCount",
];

/// The two recorders a run writes into, in the words the rows carry.
const SINK_SIMULATION: &str = "simulation";
const SINK_LIFECYCLE: &str = "lifecycle";

/// Everything §21's root holds that one assembly of the three live runs writes: the writer's ten
/// generic files, and this milestone's six.
const GENERATED_FILES: [&str; 16] = [
    README_FILE,
    ACCOUNT_MATRIX_FILE,
    BOTTLENECK_FILE,
    DEPENDENCY_MAP_FILE,
    DEPENDENCY_SUMMARY_FILE,
    DUPLICATES_FILE,
    OUTSIDE_FILE,
    PIPELINE_CALLS_FILE,
    PIPELINE_SUMMARY_FILE,
    RPC_GAPS_FILE,
    RPC_SUMMARY_FILE,
    SIMULATION_SUMMARY_FILE,
    STAGE_SUMMARY_FILE,
    STORAGE_BREAKDOWN_FILE,
    STORAGE_READS_FILE,
    TRACES_FILE,
];

/// `M841_STORAGE_DEPENDENCY_REFRESH` names no directory: its presence is the instruction to copy a
/// fresh assembly over the committed evidence. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M841_STORAGE_DEPENDENCY_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_DIR)
}

/// A scratch assembly directory, emptied first because the evidence writer appends: a second
/// assembly into a directory holding a first would double every trace line rather than reproduce
/// the first.
fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target/pipeline-tests")
        .join(format!("m8.4.1-{name}"));
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

/// A JSONL file's line count, with the file required to exist: a directory that had quietly lost
/// [`SIGNED_FILE`] would make "0 signatures" true for the wrong reason.
fn jsonl_lines(path: &Path) -> usize {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// The run directories of one arm, in listing order. A directory inside an arm that holds no trace
/// file is reported as the mistake it is rather than skipped, and an arm that lost a run fails
/// here rather than in the pooled table that would have quietly had one fewer row: §6's raw layer
/// has to be complete before any aggregate over it means anything.
fn run_names(arm: &str, expected: usize) -> Vec<String> {
    let root = evidence_dir().join(arm);
    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .map(|name| name.to_string_lossy().to_string())
        .collect();
    for name in &names {
        assert!(
            root.join(name).join(TRACES_FILE).exists(),
            "{EVIDENCE_DIR}/{arm}/{name} has no `{TRACES_FILE}`: a directory inside an arm that \
             is not a run would otherwise be left out of the assembly silently"
        );
    }
    names.sort();
    assert_eq!(
        names.len(),
        expected,
        "{EVIDENCE_DIR}/{arm} holds {} runs ({}) and the task book asks for {expected}",
        names.len(),
        names.join(", ")
    );
    names
}

/// One run, held as the files it wrote rather than as a summary of them, so a figure the assembly
/// did not read cannot appear in its output.
struct Run {
    name: String,
    /// `runs/` or `fixed-block/`.
    arm: &'static str,
    lines: Vec<Value>,
    /// §9's raw layer for this run: both sinks' rows, verbatim.
    calls: Vec<Value>,
    /// This run's stage ladder, as it published it — §17's containment check reads a run's own
    /// spans against a run's own calls and nothing else.
    stage_spans: Vec<Value>,
    outside: Value,
    rpc_summary: Value,
    dependency_map: Value,
    pipeline_summary: Value,
    /// The ladder's own record. `Null` for the fixture arm, which drives a simulation directly and
    /// never enters the route, so has no sign/broadcast files to count.
    route_run: Value,
}

impl Run {
    fn read(arm: &'static str, name: &str) -> Self {
        let dir = evidence_dir().join(arm).join(name);
        let at = |file: &str| dir.join(file);
        let pipeline_summary = read_json(&at(PIPELINE_SUMMARY_FILE));
        let stage_spans = pipeline_summary["assembled_from"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["stage_spans"].as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            arm != RUNS || !stage_spans.is_empty(),
            "{name}: a live run's {PIPELINE_SUMMARY_FILE} published no `assembled_from` stage \
             spans, so §17's ladder half of the pipeline has nothing to be rebuilt from. The \
             fixture arm may be empty — it drives a simulation and runs no stage ladder",
            name = name
        );
        let route_run = if arm == RUNS {
            read_json(
                &evidence_dir()
                    .join(ROUTE_RUNS)
                    .join(name)
                    .join(ROUTE_RUN_FILE),
            )
        } else {
            Value::Null
        };
        Self {
            name: name.to_string(),
            arm,
            lines: trace_lines(&at(TRACES_FILE)),
            calls: read_json(&at(PIPELINE_CALLS_FILE))["rows"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            stage_spans,
            // The fixture arm has no row here: it drives a simulation and never runs the ladder,
            // so it records no RPC outside one. `Null` is that absence read from the directory,
            // not a zero written in — and the live arm below still has to have the file.
            outside: match (arm, at(OUTSIDE_FILE).exists()) {
                (RUNS, false) => panic!(
                    "{EVIDENCE_DIR}/{arm}/{name} has no `{OUTSIDE_FILE}`: §12's whole-pipeline \
                     split is rebuilt from this table, and a live run that lost it would leave \
                     the root with fewer rows than runs"
                ),
                (_, false) => Value::Null,
                (_, true) => read_json(&at(OUTSIDE_FILE)),
            },
            rpc_summary: read_json(&at(RPC_SUMMARY_FILE)),
            dependency_map: read_json(&at(DEPENDENCY_MAP_FILE)),
            pipeline_summary,
            route_run,
        }
    }

    fn live_runs() -> Vec<Self> {
        run_names(RUNS, LIVE_RUNS)
            .into_iter()
            .map(|name| Self::read(RUNS, &name))
            .collect()
    }

    fn fixture_runs() -> Vec<Self> {
        run_names(FIXED_BLOCK, FIXTURE_RUNS)
            .into_iter()
            .map(|name| Self::read(FIXED_BLOCK, &name))
            .collect()
    }

    /// The one simulation this run recorded. Both arms are single-simulation route runs, and a run
    /// with a second line would have its first read by every accessor below, so the count is
    /// checked rather than assumed.
    fn line(&self) -> &Value {
        assert_eq!(
            self.lines.len(),
            1,
            "{}: §14/§15's runs are one simulation each and this one wrote {} trace lines",
            self.name,
            self.lines.len()
        );
        &self.lines[0]
    }

    fn block(&self) -> u64 {
        self.line()["block_number"].as_u64().unwrap_or(0)
    }

    fn source(&self) -> String {
        self.line()["source"].as_str().unwrap_or("?").to_string()
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

    fn endpoint_id(&self) -> String {
        self.line()["endpoint_id"]
            .as_str()
            .unwrap_or("?")
            .to_string()
    }

    /// Calls this run's *simulation* recorded — §12's 39, as opposed to the run's whole list.
    fn simulation_calls(&self) -> u64 {
        self.line()["call_count"].as_u64().unwrap_or(0)
    }

    /// §3's three counts for this run's storage reads, out of the map the run wrote.
    fn dependency_counts(&self) -> (u64, u64, u64) {
        let summary = &self.dependency_map["summary"];
        (
            summary["independent"].as_u64().unwrap_or(0),
            summary["ordered"].as_u64().unwrap_or(0),
            summary["unknown"].as_u64().unwrap_or(0),
        )
    }

    fn storage_reads(&self) -> u64 {
        self.dependency_map["summary"]["total"]
            .as_u64()
            .unwrap_or(0)
    }

    /// This run's row of §17's totals: the ladder window, the RPC union, and the excess that turns
    /// a bare `non_rpc_duration_ns: 0` from a claim into a boundary case.
    fn totals(&self) -> &Value {
        &self.pipeline_summary["per_run"][0]["totals"]
    }

    fn block_pin(&self) -> &Value {
        &self.pipeline_summary["per_run"][0]["block_pin"]
    }

    fn bucket_count(&self, bucket: &str) -> Option<u64> {
        self.pipeline_summary["per_run"][0]["pipeline_buckets"]
            .as_array()?
            .iter()
            .find(|row| row["bucket"].as_str() == Some(bucket))
            .and_then(|row| row["rpc_count"].as_u64())
    }

    /// The calls this run's two attribution accounts disagree about, as the run published them.
    fn disagreements(&self) -> Vec<Value> {
        self.pipeline_summary["per_run"][0]["attribution_accounts"]["disagreements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The method behind a `logical_request_id`, read from this run's own call rows — so the
    /// README names a call by what the file says rather than by what the writer remembers.
    fn method_of(&self, id: &str) -> String {
        self.calls
            .iter()
            .find(|row| row["logical_request_id"].as_str() == Some(id))
            .map(|row| shown(&row["method"]))
            .unwrap_or_else(|| "{a request id in no call row}".to_string())
    }

    /// §13's question asked of one sink: how many of this run's state reads that sink made, and
    /// how many of those named the block this run pinned. A head read is not a state read.
    fn state_reads_at(&self, sink: &str) -> (u64, u64) {
        let pinned = self.block().to_string();
        self.calls.iter().fold((0, 0), |(total, at), row| {
            if row["sink"].as_str() != Some(sink)
                || !STATE_METHODS.contains(&row["method"].as_str().unwrap_or(""))
            {
                return (total, at);
            }
            (
                total + 1,
                at + u64::from(row["block_tag"].as_str() == Some(pinned.as_str())),
            )
        })
    }

    /// The state reads that named a height other than this run's, as `{stage} {method} at {tag}`.
    fn off_pin_reads(&self) -> Vec<String> {
        let pinned = self.block().to_string();
        self.calls
            .iter()
            .filter(|row| {
                STATE_METHODS.contains(&row["method"].as_str().unwrap_or(""))
                    && row["block_tag"].as_str() != Some(pinned.as_str())
            })
            .map(|row| {
                format!(
                    "{} {} at {}",
                    shown(&row["stage"]),
                    shown(&row["method"]),
                    shown(&row["block_tag"]),
                )
            })
            .collect()
    }

    fn calls_by_sink(&self) -> (u64, u64) {
        let by_sink = &self.pipeline_summary["per_run"][0]["calls_by_sink"];
        (
            by_sink[SINK_SIMULATION].as_u64().unwrap_or(0),
            by_sink[SINK_LIFECYCLE].as_u64().unwrap_or(0),
        )
    }

    /// The raw row [`dependency_tables_of`] takes: this run's own lines and call rows, plus the
    /// provenance it published for them. One of these per run is the whole input of the pooled
    /// tables, which is what makes the root a re-assembly rather than a re-measurement.
    fn dependency_source(&self) -> Value {
        json!({
            "run": self.name,
            "source": self.line()["source"].clone(),
            "chain_id": self.line()["chain_id"].clone(),
            "block_number": self.line()["block_number"].clone(),
            "endpoint_id": self.line()["endpoint_id"].clone(),
            "execution_mode": self.execution_mode(),
            "git_revision": self.git_revision(),
            "generated_at_unix_ms": self.generated_at(),
            "lines": self.lines,
            "calls": self.calls,
            "stage_spans": self.stage_spans,
        })
    }

    /// §6's provenance row: what the root says about where this run's figures came from, in the
    /// integers the run itself used.
    fn provenance(&self) -> Value {
        let (independent, ordered, unknown) = self.dependency_counts();
        let (simulation, lifecycle) = self.calls_by_sink();
        json!({
            "run": self.name,
            "directory": format!("{EVIDENCE_DIR}/{}/{}/", self.arm, self.name),
            "arm": if self.arm == RUNS { "§15 live" } else { "§14 fixed-block" },
            "source": self.line()["source"].clone(),
            "chain_id": self.line()["chain_id"].clone(),
            "block_number": self.line()["block_number"].clone(),
            "endpoint_id": self.line()["endpoint_id"].clone(),
            "git_revision": self.git_revision(),
            "execution_mode": self.execution_mode(),
            "generated_at_unix_ms": self.generated_at(),
            "simulations": self.lines.len(),
            "simulation_calls": simulation,
            "lifecycle_calls": lifecycle,
            "state_read_reuse": self.line()["state_read_cache"]["reuse"].clone(),
            "configured_concurrency": self.line()["state_read_concurrency"]["configured"].clone(),
            "storage_reads": self.storage_reads(),
            "dependency_independent": independent,
            "dependency_ordered": ordered,
            "dependency_unknown": unknown,
            "pipeline_total_duration_ns": self.totals()["pipeline_total_duration_ns"].clone(),
            "rpc_union_duration_ns": self.totals()["rpc_union_duration_ns"].clone(),
            "non_rpc_duration_ns": self.totals()["non_rpc_duration_ns"].clone(),
            "union_extends_past_the_ladder_window_ns":
                self.totals()["union_extends_past_the_ladder_window_ns"].clone(),
            "all_state_reads_pinned": self.block_pin()["all_state_reads_pinned"].clone(),
            "route_mode": self.route_run["mode"].clone(),
            "route_counters": self.route_run["counters"]["counters"].clone(),
            "successful_real_arbitrage": self.route_run["successful_real_arbitrage"].clone(),
        })
    }
}

/// The whole assembly: three live runs' raw re-fed through the writer a run uses, §14's lifecycle
/// half pooled from those runs' own rows, and this milestone's generated README beside them.
fn assemble_to(dir: &Path, runs: &[Run]) -> Vec<String> {
    assert!(
        !runs.is_empty(),
        "there is nothing to assemble: {EVIDENCE_DIR}/{RUNS} holds no run directory"
    );
    let revisions: Vec<&str> = runs.iter().map(Run::git_revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] == pair[1]),
        "§15's runs have to be one build run three times, and these are {revisions:?}"
    );
    let modes: Vec<&str> = runs.iter().map(Run::execution_mode).collect();
    assert!(
        modes.windows(2).all(|pair| pair[0] == pair[1]),
        "§15's runs have to be one mode, and these are {modes:?}"
    );

    // The one wall-clock stamp in the directory is the latest of the runs' own rather than the
    // moment of assembly, so a re-assembly of the same runs writes the same bytes — which is the
    // gate below. No duration in these files reads a wall clock (§7).
    let generated_at = runs.iter().map(Run::generated_at).max().unwrap_or(0);
    let simulation_calls: u64 = runs.iter().map(|run| run.calls_by_sink().0).sum();
    let tables = dependency_tables_of(
        &runs
            .iter()
            .map(Run::dependency_source)
            .collect::<Vec<Value>>(),
    );
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
    let provenance: Vec<Value> = runs.iter().map(Run::provenance).collect();

    let mut evidence =
        DiagnosisEvidence::open(dir, runs[0].git_revision(), runs[0].execution_mode(), true)
            .expect("the assembly directory opens");
    evidence.assemble_from(generated_at, provenance);
    evidence.attach_dependency(tables);
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

    let root = read_root(dir);
    write_text(&dir.join(README_FILE), &build_readme(runs, &root));

    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The assembled root's tables, read back out of the directory that was just written, so every
/// figure the gates and the README use below is a published figure rather than a local one.
fn read_root(dir: &Path) -> Value {
    json!({
        DEPENDENCY_MAP_FILE: read_json(&dir.join(DEPENDENCY_MAP_FILE)),
        DEPENDENCY_SUMMARY_FILE: read_json(&dir.join(DEPENDENCY_SUMMARY_FILE)),
        PIPELINE_SUMMARY_FILE: read_json(&dir.join(PIPELINE_SUMMARY_FILE)),
        STAGE_SUMMARY_FILE: read_json(&dir.join(STAGE_SUMMARY_FILE)),
        STORAGE_READS_FILE: read_json(&dir.join(STORAGE_READS_FILE)),
        PIPELINE_CALLS_FILE: read_json(&dir.join(PIPELINE_CALLS_FILE)),
    })
}

fn root_table<'root>(root: &'root Value, name: &str) -> &'root Value {
    root.get(name)
        .unwrap_or_else(|| panic!("the assembly wrote no {name} table"))
}

/// A fresh assembly of the committed runs, and its file listing.
fn fresh_assembly(name: &str) -> (PathBuf, Vec<String>) {
    let dir = scratch(name);
    let names = assemble_to(&dir, &Run::live_runs());
    (dir, names)
}

/// §21's generated file set — what one assembly writes, no more and no less. An extra file is an
/// artifact nothing asked for; a missing one a question the directory has stopped answering.
fn assert_the_listing(dir: &Path, names: &[String]) {
    let mut expected: Vec<String> = GENERATED_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        names,
        &expected,
        "{}: not the file set one assembly of the live runs writes",
        dir.display()
    );
}

/// The committed directory's top-level entries, files and directories apart.
fn committed_listing() -> (Vec<String>, Vec<String>) {
    let root = evidence_dir();
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

/// A figure as the README prints it: an absent one stays the word `null`, because a table that
/// printed an absent p99 as a blank cell reads as a zero, and §11 forbids that swap.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(values) => values.iter().map(shown).collect::<Vec<String>>().join(", "),
        Value::Object(_) => "{…}".to_string(),
    }
}

/// A nanosecond duration as seconds to three decimals, for prose only: the JSON keeps integers,
/// and a float in a table would be a figure a reader could not add back up.
fn seconds(ns: &Value) -> String {
    match ns.as_u64() {
        Some(ns) => format!("{:.3}", ns as f64 / 1e9),
        None => "null".to_string(),
    }
}

fn table_row(lines: &mut Vec<String>, cells: &[&str]) {
    lines.push(format!("| {} |", cells.join(" | ")));
}

fn separator_row(lines: &mut Vec<String>, columns: usize) {
    table_row(lines, &vec!["—"; columns]);
}

/// A run's dependency counts as one phrase, or the honest report that the arm is not uniform.
fn counts_wording(counts: (u64, u64, u64)) -> String {
    let (independent, ordered, unknown) = counts;
    format!("{independent} independent, {ordered} ordered, {unknown} unknown")
}

/// The same counts for a whole arm — printed only when every run in it reports the same triple,
/// since the README says 「per run」 beside it.
fn arm_counts(runs: &[Run]) -> String {
    let first = runs[0].dependency_counts();
    if runs.iter().all(|run| run.dependency_counts() == first) {
        counts_wording(first)
    } else {
        format!(
            "a different triple per run ({})",
            runs.iter()
                .map(|run| format!("{}: {}", run.name, counts_wording(run.dependency_counts())))
                .collect::<Vec<String>>()
                .join("; ")
        )
    }
}

/// A run's `route-run.json` counters as `key=value` pairs in the file's own sorted order.
fn counters(run: &Run) -> String {
    let Some(entries) = run.route_run["counters"]["counters"].as_object() else {
        return "no counters".to_string();
    };
    let mut keys: Vec<&String> = entries.keys().collect();
    keys.sort();
    keys.iter()
        .map(|key| format!("{key}={}", shown(&entries[*key])))
        .collect::<Vec<String>>()
        .join(", ")
}

/// One figure every run in the arm reports, or the list of what they actually said.
fn uniform(values: Vec<String>) -> String {
    if values.windows(2).all(|pair| pair[0] == pair[1]) {
        values.first().cloned().unwrap_or_default()
    } else {
        format!("not uniform across runs: {}", values.join(" / "))
    }
}

/// The same values counted by kind — `a ×19, b ×1` — so a table cell never averages away a state
/// that only one row is in.
fn tally(values: Vec<String>) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for value in values {
        match counts.iter_mut().find(|(name, _)| *name == value) {
            Some((_, count)) => *count += 1,
            None => counts.push((value, 1)),
        }
    }
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// §21's README, drawn entirely from the tables beside it and from the run directories.
fn build_readme(runs: &[Run], root: &Value) -> String {
    let map = root_table(root, DEPENDENCY_MAP_FILE);
    let dependency = root_table(root, DEPENDENCY_SUMMARY_FILE);
    let pipeline = root_table(root, PIPELINE_SUMMARY_FILE);
    let per_run = pipeline["per_run"].as_array().cloned().unwrap_or_default();
    let fixtures = Run::fixture_runs();
    let nodes = map["nodes"].as_array().cloned().unwrap_or_default();
    let edges = map["edges"].as_array().cloned().unwrap_or_default();
    let leg_rows: Vec<String> = map["leg_vocabulary"]["reads_per_leg"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| format!("{} ×{}", shown(&row["leg"]), shown(&row["reads"])))
        .collect();
    let vocabulary: Vec<String> = map["leg_vocabulary"]["proofs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| shown(&row["leg"]))
        .collect();
    let mut lines: Vec<String> = Vec::new();

    lines.push("# M8.4.1 — storage dependency and end-to-end RPC diagnosis".to_string());
    lines.push(String::new());
    lines.push(format!(
        "{runs} live runs on one commit ({commit}), {fixtures} fixed-block runs of the offline \
         fixture beside them, all of them `build-only`. This directory is a diagnosis and nothing \
         else: §1 forbids, for this milestone, the very changes these tables would make a case for \
         — storage concurrency, the C1/C2/C4 scheduler, batch JSON-RPC, multicall, prefetch, new \
         caches, connection and keep-alive tuning, provider swaps, and anything in REVM, Graph, \
         Opportunity, Risk or Execution — so no figure below is evidence that a run got faster, \
         and none is offered as a reason to make one of those changes.",
        runs = runs.len(),
        fixtures = FIXTURE_RUNS,
        commit = runs[0].git_revision(),
    ));
    lines.push(String::new());
    lines.push(format!(
        "```text\n{}\n```",
        "Q1：20 个 storage read 是否存在真实 dependency？\n\
         Q2：如果没有，为什么实际上还是串行？\n\
         Q3：simulation 外的 RPC 时间去了哪里？"
    ));
    lines.push(String::new());

    // 1 — the three answers.
    let (independent, ordered, unknown) = (
        &map["summary"]["independent"],
        &map["summary"]["ordered"],
        &map["summary"]["unknown"],
    );
    lines.push("## 1. The three answers".to_string());
    lines.push(format!(
        "**Q1 — yes, on every read the code could argue about.** The pooled map covers {total} \
         storage reads, {per_run} of them in each of the {runs} live runs; §3 sets the \
         `{FIXED_BLOCK}` arm's own counts beside them. {independent} of the reads are proved \
         independent of every other; {ordered} are `ordered`, each naming the read it waited for \
         and the leg that makes the two sequential; {unknown} is `unknown`, which is one per \
         simulation — its first storage read, where there is no earlier read to argue from. \
         Nothing was promoted: §3 forbids it and §5 forbids the other route to the same \
         conclusion, since a different slot is not evidence of independence. The legs those reads \
         sit on are {legs}, and the reason a read on `fiber` cannot be argued independent is in \
         its own proof text: REVM's async database holds one outstanding request there, so the \
         next slot is a function of the value just returned.",
        total = map["summary"]["total"],
        per_run = runs[0].storage_reads(),
        runs = runs.len(),
        legs = leg_rows.join(", "),
    ));
    lines.push(format!(
        "**Q2 — the order the interpreter chose, and the wire agrees.** Over the {runs} live runs \
         the pooled pair table counts {pairs} pairs of storage reads inside one simulation, of \
         which {pair_ordered} are `ordered` and {pair_independent} `independent`; and on the wire \
         each run's calls went out one at a time — `max_concurrency` 1, `rpc_overlap_duration_ns` \
         0 in every run's row of `{PIPELINE_SUMMARY_FILE}`. That is as far as a diagnosis goes: \
         whether the reads *could* be issued together is a design question §1 takes off this \
         milestone's table.",
        runs = runs.len(),
        pairs = pair_total(map),
        pair_ordered = pair_count(map, "ordered"),
        pair_independent = pair_count(map, "independent"),
    ));
    let buckets = dependency["pipeline"]
        .as_object()
        .map(|entries| {
            entries
                .iter()
                .filter(|(bucket, _)| bucket.as_str() != "stages_the_seven_do_not_name")
                .map(|(bucket, row)| (bucket.clone(), row.clone()))
                .collect::<Vec<(String, Value)>>()
        })
        .unwrap_or_default();
    let named: Vec<String> = buckets
        .iter()
        .filter(|(_, row)| row["rpc_count_total"].as_u64().unwrap_or(0) > 0)
        .map(|(bucket, row)| {
            format!(
                "{bucket}: {} calls / {} s of union",
                row["rpc_count_total"],
                seconds(&row["rpc_union_duration_ns_total"])
            )
        })
        .collect();
    let named_calls: u64 = buckets
        .iter()
        .map(|(_, row)| row["rpc_count_total"].as_u64().unwrap_or(0))
        .sum();
    // The four named buckets do not hold the arm's calls, and a sentence that added them up and
    // called the sum 「calls across the runs」 would read as though they did. The difference is
    // this bucket and the rows no stage stamped, so the reader can re-add the arm's total from the
    // two figures named beside them — and each of those two has to be pointed at where it actually
    // prints, because one is a row of the table above and the other is a per-run total, not a row.
    let unnamed_bucket_calls: u64 = dependency["pipeline"]["stages_the_seven_do_not_name"]
        ["rpc_count_total"]
        .as_u64()
        .unwrap_or(0);
    let unstamped_calls: u64 = runs
        .iter()
        .map(|run| {
            run.totals()["calls_without_a_stamped_stage"]
                .as_u64()
                .unwrap_or(0)
        })
        .sum();
    let arm_calls: u64 = runs
        .iter()
        .map(|run| run.totals()["attempts_total"].as_u64().unwrap_or(0))
        .sum();
    lines.push(format!(
        "**Q3 — into the stages around the simulation, and it is not a small part.** {named_calls} \
         of the arm's {arm_calls} recorded calls fall in the four buckets §7 names — {named}. The \
         remaining {} are {unnamed_bucket_calls} in `stages_the_seven_do_not_name`, the last row of \
        §5's table, and {unstamped_calls} the issuing code stamped no stage at all, which print as \
        `calls_without_a_stamped_stage` in each run's totals and as `stage: null` rows in \
        `{PIPELINE_CALLS_FILE}` — one population of \
         {arm_calls} read at two granularities, not two counts to add to it. A bucket's union is \
         measured inside one run and then added across runs, never swept across two clocks, so \
         these are sums of {runs} separate measurements and not one distribution. \
         `{PIPELINE_CALLS_FILE}` has the row each figure came from.",
        unnamed_bucket_calls + unstamped_calls,
        named = named.join("; "),
        runs = runs.len(),
    ));
    lines.push(String::new());

    // 2 — the runs.
    lines.push("## 2. The runs".to_string());
    table_row(
        &mut lines,
        &[
            "run",
            "block",
            "sim calls",
            "lifecycle calls",
            "storage reads",
            "independent",
            "ordered",
            "unknown",
            "ladder window s",
            "RPC union s",
            "union past ladder s",
            "local (non-RPC) s",
            "max concurrency",
            "RPC overlap s",
        ],
    );
    separator_row(&mut lines, 14);
    for run in runs {
        let (sim, lifecycle) = run.calls_by_sink();
        let (independent, ordered, unknown) = run.dependency_counts();
        let totals = &per_run
            .iter()
            .find(|row| row["run"] == json!(run.name))
            .unwrap_or_else(|| panic!("the assembly dropped {}'s row", run.name))["totals"];
        table_row(
            &mut lines,
            &[
                &run.name,
                &run.block().to_string(),
                &sim.to_string(),
                &lifecycle.to_string(),
                &run.storage_reads().to_string(),
                &independent.to_string(),
                &ordered.to_string(),
                &unknown.to_string(),
                &seconds(&totals["pipeline_total_duration_ns"]),
                &seconds(&totals["rpc_union_duration_ns"]),
                &seconds(&totals["union_extends_past_the_ladder_window_ns"]),
                &seconds(&totals["non_rpc_duration_ns"]),
                &shown(&totals["max_concurrency"]),
                &seconds(&totals["rpc_overlap_duration_ns"]),
            ],
        );
    }
    lines.push(String::new());
    let past: Vec<String> = runs
        .iter()
        .filter(|run| !run.totals()["union_extends_past_the_ladder_window_ns"].is_null())
        .map(|run| run.name.clone())
        .collect();
    lines.push(format!(
        "The two columns headed `union past ladder s` and `local (non-RPC) s` belong together. \
         `{RUNS}` has {runs} runs and they did not all stop \
         in the same place: {past} got as far as the execution lane's `build` stage, which issues \
         calls and writes no closed span of its own, so on those runs the calls cover more time \
         than the ladder's window and `non_rpc_duration_ns` clamps to 0. Publishing the excess \
         beside it — `union_extends_past_the_ladder_window_ns` — is what stops that 0 from being \
         read as 「this run had no local time」, which would be §11's forbidden swap in a field \
         nobody thought about.",
        runs = runs.len(),
        past = if past.is_empty() {
            "no run".to_string()
        } else {
            format!("{} of them ({})", past.len(), past.join(", "))
        },
    ));
    let shortest = runs
        .iter()
        .min_by_key(|run| run.totals()["rpc_count"].as_u64().unwrap_or(0))
        .unwrap_or_else(|| panic!("{} has no run to compare", RUNS));
    let others: Vec<String> = runs
        .iter()
        .filter(|run| run.name != shortest.name)
        .map(|run| {
            format!(
                "{} ({} calls): {}",
                run.name,
                shown(&run.totals()["rpc_count"]),
                counters(run)
            )
        })
        .collect();
    lines.push(format!(
        "The run that stopped earlier is the one with the short call column: {} held {} calls and \
         its own `route-run.json` counters read {short}, where the others read {others}. That is \
         why the two accounts of the same run's wall time differ between runs — a preflight that \
         blocks never reaches the `build` reads, so they are not in its union either.",
        shortest.name,
        shown(&shortest.totals()["rpc_count"]),
        short = counters(shortest),
        others = others.join("; "),
    ));
    lines.push(String::new());

    // 3 — the two arms.
    let fixture_reads: u64 = fixtures.iter().map(Run::storage_reads).sum();
    let live_reads: u64 = runs.iter().map(Run::storage_reads).sum();
    lines.push("## 3. The fixed-block arm and the live arm".to_string());
    lines.push(format!(
        "`{FIXED_BLOCK}/` is §14's arm: the offline fixture's recorded dump, replayed {FIXTURE_RUNS} \
         times — one block, no network, `all_state_reads_pinned` true in every run. Its {fixture_reads} \
         storage reads against the live arm's {live_reads} differ by the slots this route touches on \
         a real head, and the call lists differ the same way — {} inside the simulation against {}. \
         What is identical is the verdict, which is the only thing §27 asks the two arms to share: \
         `{FIXED_BLOCK}/run-*/{DEPENDENCY_MAP_FILE}` reports {}, and \
         `{RUNS}/*/dependency-map.json` {} per run. An arm with a network in it changed nothing \
         about what the code proves.",
        uniform(
            fixtures
                .iter()
                .map(|run| run.simulation_calls().to_string())
                .collect()
        ),
        uniform(runs.iter().map(|run| run.simulation_calls().to_string()).collect()),
        arm_counts(&fixtures),
        arm_counts(runs),
    ));
    lines.push(String::new());

    // 4 — the map, read by read.
    lines.push("## 4. What the dependency map says read by read".to_string());
    lines.push(format!(
        "`{DEPENDENCY_MAP_FILE}` is §6's machine-readable shape: {} `nodes`, {} `edges`, one \
         `summary`, plus `per_simulation` and `pair_summary`. A node names its `caller` (the REVM \
         phase that asked, verbatim), its `leg`, its `dependency`, the `depends_on` list it waited \
         behind, and an `evidence` array carrying the proof text for exactly that claim — so §27's \
         「所有 dependency 分类必须有 evidence」 is checkable per read rather than per paragraph. The \
         legs this build can tell apart are the {} in `leg_vocabulary.proofs` ({}), each matched \
         against the simulation crate's own phase constants, and every storage read of this route \
         landed on one leg: {}. Only `materialised_audit` is a leg whose definition is a proof of \
         independence — its whole key list exists before its first request goes out — and no read \
         here is on it. Two reads on different legs are neither: their pair is `undecidable`, a \
         relation of its own rather than a side to file it on.",
        nodes.len(),
        edges.len(),
        vocabulary.len(),
        vocabulary.join(", "),
        leg_rows.join(", "),
    ));
    lines.push(format!(
        "`uses_prior_response` — §4's fourth field — never says `yes` here: {upr} in the pooled \
         nodes. Where the leg's order is what makes a read wait it says `possible_not_determined`; \
         where there was no earlier read on the leg to argue from, `not_determined`. A value \
         flowing through the interpreter is not in this record, only the requests are, so a `yes` \
         would be a claim built out of the wrong material.",
        upr = tally(
            nodes
                .iter()
                .map(|node| shown(&node["uses_prior_response"]))
                .collect()
        ),
    ));
    lines.push(format!(
        "Pairs, which is how §19's summary reads: of the {total} pairs of reads inside one \
         simulation, {pairs} are ordered, {indep} independent, {unde} undecidable, {contra} \
         contradiction. That is why the pair count sits far above the node count and why \
         `independent` stays where §20 wants it.",
        total = pair_total(map),
        pairs = pair_count(map, "ordered"),
        indep = pair_count(map, "independent"),
        unde = pair_count(map, "undecidable"),
        contra = pair_count(map, "contradiction"),
    ));
    lines.push(String::new());

    // 5 — the pipeline split, per bucket and per run.
    lines.push("## 5. Where the run's RPC time is".to_string());
    table_row(
        &mut lines,
        &[
            "bucket",
            "code stage names",
            &format!("calls ({} runs)", runs.len()),
            "union total s",
            "per-run min s",
            "per-run max s",
            "runs reporting it",
        ],
    );
    separator_row(&mut lines, 7);
    for (bucket, row) in &buckets {
        table_row(
            &mut lines,
            &[
                bucket,
                &shown(&row["code_stage_names"]),
                &shown(&row["rpc_count_total"]),
                &seconds(&row["rpc_union_duration_ns_total"]),
                &seconds(&row["rpc_union_duration_ns_per_run"]["min_ns"]),
                &seconds(&row["rpc_union_duration_ns_per_run"]["max_ns"]),
                &shown(&row["runs_reporting_it"]),
            ],
        );
    }
    let catch_all = &dependency["pipeline"]["stages_the_seven_do_not_name"];
    table_row(
        &mut lines,
        &[
            "stages_the_seven_do_not_name",
            "unstamped + a stage no bucket folds",
            &shown(&catch_all["rpc_count_total"]),
            &seconds(&catch_all["rpc_union_duration_ns_total"]),
            &seconds(&catch_all["rpc_union_duration_ns_per_run"]["min_ns"]),
            &seconds(&catch_all["rpc_union_duration_ns_per_run"]["max_ns"]),
            &shown(&catch_all["runs_reporting_it"]),
        ],
    );
    lines.push(String::new());
    let disagreements: Vec<String> = runs
        .iter()
        .map(|run| {
            let rows = run.disagreements();
            format!(
                "{}: {}",
                run.name,
                if rows.is_empty() {
                    "none".to_string()
                } else {
                    rows.iter()
                        .map(|row| {
                            format!(
                                "`{}` stamped {}, held by `{}`",
                                run.method_of(
                                    row["logical_request_id"].as_str().unwrap_or_default()
                                ),
                                shown(&row["stamped_stage"]),
                                shown(&row["held_by_stage_span"]),
                            )
                        })
                        .collect::<Vec<String>>()
                        .join("; ")
                }
            )
        })
        .collect();
    let disagreement_pairs: usize = runs.iter().map(|run| run.disagreements().len()).sum();
    let call_rows: u64 = runs.iter().map(|run| run.calls.len() as u64).sum();
    let unstamped_methods: Vec<String> = runs
        .iter()
        .flat_map(|run| {
            run.calls
                .iter()
                .filter(|row| row["stage"].is_null())
                .map(|row| shown(&row["method"]))
                .collect::<Vec<String>>()
        })
        .collect();
    lines.push(format!(
        "Two accounts of the same call are kept beside each other rather than merged: `stage` on a \
         `{PIPELINE_CALLS_FILE}` row is what the issuing code said, and `attribution_accounts` in \
         `{PIPELINE_SUMMARY_FILE}` is what that run's ladder says held the instant. They disagree \
         on {disagreement_pairs} of the {call_rows} call rows in this arm, and the disagreement is \
         published as a pair rather than resolved — either the stamp is wrong or the socket is \
         wired to the wrong leg, and both are findings §11 would rather see than a table that \
         agrees with itself: {per_run}. Every row with no stamp at all is {unstamped} — the call \
         that produces the adapter a sink could be attached to, so it cannot carry a stamp issued \
         by one. {ambiguous} of the {call_rows} rows carry a stamp the run itself marks \
         untrustworthy — no label here had to be guessed at because two calls were in flight at \
         once, which is the same fact §2's last two columns record from the other side.",
        per_run = disagreements.join("; "),
        unstamped = tally(unstamped_methods),
        ambiguous = uniform(
            runs.iter()
                .map(|run| shown(&run.totals()["calls_with_an_ambiguous_label"]))
                .collect()
        ),
    ));
    lines.push(format!(
        "A rank over {runs} runs is `null`: p90 needs {minimum} samples by the repository's own \
         rule (`minimum_samples_for_rank`), so the per-run columns above are the measurements and \
         the total is a sum. Nothing in this directory pools a duration across two clocks.",
        runs = runs.len(),
        minimum = dependency["minimum_samples_for_rank"]["p90"],
    ));
    lines.push(String::new());

    // 6 — §13's pinning.
    lines.push("## 6. Which height every read named".to_string());
    table_row(
        &mut lines,
        &[
            "run",
            "state reads in the run",
            "at this run's block",
            "at another height",
            "at a tag that is not a height",
            "all pinned",
        ],
    );
    separator_row(&mut lines, 6);
    for row in &per_run {
        let pin = &row["block_pin"];
        table_row(
            &mut lines,
            &[
                &shown(&row["run"]),
                &shown(&pin["state_read_calls"]),
                &shown(&pin["state_read_calls_at_this_runs_block"]),
                &shown(&pin["state_read_calls_at_another_height"]),
                &shown(&pin["state_read_calls_at_a_tag_that_is_not_a_height"]),
                &shown(&pin["all_state_reads_pinned"]),
            ],
        );
    }
    lines.push(String::new());
    let (sim_state_reads, sim_at_pin): (u64, u64) = runs
        .iter()
        .map(|run| run.state_reads_at(SINK_SIMULATION))
        .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let off_pin: Vec<String> = runs.iter().flat_map(Run::off_pin_reads).collect();
    lines.push(format!(
        "The boolean has two independent causes, and the three counts before it separate them. \
         Every state read of a **simulation**, in every run of both arms, named the block that \
         simulation pinned — {sim_at_pin} of the {sim_state_reads} in this arm, and all of them in \
         each `{FIXED_BLOCK}` run. The head read is not a state read at all: its job is to ask what \
         height there is. What makes a live row `false` is the execution lane, and its rows are \
         these: {}. That is §13's business too, and it is why this milestone publishes the split \
         instead of a bare `false` a reader has to take on faith. In the fixture arm there is no \
         execution lane, so `true` there means what it says.",
        tally(off_pin),
    ));
    lines.push(String::new());

    // 7 — §16's correctness.
    let comparison = read_json(&evidence_dir().join(CORRECTNESS).join(COMPARISON_FILE));
    let fields = comparison["fields"]
        .as_object()
        .map(|entries| entries.len())
        .unwrap_or(0);
    let identical = comparison["fields"]
        .as_object()
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, row)| row["identical"].as_bool().unwrap_or(false))
                .count()
        })
        .unwrap_or(0);
    lines.push("## 7. §16: did the instrumentation change an answer?".to_string());
    lines.push(format!(
        "`{CORRECTNESS}/{COMPARISON_FILE}`: the same fixed-block route run with the sink off and \
         with it on. {fields} result fields compared, {identical} of them `identical`, the whole \
         result's fingerprint `{fingerprint}` with `fingerprint_identical` {fp_same}, the endpoint \
         seeing {wire_calls} calls with `identical_in_order` {order_same}, and `gas_charge` among \
         the fields that came out the same on both sides. The silent arm recorded \
         {recorded_baseline} rows into the sink against the instrumented arm's \
         {recorded_instrumented} — that gap is the control, and without it an equality of two \
         identical arms would be a tautology: nothing was observed, so nothing could disagree.",
        fingerprint = shown(&comparison["whole_result"]["fingerprint"]),
        fp_same = shown(&comparison["whole_result"]["fingerprint_identical"]),
        wire_calls = comparison["calls"]["count_instrumented"],
        order_same = shown(&comparison["calls"]["identical_in_order"]),
        recorded_baseline = comparison["metrics"]["baseline"]["calls_recorded"],
        recorded_instrumented = comparison["metrics"]["instrumented"]["calls_recorded"],
    ));
    lines.push(String::new());

    // 8 — safety.
    let (mut signed, mut submitted) = (0, 0);
    for name in run_names(RUNS, LIVE_RUNS) {
        let dir = evidence_dir().join(ROUTE_RUNS).join(&name);
        signed += jsonl_lines(&dir.join(SIGNED_FILE));
        submitted += jsonl_lines(&dir.join(SUBMISSIONS_FILE));
    }
    lines.push("## 8. Did these runs sign, broadcast, or spend anything?".to_string());
    lines.push(format!(
        "No. {signed} lines across `{ROUTE_RUNS}/*/{SIGNED_FILE}` and {submitted} across \
         `{ROUTE_RUNS}/*/{SUBMISSIONS_FILE}` for the {runs} route runs, `mode` is `{mode}` in every \
         one of them, `successful_real_arbitrage` is {arb} in every one, and no call row anywhere \
         in this directory carries `eth_sendRawTransaction`. §15's 「不得 sign / broadcast」 held, \
         and no key entered the process to take these figures: the execution lane got as far as \
         building an intent and, in the run that stopped earliest, did not get past its preflight \
         fee check.",
        runs = runs.len(),
        mode = uniform(
            runs.iter()
                .map(|run| shown(&run.route_run["mode"]))
                .collect()
        ),
        arb = uniform(
            runs.iter()
                .map(|run| shown(&run.route_run["successful_real_arbitrage"]))
                .collect()
        ),
    ));
    lines.push(String::new());

    // 9 — what the directory cannot see.
    lines.push("## 9. What this directory cannot see".to_string());
    for row in pipeline["not_observed"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        lines.push(format!(
            "- **{}** — {}",
            shown(&row["surface"]),
            shown(&row["why"])
        ));
    }
    lines.push(format!(
        "- **a stage that issues calls and writes no span** — the execution lane's `build` reads \
         facts to build an intent and leaves no closed interval behind, which is why \
         `union_extends_past_the_ladder_window_ns` exists as a field and {past} of the {runs} runs \
         have a non-null value in it.",
        past = per_run
            .iter()
            .filter(|row| !row["totals"]["union_extends_past_the_ladder_window_ns"].is_null())
            .count(),
        runs = runs.len(),
    ));
    lines.push(String::new());

    // 10 — regeneration.
    lines.push("## 10. How to regenerate this directory".to_string());
    lines.push(format!(
        "```bash\n{}\n```",
        "# 1. the build, with the recipe this repository needs on this machine\n\
         CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" GIT_REVISION=$(git rev-parse HEAD) \\\n\
         \x20 cargo build --bin evm-mev-bot\n\n\
         # 2. §14's fixed-block arm and §16's correctness pair, offline on the recorded dump\n\
         M841_FIXTURE_EVIDENCE=data/evidence/m8/storage-dependency \\\n\
         \x20 cargo test -p evm-simulation --test storage_dependency_experiment\n\n\
         # 3. §15's live arm: three build-only runs, no key in the environment. `GIWA_RPC_URL`\n\
         #    holds the endpoint; each run opens its own session directory under both roots.\n\
         ./target/debug/evm-mev-bot arbitrage \\\n\
         \x20 --rpc-url \"$GIWA_RPC_URL\" --execution-mode build-only \\\n\
         \x20 --sender <the test sender> --input-token <the input token> \\\n\
         \x20 --candidate-mid <the router> --candidate-pool <pool A> --candidate-pool <pool B> \\\n\
         \x20 --input-wei 100000000000000 --fee-num 997 --fee-den 1000 \\\n\
         \x20 --fee-evidence data/evidence/m7/candidate-fee-measurement.json \\\n\
         \x20 --market real-market \\\n\
         \x20 --market-evidence \"reserves and blockTimestampLast read at the pinned live head by this run\" \\\n\
         \x20 --latency-trace --rpc-trace --diagnose-state-acquisition --diagnose-storage-dependency \\\n\
         \x20 --evidence-dir data/evidence/m8/storage-dependency/route-runs \\\n\
         \x20 --rpc-output data/evidence/m8/storage-dependency/runs\n\n\
         # 4. the pooled tables, this README, and the byte-for-byte check\n\
         M841_STORAGE_DEPENDENCY_REFRESH=1 cargo test -p evm-pipeline --test storage_dependency_evidence\n\
         cargo test -p evm-pipeline --test storage_dependency_evidence"
    ));
    lines.push(String::new());
    lines.push(
        "The last command's second invocation is the one that fails if a number in this directory \
         was hand-edited. Step 3's refresh is the only way these root files change; `runs/`, \
         `route-runs/`, `fixed-block/` and `correctness/` are the runs' own output and are copied \
         into place, never rewritten. Each live run's stage ladder is beside it in \n\
         `data/evidence/m8/latency/<session>/`, under the same session name as its \
         `route-runs/` directory."
            .to_string(),
    );
    lines.push(String::new());

    // 11 — §20's forbidden inferences, in the directory rather than only in the report.
    lines.push("## 11. What may not be concluded from these tables".to_string());
    for claim in [
        "「storage 是独立的」 — no read here carries a proof of it, and the count of such proofs is \
         in §1 above; a slot that differs is not one",
        "「provider 是瓶颈」 — one duration is recorded at the point a request becomes bytes on the \
         wire, so node work, network round trip and this process's decode are not separable there \
         (§9's second bullet)",
        "「C4 一定更优」 — no concurrency arm is in this directory, and the leg a proof of \
         independence would have to be built on is the one §1 forbids this milestone to touch",
        "「execution RPC = 0」 — the execution lane's own adapter has no sink in this build: what \
         its reads cost is in the lifecycle rows and the buckets above where the same adapter was \
         traced, and the surfaces that stayed out are named rather than counted as none",
        "「network latency」 — nothing measured a round trip separately from a request, so every \
         duration here is a total",
        "「this run got faster」 — nothing was changed to make a run faster, and the three live runs \
         sit on three different blocks, so a difference between two of them is a difference \
         between blocks as much as between anything else",
    ] {
        lines.push(format!("- {claim}"));
    }
    lines.push(String::new());

    // 12 — provenance and file index.
    lines.push("## 12. Provenance and which file answers what".to_string());
    lines.push(
        "Every pooled file here carries `assembled_from`: one row per run, with its directory, \
         source, block, endpoint digest, commit, mode, per-sink call counts, storage read counts \
         and dependency verdicts, its own ladder figures, and its route run's counters. The \
         wall-clock stamp is the latest of the runs' own, so re-assembly is byte-reproducible and \
         no duration reads it."
            .to_string(),
    );
    for (file, answers) in [
        (
            STORAGE_READS_FILE,
            "§4's per-read record: address, slot, height, caller, stage, \
            dependency, depends_on, and the evidence beside each — one row per storage read",
        ),
        (
            DEPENDENCY_MAP_FILE,
            "§6's nodes / edges / summary over the same rows",
        ),
        (
            DEPENDENCY_SUMMARY_FILE,
            "§19's one-screen tally: the three counts, the pair counts, the \
            per-bucket call and union figures",
        ),
        (
            PIPELINE_CALLS_FILE,
            "§9's raw layer: every call of both sinks, one row each, with its \
            stage, caller, height tag, duration and attempts",
        ),
        (
            PIPELINE_SUMMARY_FILE,
            "§7/§10's per-run totals and stage rows, the two attribution \
            accounts, §13's pin table, and what the build cannot observe",
        ),
        (
            STAGE_SUMMARY_FILE,
            "the same stage arrays lifted out per run, plus the pooled buckets",
        ),
        (
            TRACES_FILE,
            "§9's raw layer for this arm: one line per simulation, with its window, \
            height, endpoint digest and every call row, which is what the tables above rebuild from",
        ),
        (
            OUTSIDE_FILE,
            "the lifecycle's own calls, pooled per run and never across two clocks, \
            beside the reads no sink in this build can reach",
        ),
        (
            ACCOUNT_MATRIX_FILE,
            "M8.3.2 §10's matrix: one row per address a run read, split by which \
            of the three legs asked, and `required_by` beside it",
        ),
        (
            STORAGE_BREAKDOWN_FILE,
            "M8.3.2 §8's storage reads per slot, and the same rows grouped by address",
        ),
        (
            DUPLICATES_FILE,
            "M8.3.2 §9's duplicate tally per source: the state reads that repeat a \
            key and so cost no request, which is why `rpc_count` is not a count of reads",
        ),
        (
            RPC_GAPS_FILE,
            "M8.3.2 §7's waits between one simulation's consecutive calls, per \
            simulation and pooled",
        ),
        (
            RPC_SUMMARY_FILE,
            "the per-method and per-source call tallies, and the sources that \
            refused to be traced",
        ),
        (
            SIMULATION_SUMMARY_FILE,
            "one row per simulation: window, call count, duration and cache stats",
        ),
        (
            BOTTLENECK_FILE,
            "§17's classification: every candidate word with the threshold that \
            fired it and the ones that did not",
        ),
    ] {
        lines.push(format!("- `{file}` — {answers}"));
    }
    lines.push(
        "Every rank in these files has `minimum_samples_for_rank` beside it, so a p90 over three \
         samples is `null` and says so rather than estimating."
            .to_string(),
    );
    lines.push(String::new());
    lines.push(format!(
        "The three files the runs wrote and this assembly did not: `{CORRECTNESS}/` (§16's \
         baseline/instrumented/comparison triple, from \n\
         `crates/simulation/tests/storage_dependency_experiment.rs`), `{FIXED_BLOCK}/` (§14's three \
         fixture runs, same source), and `{ROUTE_RUNS}/` (the ladder's own record per live run, \
         from `crates/cli`). The nodes of the dependency graph are identified as \
         `<run>:<simulation>:rpc<n>`, so a node id from one arm is never comparable with one from \
         another and the cross-arm check above compares verdicts, not ids."
    ));
    lines.push(String::new());
    lines.push("## 13. What this directory is not".to_string());
    lines.push(
        "Not an optimization plan, and not a result. It records calls and the order the engine \
         asked for them in, and it records them the same way whether or not a sink is attached — \
         which §16 is the record of. §22's completion report is where the eight questions this \
         milestone was asked get answered in prose, and §28's rule — 「没有证明 dependency，就不能 \
         说 independent」 — is the one line these tables were built to make checkable."
            .to_string(),
    );
    lines.push(String::new());
    let _ = nodes;
    let _ = edges;
    lines.join("\n") + "\n"
}

/// §19's pair total, read out of the map's own `pair_summary`.
fn pair_total(map: &Value) -> u64 {
    pair_sum(map, None)
}

fn pair_count(map: &Value, relation: &str) -> u64 {
    pair_sum(map, Some(relation))
}

fn pair_sum(map: &Value, relation: Option<&str>) -> u64 {
    map["pair_summary"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|row| relation.is_none_or(|name| row["relation"].as_str() == Some(name)))
        .map(|row| row["pairs"].as_u64().unwrap_or(0))
        .sum()
}

/// §15 and §27's input rule: three live runs and three fixture runs, one build, one mode, one
/// endpoint for the live arm, three distinct real blocks, and no fixture run on a live block.
#[test]
fn three_live_runs_of_one_build_are_the_input() {
    let runs = Run::live_runs();
    let fixtures = Run::fixture_runs();
    let both: Vec<&Run> = runs.iter().chain(fixtures.iter()).collect();

    let revisions: Vec<&str> = both.iter().map(|run| run.git_revision()).collect();
    for revision in &revisions {
        assert_eq!(
            revision, &revisions[0],
            "both arms ran on one commit or §27's comparison is between two builds: {revisions:?}"
        );
    }
    for run in &both {
        assert_eq!(
            run.execution_mode(),
            "build-only",
            "{}: §15's BuildOnly — a run in another mode is an experiment nobody asked for",
            run.name
        );
        assert_eq!(
            run.source(),
            if run.arm == RUNS { "live" } else { "fixture" },
            "{}: the arms are only separable by what they say they are, so a live run has to \
             report `live` and a fixture run `fixture`",
            run.name
        );
        assert_eq!(
            run.lines.len(),
            1,
            "{}: §14/§15's runs are one simulation each",
            run.name
        );
        assert_eq!(
            run.line()["state_read_concurrency"]["configured"].as_u64(),
            Some(1),
            "{}: §14's arm runs at the default bound, and the field that says so is published \
             per simulation rather than per directory",
            run.name
        );
    }

    let mut blocks: Vec<u64> = runs.iter().map(Run::block).collect();
    blocks.sort_unstable();
    let distinct = {
        let mut sorted = blocks.clone();
        sorted.dedup();
        sorted.len()
    };
    assert_eq!(
        distinct, LIVE_RUNS,
        "three live runs on three blocks, §15-style: {blocks:?}"
    );
    let mut endpoints: Vec<String> = runs.iter().map(Run::endpoint_id).collect();
    endpoints.sort();
    endpoints.dedup();
    assert_eq!(
        endpoints.len(),
        1,
        "one endpoint for the live arm, named here by digest rather than by URL: {endpoints:?}"
    );
    let fixture_block = fixtures[0].block();
    for fixture in &fixtures {
        assert_eq!(
            fixture.block(),
            fixture_block,
            "§14's arm is one block replayed, so a run of it on another height is not the \
             controlled arm any more"
        );
    }
    for run in &runs {
        assert_ne!(
            run.block(),
            fixture_block,
            "and no live run may land on the fixture's block — that would make the two arms one \
             arm and §14's control meaningless"
        );
    }
}

/// §6 and §27's reproducibility gate: the committed root is what the three live runs' raw rows,
/// re-fed through the writer a run uses, produce — byte for byte.
#[test]
fn the_committed_root_is_a_reassembly_of_the_runs_that_measured_it() {
    let runs = Run::live_runs();
    let dir = scratch("byte-gate");
    let names = assemble_to(&dir, &runs);
    assert_the_listing(&dir, &names);

    if refreshing() {
        for name in &names {
            std::fs::copy(dir.join(name), evidence_dir().join(name)).unwrap_or_else(|error| {
                panic!("{name}: the refresh could not be committed: {error}")
            });
        }
        eprintln!(
            "refreshed {EVIDENCE_DIR} from {} runs: {}",
            runs.len(),
            names.join(", ")
        );
        return;
    }

    let (committed_files, committed_dirs) = committed_listing();
    assert_eq!(
        committed_files, names,
        "{EVIDENCE_DIR}: the committed file set is not what one assembly writes"
    );
    let mut expected_dirs = vec![
        CORRECTNESS.to_string(),
        FIXED_BLOCK.to_string(),
        ROUTE_RUNS.to_string(),
        RUNS.to_string(),
    ];
    expected_dirs.sort();
    assert_eq!(
        committed_dirs, expected_dirs,
        "{EVIDENCE_DIR}: an arm appeared or vanished — the root's tables are gated against \
         `{RUNS}` only, so a directory added here would be pooled by nothing and checked by less"
    );
    for name in &names {
        assert_eq!(
            read_bytes(&dir.join(name)),
            read_bytes(&evidence_dir().join(name)),
            "{name} differs from a fresh assembly of the runs: either a figure in it was edited, \
             or `diagnosis.rs` changed after these files were written and they have to be \
             refreshed from the runs that measured them"
        );
    }
}

/// The same assembly twice. A pooled file stamped at the moment of writing, or a table whose rows
/// came out in hash order, would pass the gate above every time nobody ran it twice.
#[test]
fn two_assemblies_of_the_same_runs_write_byte_identical_files() {
    let (first, names) = fresh_assembly("repeat-a");
    let (second, second_names) = fresh_assembly("repeat-b");
    assert_eq!(
        names, second_names,
        "two assemblies of the same runs wrote different file sets"
    );
    for name in &names {
        assert_eq!(
            read_bytes(&first.join(name)),
            read_bytes(&second.join(name)),
            "{name} differs between two assemblies of the same runs"
        );
    }
}

/// §6's 「所有 aggregate 必须能够从 raw evidence 重建」 as a data claim and not only a byte claim:
/// the root's nodes are the runs' nodes, its edges theirs, and its summary theirs added.
#[test]
fn the_pooled_tables_are_the_runs_own_rows_and_nothing_else() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("rebuild");
    let root = read_root(&dir);
    let map = root_table(&root, DEPENDENCY_MAP_FILE);

    let pooled_nodes = map["nodes"].as_array().expect("a node list");
    let from_runs: Vec<Value> = runs
        .iter()
        .flat_map(|run| {
            run.dependency_map["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        pooled_nodes.len(),
        from_runs.len(),
        "the pooled node list is the runs' node lists end to end"
    );
    for (pooled, original) in pooled_nodes.iter().zip(&from_runs) {
        assert_eq!(
            pooled, original,
            "a pooled node that is not verbatim one run's node is a second derivation of the \
             rows, which is what §6 forbids: {}",
            pooled["node_id"]
        );
    }
    let pooled_edges: usize = map["edges"].as_array().expect("an edge list").len();
    let edges_from_runs: usize = runs
        .iter()
        .map(|run| {
            run.dependency_map["edges"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(
        pooled_edges, edges_from_runs,
        "the pooled edge list likewise"
    );

    assert_eq!(
        map["summary"]["total"].as_u64(),
        Some(runs.iter().map(Run::storage_reads).sum()),
        "the pooled total is the runs' totals added"
    );
    for (key, pick) in [("independent", 0_usize), ("ordered", 1), ("unknown", 2)] {
        let sum: u64 = runs
            .iter()
            .map(|run| {
                let (independent, ordered, unknown) = run.dependency_counts();
                [independent, ordered, unknown][pick]
            })
            .sum();
        assert_eq!(
            map["summary"][key].as_u64(),
            Some(sum),
            "the pooled `{key}` count is the runs' `{key}` counts added"
        );
    }

    // §9's raw layer and §4's rows likewise: one pooled row per run row, nothing invented.
    let calls = root_table(&root, PIPELINE_CALLS_FILE)["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        calls.len(),
        runs.iter().map(|run| run.calls.len()).sum::<usize>(),
        "{PIPELINE_CALLS_FILE} is the runs' rows, so its count is theirs added"
    );
    let storage_rows = root_table(&root, STORAGE_READS_FILE)["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        storage_rows.len(),
        from_runs.len(),
        "and the §4 rows file is the map's own nodes, not a second pass over the lines"
    );
    for run in &runs {
        assert_eq!(
            run.bucket_count("simulation"),
            Some(SIMULATION_CALLS),
            "{}: §17's simulation bucket holds this run's whole state-acquisition call list",
            run.name
        );
        let bucket_total: u64 = run.pipeline_summary["per_run"][0]["pipeline_buckets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|row| row["rpc_count"].as_u64().unwrap_or(0))
            .sum();
        // §19's buckets cover every stage the code *named*; a call it never stamped is in the
        // totals and in `calls_without_a_stamped_stage`, and in no bucket. Adding that second
        // number rather than dropping it is what keeps this a check instead of a rounding.
        let unstamped = run.totals()["calls_without_a_stamped_stage"]
            .as_u64()
            .unwrap_or(0);
        assert_eq!(
            bucket_total + unstamped,
            run.totals()["rpc_count"].as_u64().unwrap_or(0),
            "{}: {} bucket rows + {unstamped} unstamped call(s) and the run's own call count are \
             two ways of counting the same rows, so they cannot disagree — and a bucket sum that \
             left the unstamped calls out would be the silent gap this milestone is here to name",
            run.name,
            bucket_total
        );
    }
}

/// §9's raw row is the file a reader joins everything else to, so the column that says what a \
/// call was about has to be named after the value it actually holds. `RpcCallEvent::target` is \
/// the account, contract or hash a call names — and for `eth_getBlockByNumber` the issuing code \
/// puts the *height* there, which is why this milestone's first draft published those rows under \
/// `address` and read `0x37674203` and `0xlatest` as if they named accounts. §28's 「没有测到，就 \
/// 记录 not observed」 covers a field as much as a table: a name claiming something the call did \
/// not do is the failure, so the column is `target` and this gate is the check that keeps it \
/// shaped like its method rather than a restatement of the fix.
#[test]
fn a_raw_call_row_names_its_target_column_after_the_value_it_holds() {
    const ACCOUNT_METHODS: [&str; 5] = [
        "eth_getBalance",
        "eth_getCode",
        "eth_getStorageAt",
        "eth_getTransactionCount",
        "eth_call",
    ];
    let mut rows = 0_usize;
    let mut account_targets = 0_usize;
    let mut block_targets = 0_usize;
    let runs = Run::live_runs();
    let fixtures = Run::fixture_runs();
    for run in runs.iter().chain(fixtures.iter()) {
        for row in &run.calls {
            rows += 1;
            let method = row["method"].as_str().unwrap_or("?");
            assert!(
                row.get("address").is_none(),
                "{}: a {} row carries an `address` key. The column that says what a call names \
                 is `target`; an `address` on a height or a hash would be a field whose name \
                 claims an account the call never had",
                run.name,
                method
            );
            let target = row["target"].as_str();
            if ACCOUNT_METHODS.contains(&method) {
                let target = target.unwrap_or("<absent>");
                assert!(
                    target.len() == 42
                        && target.starts_with("0x")
                        && target[2..]
                            .chars()
                            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{}: an account-naming {} row carries target `{target}`, which is not one \
                     lowercased 20-byte account — the case §6's grouping depends on",
                    run.name,
                    method
                );
                account_targets += 1;
            } else if method == "eth_getBlockByNumber" {
                let target = target.unwrap_or("<absent>");
                assert_eq!(
                    target,
                    row["block_tag"].as_str().unwrap_or("<absent>"),
                    "{}: a block row's target is the height or tag it asked for, so it must read \
                     exactly like the row's own `block_tag` and not like an account",
                    run.name
                );
                block_targets += 1;
            } else {
                assert!(
                    target.is_none(),
                    "{}: a {} row names target `{}` — this build has no rule for what that call \
                     is about, so the honest value is null and not a borrowed word",
                    run.name,
                    method,
                    target.unwrap_or("<absent>")
                );
            }
        }
    }
    assert!(
        account_targets > 0 && block_targets > 0,
        "{account_targets} account rows and {block_targets} block rows out of {rows} read: a gate \
         that never opens one of its arms is not checking anything, and this directory has both \
         kinds in every run"
    );
}

/// §3, §5 and §27's first line: nothing is `independent`, an `unknown` is only ever the first read \
/// of a simulation, every `ordered` read names what it waited behind, and no verdict travels \
/// without the evidence text beside it.
#[test]
fn no_read_is_promoted_to_independent_and_unknown_stays_first() {
    let runs = Run::live_runs();
    let fixtures = Run::fixture_runs();
    for run in runs.iter().chain(fixtures.iter()) {
        let nodes = run.dependency_map["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            nodes.len() as u64,
            run.storage_reads(),
            "{}: the map's node list and its summary count disagree",
            run.name
        );
        let mut unknowns: Vec<u64> = Vec::new();
        for node in &nodes {
            let dependency = node["dependency"].as_str().unwrap_or("?");
            assert!(
                ["ordered", "unknown"].contains(&dependency),
                "{}: read {} is `{dependency}` — §27 asks for 0 `independent` in this build, and \
                 §3 is what keeps an unproved read at `unknown` rather than wherever a batch would \
                 want it",
                run.name,
                node["node_id"]
            );
            let depends_on = node["depends_on"].as_array().cloned().unwrap_or_default();
            if dependency == "unknown" {
                assert!(
                    depends_on.is_empty(),
                    "{}: an `unknown` naming a read it waited for is an `ordered` read",
                    run.name
                );
                unknowns.push(node["index"].as_u64().unwrap_or(u64::MAX));
            } else {
                assert!(
                    !depends_on.is_empty(),
                    "{}: an `ordered` read with nothing it depends on is the word without the \
                     evidence §4 asks beside it",
                    run.name
                );
                let index = node["index"].as_u64().unwrap_or(0);
                let earlier = depends_on.iter().all(|prior| {
                    prior["node_id"]
                        .as_str()
                        .and_then(|id| id.rsplit(':').next())
                        .and_then(|tail| tail.parse::<u64>().ok())
                        .is_none_or(|prior_index| prior_index < index)
                });
                assert!(
                    earlier,
                    "{}: read {index} is `ordered` behind something that did not come first in \
                     this simulation's own order",
                    run.name
                );
            }
            assert_eq!(
                node["leg"].as_str(),
                Some("fiber"),
                "{}: every storage read of this route is on the interpreter's leg; a \
                 `materialised` read would be the audit pass, which is the leg §7 proves \
                 independence on and which asks no storage read here",
                run.name
            );
            assert_eq!(
                node["uses_prior_response"].as_str(),
                Some(match dependency {
                    // The leg's order makes a prior answer a *candidate*, which is as far as a
                    // file of request boundaries goes.
                    "ordered" => USES_PRIOR_RESPONSE_POSSIBLE,
                    // The first read has no prior answer to have used.
                    _ => USES_PRIOR_RESPONSE_NOT_DETERMINED,
                }),
                "{}: §4's fourth field says `{USES_PRIOR_RESPONSE_POSSIBLE}` where the leg's order \
                 makes a prior answer a candidate and `{USES_PRIOR_RESPONSE_NOT_DETERMINED}` where \
                 nothing was checked. Neither is allowed to say `yes`, because that would be a \
                 value claim built out of requests",
                run.name
            );
            assert!(
                node["evidence"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty()),
                "{}: read {} carries no evidence text, so its verdict is a word",
                run.name,
                node["node_id"]
            );
        }
        assert_eq!(
            unknowns,
            vec![0],
            "{}: the only `unknown` is this simulation's first storage read",
            run.name
        );
    }
}

/// §14 and §15 ask the same question through two transports; §27 requires the same answer. The
/// comparison is of verdicts, and the one number that differs — the extra slot the fixture's route
/// touches — is the thing this gate pins as *not* a finding.
#[test]
fn the_fixture_arm_and_the_live_arm_reach_the_same_dependency_verdict() {
    let runs = Run::live_runs();
    let fixtures = Run::fixture_runs();
    for run in runs.iter().chain(fixtures.iter()) {
        let (independent, ordered, unknown) = run.dependency_counts();
        assert_eq!(
            independent, 0,
            "{}: §27's 「所有 dependency 分类必须有 evidence」 and §20's ban on 「storage 是独立的」 \
             both come to this one count",
            run.name
        );
        assert_eq!(
            (ordered + unknown, unknown),
            (run.storage_reads(), 1),
            "{}: everything that is not `ordered` is the first read of the leg, and there is one \
             of those per simulation",
            run.name
        );
        let pairs = run.dependency_map["pair_summary"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let count = |relation: &str| -> u64 {
            pairs
                .iter()
                .find(|row| row["relation"].as_str() == Some(relation))
                .and_then(|row| row["pairs"].as_u64())
                .unwrap_or(0)
        };
        let total = run.storage_reads();
        assert_eq!(
            (count("independent"), count("contradiction")),
            (0, 0),
            "{}: a pair counted as independent is the promotion §3 forbids, and a contradiction \
             would be the map disagreeing with the runs it read",
            run.name
        );
        assert_eq!(
            count("ordered"),
            total * (total - 1) / 2,
            "{}: every pair of this run's storage reads is `ordered` — which is the whole of \
             §14's answer in one integer",
            run.name
        );
    }
    let fixture_reads: Vec<u64> = fixtures.iter().map(Run::storage_reads).collect();
    let live_reads: Vec<u64> = runs.iter().map(Run::storage_reads).collect();
    assert!(
        fixture_reads.windows(2).all(|pair| pair[0] == pair[1]),
        "the fixture arm is one block replayed, so it reads the same count every time: {fixture_reads:?}"
    );
    assert!(
        live_reads.windows(2).all(|pair| pair[0] == pair[1]),
        "so does this route on a live node: {live_reads:?}"
    );
    assert_ne!(
        fixture_reads[0], live_reads[0],
        "the two arms differ by the slots each route touches — §README's §3 says by how much, and \
         if this ever stops being true that sentence is the one to fix"
    );
}

/// §7, §10 and §11's other half: the split is published with its own boundary case, and what the
/// build cannot see is named rather than counted as none.
#[test]
fn the_pipeline_split_publishes_its_boundary_case_instead_of_a_clean_zero() {
    let runs = Run::live_runs();
    for run in &runs {
        let totals = run.totals();
        let (simulation, lifecycle) = run.calls_by_sink();
        assert_eq!(
            simulation + lifecycle,
            run.calls.len() as u64,
            "{}: {PIPELINE_CALLS_FILE} and the run's totals count different call lists",
            run.name
        );
        assert_eq!(
            simulation,
            run.simulation_calls(),
            "{}: the pipeline's simulation-sink count has to be the trace line's own",
            run.name
        );
        assert!(
            lifecycle > 0,
            "{}: §15 asks for the whole pipeline, and a live route run with no lifecycle calls \
             means the second sink never got attached",
            run.name
        );
        match (
            totals["pipeline_total_duration_ns"].as_u64(),
            totals["rpc_union_duration_ns"].as_u64(),
        ) {
            (Some(ladder), Some(union)) if union > ladder => {
                assert_eq!(
                    totals["union_extends_past_the_ladder_window_ns"].as_u64(),
                    Some(union - ladder),
                    "{}: the calls cover more than the ladder's window, so the excess has to be \
                     published — otherwise `non_rpc_duration_ns: {}` is read as 「there was no \
                     local time」, which is the opposite of what a clamped boundary case means",
                    run.name,
                    totals["non_rpc_duration_ns"]
                );
                assert_eq!(
                    totals["non_rpc_duration_ns"].as_u64(),
                    Some(0),
                    "{}: the clamped figure itself, beside the excess that explains it",
                    run.name
                );
            }
            (Some(_), Some(_)) => {
                assert!(
                    totals["union_extends_past_the_ladder_window_ns"].is_null(),
                    "{}: the ladder covers the calls, so there is nothing to publish and a number \
                     here would be an invention",
                    run.name
                );
            }
            other => panic!("{}: no ladder/union pair to check: {other:?}", run.name),
        }
        assert_eq!(
            (
                totals["max_concurrency"].as_u64(),
                totals["rpc_overlap_duration_ns"].as_u64()
            ),
            (Some(1), Some(0)),
            "{}: Q2's premise, measured on the wire rather than inferred from the map",
            run.name
        );
    }

    let (dir, _) = fresh_assembly("not-observed");
    let root = read_root(&dir);
    let pipeline = root_table(&root, PIPELINE_SUMMARY_FILE);
    let not_observed = pipeline["not_observed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !not_observed.is_empty(),
        "§11: a directory that could observe everything would be the suspicious one"
    );
    for row in &not_observed {
        assert!(
            !row["surface"].is_null() && !row["why"].is_null(),
            "a `not observed` row names both the surface and the reason: {row}"
        );
    }
    let surfaces: Vec<String> = not_observed
        .iter()
        .map(|row| shown(&row["surface"]))
        .collect();
    assert!(
        surfaces
            .iter()
            .any(|surface| surface.contains("queueing") || surface.contains("network")),
        "§20 forbids 「network latency」 as a conclusion, and the file-level statement of why is \
         that the transport was never separable: {surfaces:?}"
    );
    assert!(
        surfaces
            .iter()
            .any(|surface| surface.contains("market event")),
        "the event stream is the other surface a zero would have lied about: {surfaces:?}"
    );
}

/// §13's pinning, read where it holds and where it does not: every state read of a *simulation* is
/// at the block that simulation pinned, and the execution lane's freshness reads are counted
/// separately rather than folded into a bare `false`.
#[test]
fn every_simulation_state_read_named_the_block_its_run_pinned() {
    for run in Run::live_runs() {
        let pinned = run.block().to_string();
        let mut state_calls = 0;
        for call in run.line()["calls"].as_array().expect("a call list") {
            let method = call["method"].as_str().unwrap_or("?");
            if !STATE_METHODS.contains(&method) {
                continue;
            }
            state_calls += 1;
            let block = call["block"].as_str().unwrap_or_else(|| {
                panic!(
                    "{}: {method} carries no block parameter — §13's 「禁止 latest / pending」 is \
                     about state reads, and a silent absence is how a latest read would hide",
                    run.name
                )
            });
            assert_eq!(
                block, pinned,
                "{}: {method} went out at `{block}` while the simulation pinned {pinned}",
                run.name
            );
        }
        assert_eq!(
            state_calls as u64,
            SIMULATION_CALLS - 1,
            "{}: 38 of the 39 simulation calls are state reads and the header read is the other; \
             §13's rule is about state, so the count that has to move for it to break is 38",
            run.name
        );

        let pin = run.block_pin();
        let at_pin = pin["state_read_calls_at_this_runs_block"].as_u64();
        let at_other = pin["state_read_calls_at_another_height"].as_u64();
        let at_tag = pin["state_read_calls_at_a_tag_that_is_not_a_height"].as_u64();
        assert_eq!(
            pin["state_read_calls"].as_u64(),
            at_pin.map(|a| a + at_other.unwrap_or(0) + at_tag.unwrap_or(0)),
            "{}: the three counts are a partition of the run's state reads, which is the whole \
             point of publishing them beside the boolean",
            run.name
        );
        // The partition's top line, rebuilt from §9's rows rather than taken on trust: the
        // simulation's 38, plus whatever the lifecycle happened to ask at the same height.
        let at_pin_from_rows = run
            .calls
            .iter()
            .filter(|row| {
                STATE_METHODS.contains(&row["method"].as_str().unwrap_or("?"))
                    && row["block_tag"].as_str() == Some(pinned.as_str())
            })
            .count() as u64;
        assert_eq!(
            at_pin,
            Some(at_pin_from_rows),
            "{}: the pin table's 「at this run's block」 has to be the rows' own count",
            run.name
        );
        assert!(
            at_pin_from_rows >= SIMULATION_CALLS - 1,
            "{}: at least the simulation's 38 state reads are at the pin, by the paragraph above",
            run.name
        );

        let off_pin: Vec<&Value> = run
            .calls
            .iter()
            .filter(|row| {
                STATE_METHODS.contains(&row["method"].as_str().unwrap_or("?"))
                    && row["block_tag"].as_str() != Some(pinned.as_str())
            })
            .collect();
        assert_eq!(
            off_pin.len() as u64,
            at_other.unwrap_or(0) + at_tag.unwrap_or(0),
            "{}: the rows off the pin are the same ones those two counts describe",
            run.name
        );
        for row in &off_pin {
            assert_eq!(
                row["sink"].as_str(),
                Some(SINK_LIFECYCLE),
                "{}: an off-pin state read *inside* a simulation would be §13's actual violation; \
                 this one is on the execution lane's sink",
                run.name
            );
            assert!(
                ["preflight", "build"].contains(&row["stage"].as_str().unwrap_or("?")),
                "{}: the off-pin read {:?} is stage {:?} — the lifecycle's freshness reads are the \
                 known exception, and a new stage reading state off the pin would be a finding of \
                 a different kind",
                run.name,
                row["method"],
                row["stage"]
            );
        }
    }

    // The fixture arm has no execution lane, so its boolean means what it says.
    for run in Run::fixture_runs() {
        let pin = run.block_pin();
        assert_eq!(
            pin["all_state_reads_pinned"].as_bool(),
            Some(true),
            "{}: every state read of the offline arm is at the one block",
            run.name
        );
        assert_eq!(
            pin["state_read_calls_at_another_height"].as_u64(),
            Some(0),
            "{}: and the other two causes of a `false` are published as zero rather than absent",
            run.name
        );
    }
}

/// §12: instrumentation may not add one RPC. The check is a comparison against the previous
/// milestone's own committed run, so it is a difference rather than a remembered number.
#[test]
fn the_instrumentation_asked_the_node_for_nothing_extra() {
    let path = workspace_root()
        .join(PREVIOUS_MILESTONE)
        .join(RUNS)
        .join(BASELINE_RUN)
        .join(TRACES_FILE);
    let lines = trace_lines(&path);
    let previous = lines
        .first()
        .unwrap_or_else(|| panic!("{path:?}: M8.3.3's C1 run wrote no trace line to compare with"));
    let per_method = |line: &Value| -> Vec<(String, u64)> {
        line["methods"]
            .as_array()
            .expect("a per-method list")
            .iter()
            .map(|row| {
                (
                    row["method"].as_str().unwrap_or("?").to_string(),
                    row["count"].as_u64().unwrap_or(0),
                )
            })
            .collect()
    };
    let before = per_method(previous);
    assert_eq!(
        previous["call_count"].as_u64(),
        Some(SIMULATION_CALLS),
        "the control itself: {BASELINE_RUN} is the 39-call run §12 compares against"
    );

    for run in Run::live_runs() {
        assert_eq!(
            per_method(run.line()),
            before,
            "{}: §12's 「instrumentation 不允许新增任何 RPC」 — a method count that moved between \
             M8.3.3's C1 run and this milestone's live run means this build asked the node for \
             something that other build did not, and the extra stamps are not an explanation, \
             they are the thing being checked",
            run.name
        );
        assert_eq!(run.simulation_calls(), SIMULATION_CALLS, "{}", run.name);
        // And across both sinks, no call carries an attempt count above 1 unless the wire really
        // retried: §18's logical read vs physical attempt.
        // And across both sinks, §18's two layers stay apart: a row is one logical read, its
        // `attempts` array is one row per physical try, and the attempt number is that length.
        for row in &run.calls {
            let tries = row["attempts"].as_array().map_or(0, Vec::len);
            let attempt = row["attempt"].as_u64().unwrap_or(0);
            assert!(
                attempt >= 1,
                "{}: {} is a call row with no attempt number — a logical read that never went out \
                 belongs in neither §9's list nor §18's attempt count",
                run.name,
                shown(&row["method"])
            );
            assert_eq!(
                attempt as usize,
                tries.max(1),
                "{}: {} says {attempt} attempt(s) and carries {tries} try records",
                run.name,
                shown(&row["method"])
            );
            assert_eq!(
                row["attempts_recorded"].as_bool(),
                Some(tries > 0),
                "{}: {} says whether its tries were recorded, and the array disagrees",
                run.name,
                shown(&row["method"])
            );
        }
    }

    // The fixture arm: every storage read on the wire is a node, and vice versa.
    for run in Run::fixture_runs() {
        let storage = run.line()["methods"]
            .as_array()
            .expect("a per-method list")
            .iter()
            .find(|row| row["method"] == json!("eth_getStorageAt"))
            .unwrap_or_else(|| {
                panic!(
                    "{}: no storage row in the run's own per-method list",
                    run.name
                )
            });
        assert_eq!(
            storage["count"].as_u64(),
            Some(run.storage_reads()),
            "{}: the wire's storage count and the map's node count are one list",
            run.name
        );
    }
}

/// §16's correctness record: the fields the experiment published, one verdict per field, plus the
/// number that makes the pair a control rather than a tautology.
#[test]
fn the_instrumented_arm_and_the_silent_one_agree_field_by_field() {
    let comparison = read_json(&evidence_dir().join(CORRECTNESS).join(COMPARISON_FILE));
    assert_eq!(
        comparison["milestone"].as_str(),
        Some("M8.4.1 §16"),
        "the correctness record has to say which milestone and which section it answers, and a \
         record stamped anything else is from a different build of the experiment"
    );
    let fields = comparison["fields"]
        .as_object()
        .expect("a field-by-field comparison");
    assert!(
        fields.len() >= 20,
        "§16 asks for the simulation result compared field by field, and {} fields is not that",
        fields.len()
    );
    for (name, row) in fields {
        assert_eq!(
            row["identical"].as_bool(),
            Some(true),
            "§16/§27: `{name}` differs between the instrumented arm and the silent one — this is \
             the record of a build whose observation changed an outcome"
        );
        assert_eq!(
            row["baseline"], row["instrumented"],
            "`{name}` says identical and the two sides are not the same value"
        );
    }
    for name in ["fingerprint", "gas_used", "outcome", "net_profit", "logs"] {
        assert!(
            fields.contains_key(name),
            "§16's comparison lost the `{name}` field it exists to check"
        );
    }
    assert_eq!(
        comparison["whole_result"]["identical"].as_bool(),
        Some(true),
        "and the whole result beside its fields"
    );
    assert_eq!(
        comparison["calls"]["identical_in_order"].as_bool(),
        Some(true),
        "the endpoint saw the same calls, in the same order, in both arms"
    );
    assert_eq!(
        comparison["gates"]["the_instrument_recorded_every_call_it_made"].as_bool(),
        Some(true),
        "the instrumented arm recorded a row for every call it made — otherwise the two arms \
         having equal results would only say the sink stayed out of the way of a run it never saw"
    );
    assert_eq!(
        comparison["metrics"]["baseline"]["calls_recorded"].as_u64(),
        Some(0),
        "and the silent arm recorded nothing, which is what makes it the control"
    );
    assert_eq!(
        comparison["metrics"]["instrumented"]["calls_recorded"].as_u64(),
        comparison["metrics"]["instrumented"]["calls_on_the_wire"].as_u64(),
        "the instrumented arm's rows and its wire count are one-to-one"
    );
}

/// §15's 「不得 sign / broadcast」 and §24's secret rule, read out of this directory.
#[test]
fn nothing_was_signed_broadcast_or_written_into_these_files() {
    let mut signed = 0;
    let mut submitted = 0;
    for name in run_names(RUNS, LIVE_RUNS) {
        let dir = evidence_dir().join(ROUTE_RUNS).join(&name);
        signed += jsonl_lines(&dir.join(SIGNED_FILE));
        submitted += jsonl_lines(&dir.join(SUBMISSIONS_FILE));
        let route_run = read_json(&dir.join(ROUTE_RUN_FILE));
        assert_eq!(
            route_run["mode"].as_str(),
            Some("build-only"),
            "{name}: the ladder's own record says {:?}",
            route_run["mode"]
        );
        assert_eq!(
            route_run["successful_real_arbitrage"].as_bool(),
            Some(false),
            "{name}: §1's list ends at 真实套利, and this run says it did one"
        );
    }
    assert_eq!(
        (signed, submitted),
        (0, 0),
        "§15's build-only boundary: signature and submission files exist and are empty, which is \
         the shape that distinguishes 「did not happen」 from 「was not recorded」"
    );

    let files = walk_files(&evidence_dir());
    assert!(
        files.len() > 100,
        "the scan walked {} files, fewer than this directory holds — a listing rule that silently \
         misses a file turns a secret scan into a sentence",
        files.len()
    );
    // The endpoint's host name is in exactly two kinds of file, both written by the ladder rather
    // than by this milestone: `preflight.json` and `route-run.json` quote the URL in their
    // provenance note, as every route run has since M7. Every table — the runs' own, the pooled
    // root, the fixture and the correctness arm — names the endpoint by digest only, and that is
    // what this asserts. A scan that looked only for a key would miss that the URL is here at all;
    // one that forbade it outright would fail on files this milestone did not write.
    let mut host_files: Vec<String> = Vec::new();
    for path in &files {
        let text = String::from_utf8_lossy(&read_bytes(path)).to_string();
        if text.contains("giwa.io") {
            host_files.push(path.to_string_lossy().to_string());
        }
    }
    for shown in &host_files {
        assert!(
            shown.contains(&format!("{ROUTE_RUNS}/"))
                && (shown.ends_with(ROUTE_RUN_FILE) || shown.ends_with("preflight.json")),
            "{shown}: a host name outside the two ladder files that have always quoted the URL — \
             the tables in this directory carry the endpoint digest (`rpc-…`) and nothing else"
        );
    }
    assert_eq!(
        host_files.len(),
        run_names(RUNS, LIVE_RUNS).len() * 2,
        "the ladder files that quote the URL are two per live run; anything else is a new place \
         for a host name to appear, and anything fewer means one of these runs lost its provenance"
    );
    for path in files {
        let text = String::from_utf8_lossy(&read_bytes(&path)).to_string();
        let shown = path.to_string_lossy().to_string();
        assert!(
            !text.contains("\"eth_sendRawTransaction\""),
            "{shown}: a submission method called in build-only evidence"
        );
        assert!(
            !text.contains("0x5b3326dc"),
            "{shown}: the test wallet's private key belongs in an environment variable or an \
             out-of-repo file at sign time, and §24's scan says not in evidence"
        );
        assert!(
            !text.contains("\"private_key\""),
            "{shown}: a field named `private_key`. §24's rule is about the field as much as the \
             value — key material lives in the environment or an out-of-repo file at sign time, so \
             an evidence directory that holds no key either is the shape, not a key whose value \
             this scan happened not to recognise"
        );
        if shown.ends_with(".json") || shown.ends_with(".jsonl") {
            // Not a loop over the offenders: the first one is the finding, and the message names
            // the path it sits at.
            if let Some((key, value)) = floats_in(&text).into_iter().next() {
                panic!(
                    "{shown}: {key} is a float ({value}) — §13's figures are integer ns and \
                     per-mille numerators over denominators, so a reader can add them up"
                );
            }
        }
    }
}

/// Every JSON number in a text with a fractional part or an exponent, i.e. not an integer.
fn floats_in(text: &str) -> Vec<(String, String)> {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => floats_in_value(&value, String::new()),
        Err(_) => {
            let mut found = Vec::new();
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    found.extend(floats_in_value(&value, String::new()));
                }
            }
            found
        }
    }
}

fn floats_in_value(value: &Value, path: String) -> Vec<(String, String)> {
    match value {
        Value::Number(number) if number.as_u64().is_none() && number.as_i64().is_none() => {
            vec![(path, number.to_string())]
        }
        Value::Array(values) => values
            .iter()
            .enumerate()
            .flat_map(|(index, value)| floats_in_value(value, format!("{path}[{index}]")))
            .collect(),
        Value::Object(entries) => entries
            .iter()
            .flat_map(|(key, value)| {
                let joined = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                floats_in_value(value, joined)
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
    {
        let entry = entry.unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_files(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// §21's last line, and the rule M8.3.3 left as precedent: the README is generated, so a hand-edited
/// number in it fails the suite and its run list is the directory's listing.
#[test]
fn the_readme_is_generated_and_lists_the_runs_the_directory_holds() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("readme");
    let text = read_text(&dir.join(README_FILE));
    assert_eq!(
        text,
        read_text(&evidence_dir().join(README_FILE)),
        "the committed README is not the generated one"
    );
    // §2's table and only that one — §6 re-lists the same runs to say something about heights, and
    // a row-collecting scan that spans both finds six rows for three runs.
    let listed: Vec<String> = text
        .lines()
        .skip_while(|line| !line.starts_with("## 2. "))
        .take_while(|line| !line.starts_with("## 3. "))
        .filter(|line| line.starts_with("| route-"))
        .filter_map(|line| line.split('|').nth(1).map(|cell| cell.trim().to_string()))
        .collect();
    assert_eq!(
        listed,
        run_names(RUNS, LIVE_RUNS),
        "the README's run table is the directory's listing, row for row"
    );
    // §6 re-lists the same runs to say something about heights; a run missing there is a run whose
    // pin this directory stopped reporting, which is exactly the silent shrink the listing above
    // guards against in the other table.
    let pinned: Vec<String> = text
        .lines()
        .skip_while(|line| !line.starts_with("## 6. "))
        .take_while(|line| !line.starts_with("## 7. "))
        .filter(|line| line.starts_with("| route-"))
        .filter_map(|line| line.split('|').nth(1).map(|cell| cell.trim().to_string()))
        .collect();
    assert_eq!(
        pinned,
        run_names(RUNS, LIVE_RUNS),
        "the height table lists a different set of runs from the run table"
    );
    for run in &runs {
        assert!(
            text.contains(&run.block().to_string()),
            "{}: its block is missing, which §15's reader looks for per run",
            run.name
        );
    }
    for section in [
        "The three answers",
        "The runs",
        "The fixed-block arm and the live arm",
        "What the dependency map says read by read",
        "Where the run's RPC time is",
        "Which height every read named",
        "did the instrumentation change an answer",
        "Did these runs sign, broadcast, or spend anything",
        "What this directory cannot see",
        "How to regenerate this directory",
        "What may not be concluded from these tables",
        "Provenance and which file answers what",
        "What this directory is not",
    ] {
        assert!(
            text.lines()
                .any(|line| line.starts_with("## ") && line.contains(section)),
            "§21's README has no `{section}` section"
        );
    }
    assert!(
        text.contains("null"),
        "§11: an unsupported rank is printed as `null`, never as a blank a reader reads as zero"
    );
    assert!(
        text.contains("eth_sendRawTransaction"),
        "and the safety sentence names the method it says is absent"
    );
    // §12's file index is the section a reader arrives at with a filename in hand, so it has to
    // name every file the assembly writes and name none twice: a duplicated bullet is how a
    // missing one hides.
    for file in GENERATED_FILES.iter().filter(|name| **name != README_FILE) {
        let bullet = format!("- `{file}` —");
        assert!(
            text.lines()
                .filter(|line| line.starts_with(&bullet))
                .count()
                == 1,
            "§12's index does not have exactly one `{file}` bullet: an absent one is a file \
             nobody said answers what, and a second is the same file listed twice while another \
             one went unlisted"
        );
    }
    // The README's own run table has to agree with the tables, per column, or the file above it
    // would be a second answer. The column is found by its heading rather than by counting cells:
    // a table that gains a column mid-milestone shifts every hardcoded index by one, and an
    // index-only check then reads the neighbouring column and calls it a pass.
    let run_table: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("## 2. "))
        .take_while(|line| !line.starts_with("## 3. "))
        .filter(|line| line.starts_with('|'))
        .collect();
    let header: Vec<&str> = run_table
        .first()
        .unwrap_or_else(|| panic!("§2 has no table"))
        .split('|')
        .map(|cell| cell.trim())
        .collect();
    let column = |label: &str| -> usize {
        header
            .iter()
            .position(|cell| *cell == label)
            .unwrap_or_else(|| panic!("§2's header has no `{label}` column"))
    };
    let (at_independent, at_ordered, at_unknown) =
        (column("independent"), column("ordered"), column("unknown"));
    for run in &runs {
        let (independent, ordered, unknown) = run.dependency_counts();
        let row = run_table
            .iter()
            .find(|line| line.starts_with(&format!("| {}", run.name)))
            .unwrap_or_else(|| panic!("{}: no row", run.name));
        let cells: Vec<&str> = row.split('|').map(|cell| cell.trim()).collect();
        let at = |index: usize| cells.get(index).and_then(|cell| cell.parse::<u64>().ok());
        assert_eq!(
            (at(at_independent), at(at_ordered), at(at_unknown)),
            (Some(independent), Some(ordered), Some(unknown)),
            "{}: the README's dependency columns are not the map's",
            run.name
        );
    }
}
