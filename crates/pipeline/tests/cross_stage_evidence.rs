//! M8.4.2 §14's four root tables at the evidence layer: `data/evidence/m8/cross-stage/`,
//! assembled from the runs that measured them.
//!
//! §6's rule — 「所有 aggregate 必须能够从 raw evidence 重建」 — is why this file exists. The run
//! directories under `runs/` hold the raw: each simulation's trace line, and
//! [`PIPELINE_CALLS_FILE`]'s row per call of both sinks. The root's four tables are those rows fed
//! back through [`cross_stage_tables`] — the same fold the runs' own directories used — and the byte
//! gate below asks whether the committed root *is* that re-assembly. There is no measured figure and
//! no arithmetic in this file: §19's 「不要手写数字」 is enforced by having nothing here that could
//! disagree with `canonicalization.rs`.
//!
//! ## Why five files at the root instead of sixteen
//!
//! §14 names four outputs, and the writer's ten generic files are already in every run directory
//! beside them. A root that held an empty copy of the generic ten would be a file a reader could
//! mistake for a measurement, so §31's 「遵循仓库现有 evidence convention」 is followed on the rule
//! rather than the listing: one directory per milestone, a generated README, raw rows under `runs/`,
//! and pooled tables at the root — and the pooled tables here are this milestone's four.
//!
//! ## Where each § is graded
//!
//! §11 (three live runs, one build, no new optimisation) and §9 (six read categories, `eth_chainId`
//! not a state read) are facts about these runs, so they are graded here. §12's reproducibility is
//! graded in two places: [`crates/simulation/tests/cross_stage_experiment.rs`] regenerates three
//! fixed-block arms and compares their tables, and
//! [`crates/pipeline/tests/cross_stage_recompute.rs`] folds a live corpus in-process; this file adds
//! the third reading — the *committed* fixed-block arms answer the same way. §13's named pairs,
//! §16's per-dimension cells, §17's intra/cross separation, §18's cache boundary, §23's block rule,
//! §25's retry rule, §26's no-extra-call control and §28's safety boundary are all read out of the
//! committed tree, because each is a claim about what this directory says.
//!
//! [`crates/simulation/tests/cross_stage_experiment.rs`]: that file's `evidence_root()` publishes
//! `fixed-block/` and `correctness/` here under `M842_FIXTURE_EVIDENCE`.
//! [`crates/pipeline/tests/cross_stage_recompute.rs`]: `cross_stage_recompute.rs`, same crate.
//!
//! ## The number this directory cannot produce, and why that is reported rather than fixed
//!
//! `safe_to_reuse` is 0 in every table here. Two of §6's five conditions — lifecycle ownership and
//! whether the consumer needs a fresh answer — are not fields of a recorded call, so no pair in this
//! corpus can resolve them to met. That is §7's 「数据相等不能证明可以复用」 arriving as a measurement,
//! and the gates below check that the tables *say* it (each row carries its five conditions) rather
//! than letting a zero read as a clean pass. Nothing in this milestone's evidence argues for a cache:
//! §35 forbids designing the experiment to reach that conclusion, and [`the_readme_reports_duplicates_and_candidates_without_recommending_a_cache`]
//! checks the generated prose does not either.
//!
//! ## How these files change
//!
//! Assembly happens under `target/pipeline-tests/`; `M842_CROSS_STAGE_REFRESH=1` copies a fresh
//! assembly over the committed root files, which is the only way they change. `runs/`,
//! `route-runs/`, `fixed-block/` and `correctness/` are the runs' own output — written by
//! `crates/cli` and by the simulation crate's experiment — and nothing here writes or deletes them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::canonicalization::{
    cross_stage_tables, CROSS_STAGE_RUN_FILE, DUPLICATE_CLASSES, DUPLICATE_EXACT,
    DUPLICATE_MATRIX_FILE, DUPLICATE_SUMMARY_FILE, NAMED_STAGE_PAIRS, NOT_A_DUPLICATE,
    PAIR_MEASURED, PAIR_NO_ASKS_OBSERVED, PAIR_NO_DUPLICATES, PAIR_STAGE_ABSENT,
    READ_CATEGORY_BLOCK_READ, READ_CATEGORY_CHAIN_IDENTITY, READ_CATEGORY_STATE_READ,
    REUSE_CANDIDATES_FILE, SAME_TARGET_BLOCK_UNDETERMINED, SAME_TARGET_DIFFERENT_BLOCK,
    SCOPE_CROSS_STAGE, SCOPE_INTRA_STAGE, STAGE_PAIRS_FILE,
};
use evm_pipeline::diagnosis::{
    CROSS_STAGE_FILES, DEPENDENCY_FILES, DUPLICATES_FILE, OUTSIDE_FILE, PIPELINE_CALLS_FILE,
    README_FILE, RPC_SUMMARY_FILE, TRACES_FILE,
};

const EVIDENCE_DIR: &str = "data/evidence/m8/cross-stage";
/// §11's live arm, §12's fixed-block arm, the ladder's own records beside them, and §27's
/// correctness pair.
const RUNS: &str = "runs";
const FIXED_BLOCK: &str = "fixed-block";
const ROUTE_RUNS: &str = "route-runs";
const CORRECTNESS: &str = "correctness";
const ROUTE_RUN_FILE: &str = "route-run.json";
const SIGNED_FILE: &str = "signed-transactions.jsonl";
const SUBMISSIONS_FILE: &str = "submissions.jsonl";

/// The previous milestone's own committed tree, read for one thing only: which RPC methods a run of
/// this CLI made before §diagnose-cross-stage existed. §26's 「instrumentation → observe，不能
/// instrumentation → new RPC」 is a claim about a *new kind of ask*, and a method list is the field
/// that shows one — counts are not, since they follow the head a run landed on (§11 allows different
/// heads, and M8.4.1's own three runs differ: 71, 82, 82).
const PREVIOUS_MILESTONE: &str = "data/evidence/m8/storage-dependency";

/// §11's 「至少 3 次 live run」, and §12's three.
const LIVE_RUNS: usize = 3;
const FIXTURE_RUNS: usize = 3;

/// The four methods whose name alone says it reads state — §9's `state_read` covers exactly these
/// plus whatever a caller asks an `eth_call` to read, which is a property of the caller and not of
/// the method name (the unit test `read_categories_follow_the_ask_not_the_method_name` covers that
/// half).
const STATE_METHODS: [&str; 4] = [
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getTransactionCount",
];
/// §9's block reads: a height or a header, asked of the node.
const BLOCK_METHODS: [&str; 2] = ["eth_getBlockByNumber", "eth_blockNumber"];
/// §9's explicit example of a read that is not a state read.
const CHAIN_ID_METHOD: &str = "eth_chainId";

/// Everything one assembly of the live runs writes at this root: §14's four tables and this
/// milestone's generated README.
const GENERATED_FILES: [&str; 5] = [
    README_FILE,
    DUPLICATE_MATRIX_FILE,
    DUPLICATE_SUMMARY_FILE,
    REUSE_CANDIDATES_FILE,
    STAGE_PAIRS_FILE,
];

/// The standing empty list a missing or non-array `rows` key falls back to. A `&Vec::new()` written
/// at each call site is a temporary that dies before the borrow of its elements does.
static NO_ROWS: Vec<Value> = Vec::new();

/// `M842_CROSS_STAGE_REFRESH` names no directory: its presence is the instruction to copy a fresh
/// assembly over the committed evidence. Nothing else in a test writes there.
fn refreshing() -> bool {
    std::env::var_os("M842_CROSS_STAGE_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_DIR)
}

fn previous_dir() -> PathBuf {
    workspace_root().join(PREVIOUS_MILESTONE)
}

fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    dir
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn trace_lines(path: &Path) -> Vec<Value> {
    let text = String::from_utf8_lossy(&read_bytes(path)).to_string();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect()
}

fn jsonl_lines(path: &Path) -> usize {
    let text = String::from_utf8_lossy(&read_bytes(path)).to_string();
    text.lines().filter(|line| !line.trim().is_empty()).count()
}

fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// A JSON table as every other evidence file in this repository holds it: two-space
/// pretty-printing and a trailing newline. serde_json without `preserve_order` sorts object keys,
/// which is why a re-assembly of the same runs is byte-reproducible.
fn write_table(path: &Path, table: &Value) {
    let mut text = serde_json::to_string_pretty(table).unwrap_or_else(|error| panic!("{error}"));
    text.push('\n');
    write_text(path, &text);
}

/// The directories of one arm, sorted, each checked to be a run and not a stray file: a directory
/// silently left out of the assembly would leave the root with fewer rows than the tree holds.
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
    dir: PathBuf,
    lines: Vec<Value>,
    /// Both sinks' rows, verbatim — the whole input of every table this milestone publishes.
    calls: Vec<Value>,
    /// The directory header, which every table in it carries.
    rpc_summary: Value,
    per_run: Value,
    matrix: Value,
    summary: Value,
    candidates: Value,
    stage_pairs: Value,
    /// The ladder's own record. `Null` for the fixture arm, which drives a simulation directly and
    /// never enters the route, so has no sign/broadcast files to count.
    route_run: Value,
}

impl Run {
    fn read(arm: &'static str, name: &str) -> Self {
        let dir = evidence_dir().join(arm).join(name);
        let at = |file: &str| dir.join(file);
        for file in CROSS_STAGE_FILES {
            assert!(
                at(file).exists(),
                "{EVIDENCE_DIR}/{arm}/{name} has no `{file}`: this milestone's switch was on for \
                 these runs, so a directory missing one of §14's tables is a run that stopped \
                 answering §1's question rather than a run that answered it with zero"
            );
        }
        assert!(
            at(CROSS_STAGE_RUN_FILE).exists(),
            "{EVIDENCE_DIR}/{arm}/{name} has no `{CROSS_STAGE_RUN_FILE}`: §12's per-run pair list \
             is what every pooled figure in this directory can be traced back to"
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
            rpc_summary: read_json(&at(RPC_SUMMARY_FILE)),
            per_run: read_json(&at(CROSS_STAGE_RUN_FILE)),
            matrix: read_json(&at(DUPLICATE_MATRIX_FILE)),
            summary: read_json(&at(DUPLICATE_SUMMARY_FILE)),
            candidates: read_json(&at(REUSE_CANDIDATES_FILE)),
            stage_pairs: read_json(&at(STAGE_PAIRS_FILE)),
            route_run,
            dir,
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

    /// The one simulation this run recorded; §11 and §12's runs are one simulation each, so a run
    /// with a second line would have its first read by every accessor below and the count is
    /// checked rather than assumed.
    fn line(&self) -> &Value {
        assert_eq!(
            self.lines.len(),
            1,
            "{}: §11/§12's runs are one simulation each and this one wrote {} trace lines",
            self.name,
            self.lines.len()
        );
        &self.lines[0]
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

    fn source(&self) -> Value {
        self.line()["source"].clone()
    }

    fn chain_id(&self) -> Value {
        self.line()["chain_id"].clone()
    }

    fn block_number(&self) -> Value {
        self.line()["block_number"].clone()
    }

    fn endpoint_id(&self) -> String {
        self.line()["endpoint_id"]
            .as_str()
            .unwrap_or("?")
            .to_string()
    }

    /// §11's settings, read off the run rather than restated: the reuse switch on, the concurrency
    /// bound at 1, and no new optimisation asked for.
    fn state_read_reuse(&self) -> Value {
        self.line()["state_read_cache"]["reuse"].clone()
    }

    fn configured_concurrency(&self) -> Value {
        self.line()["state_read_concurrency"]["configured"].clone()
    }

    fn cache_hits(&self) -> u64 {
        self.line()["state_read_cache"]["cache_hits"]
            .as_u64()
            .unwrap_or(0)
    }

    fn asks(&self) -> u64 {
        self.per_run["asks"].as_u64().unwrap_or(0)
    }

    fn pairs(&self) -> u64 {
        self.per_run["pairs"].as_u64().unwrap_or(0)
    }

    fn candidates_count(&self) -> u64 {
        self.candidates["candidates"].as_u64().unwrap_or(0)
    }

    fn safe_to_reuse(&self) -> u64 {
        self.candidates["safe_to_reuse"].as_u64().unwrap_or(0)
    }

    /// The raw row [`cross_stage_tables`] takes: this run's own call rows plus the provenance it
    /// published for them. One of these per run is the whole input of the pooled tables, which is
    /// what makes the root a re-assembly rather than a re-measurement.
    fn cross_stage_source(&self) -> Value {
        json!({
            "run": self.name,
            "source": self.source(),
            "chain_id": self.chain_id(),
            "block_number": self.block_number(),
            "endpoint_id": self.endpoint_id(),
            "execution_mode": self.execution_mode(),
            "git_revision": self.git_revision(),
            "generated_at_unix_ms": self.generated_at(),
            "calls": self.calls,
        })
    }

    /// §6's provenance row for the root's four tables: where each pooled figure came from, in the
    /// integers and digests the run itself used.
    fn provenance(&self) -> Value {
        json!({
            "run": self.name,
            "directory": format!("{EVIDENCE_DIR}/{}/{}/", self.arm, self.name),
            "arm": if self.arm == RUNS { "§11 live" } else { "§12 fixed-block" },
            "source": self.source(),
            "chain_id": self.chain_id(),
            "block_number": self.block_number(),
            "endpoint_id": self.endpoint_id(),
            "git_revision": self.git_revision(),
            "execution_mode": self.execution_mode(),
            "generated_at_unix_ms": self.generated_at(),
            "simulations": self.lines.len(),
            "call_rows": self.calls.len(),
            "state_read_reuse": self.state_read_reuse(),
            "configured_concurrency": self.configured_concurrency(),
            "cache_hits": self.cache_hits(),
            "asks": self.asks(),
            "pairs": self.pairs(),
            "reuse_candidates": self.candidates_count(),
            "safe_to_reuse": self.safe_to_reuse(),
            "route_mode": self.route_run["mode"].clone(),
            "successful_real_arbitrage": self.route_run["successful_real_arbitrage"].clone(),
        })
    }
}

/// The four §14 tables, pooled over the runs, each carrying the provenance block the root's reader
/// needs (a run directory's tables carry the directory header instead; this root is assembled by a
/// gate, so it states its own origin in the file rather than in a header nobody wrote).
fn pooled_tables(records: &[Value], assembled_from: &[Value]) -> Vec<(&'static str, Value)> {
    let tables = cross_stage_tables(records);
    CROSS_STAGE_FILES
        .iter()
        .map(|name| {
            let mut table = tables
                .get(*name)
                .cloned()
                .unwrap_or_else(|| panic!("`cross_stage_tables` returned no {name} table"));
            if let Some(object) = table.as_object_mut() {
                object.insert("assembled_from".to_string(), json!(assembled_from));
                object.insert("root".to_string(), json!(format!("{EVIDENCE_DIR}/")));
            }
            (*name, table)
        })
        .collect()
}

/// The whole assembly: three live runs' rows re-fed through the fold a run uses, and this
/// milestone's generated README beside them.
fn assemble_to(dir: &Path, runs: &[Run]) -> Vec<String> {
    assert!(
        !runs.is_empty(),
        "there is nothing to assemble: {EVIDENCE_DIR}/{RUNS} holds no run directory"
    );
    let revisions: Vec<&str> = runs.iter().map(Run::git_revision).collect();
    assert!(
        revisions.windows(2).all(|pair| pair[0] == pair[1]),
        "§11's runs have to be one build run three times, and these are {revisions:?}"
    );
    let modes: Vec<&str> = runs.iter().map(Run::execution_mode).collect();
    assert!(
        modes.windows(2).all(|pair| pair[0] == pair[1]),
        "§11's runs have to be one mode, and these are {modes:?}"
    );

    let records: Vec<Value> = runs.iter().map(Run::cross_stage_source).collect();
    let assembled_from: Vec<Value> = runs.iter().map(Run::provenance).collect();
    for (name, table) in pooled_tables(&records, &assembled_from) {
        write_table(&dir.join(name), &table);
    }

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
        DUPLICATE_MATRIX_FILE: read_json(&dir.join(DUPLICATE_MATRIX_FILE)),
        DUPLICATE_SUMMARY_FILE: read_json(&dir.join(DUPLICATE_SUMMARY_FILE)),
        REUSE_CANDIDATES_FILE: read_json(&dir.join(REUSE_CANDIDATES_FILE)),
        STAGE_PAIRS_FILE: read_json(&dir.join(STAGE_PAIRS_FILE)),
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

/// §14's file set — what one assembly of the live runs writes, no more and no less. An extra file
/// is an artifact nothing asked for; a missing one is a question the directory stopped answering.
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

/// Every file under one directory, for the scans that must not miss a file to be meaningful.
fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{dir:?}: {error}"));
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// A figure as the README prints it: an absent one stays the word `null`, because a table that
/// printed an absent figure as a blank cell reads as a zero.
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

fn table_row(lines: &mut Vec<String>, cells: &[&str]) {
    lines.push(format!("| {} |", cells.join(" | ")));
}

fn separator_row(lines: &mut Vec<String>, columns: usize) {
    lines.push(format!("|{}|", vec!["---"; columns].join("|")));
}

/// The README every figure in is read out of the tables this assembly just wrote.
fn build_readme(runs: &[Run], root: &Value) -> String {
    let summary = root_table(root, DUPLICATE_SUMMARY_FILE);
    let candidates = root_table(root, REUSE_CANDIDATES_FILE);
    let pairs_table = root_table(root, STAGE_PAIRS_FILE);
    let matrix = root_table(root, DUPLICATE_MATRIX_FILE);

    let mut lines: Vec<String> = Vec::new();
    lines.push("# M8.4.2 — cross-stage RPC redundancy and reuse (evidence)".to_string());
    lines.push(String::new());
    lines.push(
        "The question §1 asks: 不同 pipeline stage 之间，是否存在可以安全复用的重复 RPC/state read？ \
         This directory answers it for three live runs of one build. It is a diagnosis: nothing here \
         caches, reuses across stages, schedules differently, batches, or prefetches, and no \
         duplicate found here has been removed."
            .to_string(),
    );
    lines.push(String::new());

    lines.push("## Headline".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- {} asks across {} runs, of which {} are the same ask as an earlier one (§4's duplicate classes).",
        shown(&summary["total_asks"]),
        shown(&summary["runs"]),
        shown(&summary["duplicate_pairs"]),
    ));
    lines.push(format!(
        "- {} of those pairs are reuse candidates (§6's level B); **{}** are safe to reuse (§6's level C).",
        shown(&summary["reuse_candidates"]),
        shown(&summary["safe_reuse_candidates"]),
    ));
    lines.push(format!(
        "- {} pairs are refused at §23's block rule: the two asks name different heights.",
        shown(&summary["refused_by_block_identity"]),
    ));
    lines.push(format!(
        "- Scope: {} cross-stage, {} intra-stage, {} same-logical-request (§17 keeps these apart).",
        shown(&summary["cross_stage_pairs"]),
        shown(&summary["intra_stage_pairs"]),
        shown(&summary["same_logical_request_pairs"]),
    ));
    lines.push(String::new());

    lines.push("## The runs".to_string());
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "run",
            "block",
            "endpoint",
            "execution mode",
            "state-read reuse",
            "concurrency",
            "cache hits",
            "call rows",
            "asks",
            "pairs",
            "candidates",
            "safe to reuse",
        ],
    );
    separator_row(&mut lines, 12);
    for run in runs {
        table_row(
            &mut lines,
            &[
                &run.name,
                &shown(&run.block_number()),
                &run.endpoint_id(),
                run.execution_mode(),
                &shown(&run.state_read_reuse()),
                &shown(&run.configured_concurrency()),
                &run.cache_hits().to_string(),
                &run.calls.len().to_string(),
                &run.asks().to_string(),
                &run.pairs().to_string(),
                &run.candidates_count().to_string(),
                &run.safe_to_reuse().to_string(),
            ],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "All {} runs are the same build (`{}`), the same chain, and build-only. A cache hit cost no \
         request and so has no call row: the tables count asks, never lookups — §18's \
         「cache hit ≠ physical duplicate」 (§30's limit; §11's settings).",
        runs.len(),
        runs[0].git_revision(),
    ));
    lines.push(String::new());

    lines.push("## Duplicate classes (§4)".to_string());
    lines.push(String::new());
    table_row(&mut lines, &["class", "pairs"]);
    separator_row(&mut lines, 2);
    for class in DUPLICATE_CLASSES {
        if class == NOT_A_DUPLICATE {
            continue;
        }
        table_row(
            &mut lines,
            &[class, &shown(&summary[format!("{class}_pairs").as_str()])],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "The class counts sum to {} of {} pairs; `{NOT_A_DUPLICATE}` is never counted, so a pair \
         that is not a duplicate of any class is not in this table at all (§4's sixth class is a \
         verdict about a pair of asks, not a pair this file lists).",
        DUPLICATE_CLASSES
            .iter()
            .filter(|class| **class != NOT_A_DUPLICATE)
            .map(|class| summary[format!("{class}_pairs").as_str()]
                .as_u64()
                .unwrap_or(0))
            .sum::<u64>(),
        shown(&summary["duplicate_pairs"]),
    ));
    lines.push(String::new());

    lines.push("## Per-stage direction (§5, §10)".to_string());
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "producer stage → consumer stage",
            "class",
            "scope",
            "pairs",
            "methods",
        ],
    );
    separator_row(&mut lines, 5);
    // One line per §14 matrix row: the row is already the finest cell the tables carry, so the
    // README prints it as read rather than re-grouping it into a second, looser figure.
    for row in matrix["rows"].as_array().unwrap_or(&NO_ROWS) {
        table_row(
            &mut lines,
            &[
                &format!(
                    "{} → {}",
                    shown(&row["producer_stage"]),
                    shown(&row["consumer_stage"])
                ),
                &shown(&row["duplicate_type"]),
                &shown(&row["scope"]),
                &shown(&row["pairs"]),
                &shown(&row["methods"]),
            ],
        );
    }
    lines.push(String::new());
    lines.push(format!(
        "The cells add to {} pairs, which is the headline figure above (§10's per-stage reading is \
         this table, §16's per-method one is in `{}`).",
        matrix["rows"]
            .as_array()
            .unwrap_or(&NO_ROWS)
            .iter()
            .map(|row| row["pairs"].as_u64().unwrap_or(0))
            .sum::<u64>(),
        DUPLICATE_SUMMARY_FILE,
    ));
    lines.push(String::new());

    lines.push("## The eleven named stage pairs (§13)".to_string());
    lines.push(String::new());
    table_row(
        &mut lines,
        &[
            "pair",
            "status",
            "pairs",
            "pairs the other way",
            "candidates",
            "safe to reuse",
        ],
    );
    separator_row(&mut lines, 6);
    for row in pairs_table["rows"].as_array().unwrap_or(&NO_ROWS) {
        table_row(
            &mut lines,
            &[
                &shown(&row["pair"]),
                &shown(&row["status"]),
                &shown(&row["pairs"]),
                &shown(&row["pairs_in_the_other_direction"]),
                &shown(&row["candidates"]),
                &shown(&row["safe_to_reuse"]),
            ],
        );
    }
    lines.push(String::new());
    lines.push(
        "A row reading `not_applicable_no_such_stage` is §13's own instruction not to invent data: \
         this build stamps no stage by that name. A measured zero is a different sentence, and the \
         two statuses are kept apart rather than both reported as `0`. The two directions are \
         separate cells: a zero across the pair and a non-zero back across it is one finding, not \
         a contradiction (§5)."
            .to_string(),
    );
    lines.push(String::new());

    lines.push("## What these numbers cannot say".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- `safe_to_reuse` is {} because two of §6's five conditions — who owns an answer after it \
         arrives, and whether the consumer needs a fresh one — are not fields of a recorded call. \
         Each row of `{}` carries all five conditions with the reason for each, so the zero is a \
         statement about this record, not about the pipeline.",
            shown(&candidates["safe_to_reuse"]),
        REUSE_CANDIDATES_FILE,
    ));
    lines.push(
        "- Equal data is not a licence to reuse (§7). Nothing here is a recommendation to build a \
         cache, and §30 forbids this report from becoming one."
            .to_string(),
    );
    lines.push(format!(
        "- `semantic_duplicate` is reported as `not_measurable_from_this_record`: the recorded row \
         holds the collapsed form of a call, so `latest` against `pending` cannot be told apart \
         after the fact (§4.2). The count of {} in these tables is a measurement of the record, not \
         a claim that no such pair happened.",
        shown(&summary["semantic_duplicate_pairs"]),
    ));
    let arms = Run::fixture_runs();
    let joined = |figures: Vec<u64>| {
        figures
            .iter()
            .map(|figure| figure.to_string())
            .collect::<Vec<String>>()
            .join(" / ")
    };
    let arm_asks = joined(
        arms.iter()
            .map(|arm| arm.per_run["asks"].as_u64().unwrap_or(0))
            .collect(),
    );
    let arm_pairs = joined(
        arms.iter()
            .map(|arm| arm.per_run["pairs"].as_u64().unwrap_or(0))
            .collect(),
    );
    let arm_first_asks = joined(
        arms.iter()
            .map(|arm| {
                arm.per_run["first_asks_for_their_target"]
                    .as_u64()
                    .unwrap_or(0)
            })
            .collect(),
    );
    let arm_stages = arms
        .iter()
        .map(|arm| {
            arm.per_run["asks_by_stage"]
                .as_array()
                .unwrap_or(&NO_ROWS)
                .iter()
                .map(|row| format!("{} {}", shown(&row["asks"]), shown(&row["stage"])))
                .collect::<Vec<String>>()
                .join(", ")
        })
        .collect::<Vec<String>>()
        .join(" | ");
    lines.push(format!(
        "- §12's {} fixed-block arms replay one pinned block offline and record {} asks each, in \
         the stage shapes {} — so the arms do carry two stage names, and the {} pairs between them \
         are measured, not avoided: every one of an arm's asks ({} per arm) is the first ask for \
         its own identity, which says nothing inside one simulation repeats. 「Which stages \
         overlap」 is answered by the live runs above, whose lifecycle spans more than one \
         stage; these arms answer 「does one block fold the same way twice」.",
        arms.len(),
        arm_asks,
        arm_stages,
        arm_pairs,
        arm_first_asks,
    ));
    lines.push(String::new());

    lines.push("## Files".to_string());
    lines.push(String::new());
    table_row(&mut lines, &["file", "what it holds"]);
    separator_row(&mut lines, 2);
    for (name, note) in [
        (
            README_FILE,
            "this file: the figures, the runs behind them, and what they cannot say",
        ),
        (
            DUPLICATE_MATRIX_FILE,
            "one row per (producer stage, consumer stage, class, scope), pooled",
        ),
        (
            DUPLICATE_SUMMARY_FILE,
            "§16's figures, broken out by method, producer stage and consumer stage",
        ),
        (
            REUSE_CANDIDATES_FILE,
            "§15's directed rows: duplicate, reusable, safe-to-reuse as three verdicts",
        ),
        (
            STAGE_PAIRS_FILE,
            "§13's eleven named pairs, each with a status and a reason",
        ),
    ] {
        table_row(&mut lines, &[name, note]);
    }
    lines.push(String::new());
    lines.push(format!(
        "Raw evidence: `{RUNS}/<run>/{CROSS_STAGE_RUN_FILE}` (one run's own pairs), \
         `{RUNS}/<run>/{PIPELINE_CALLS_FILE}` (every call row the tables were folded from), \
         `{FIXED_BLOCK}/run-01..03` (§12's three fixed-block arms), `{CORRECTNESS}/` (§27's \
         baseline against instrumented), `{ROUTE_RUNS}/` (the ladder's own records)."
    ));
    lines.push(String::new());

    lines.push("## How to regenerate".to_string());
    lines.push(String::new());
    lines.push(format!(
        "```\nM842_CROSS_STAGE_REFRESH=1 cargo test -p evm-pipeline --test cross_stage_evidence\n```\n\
         writes this README and the four tables from `{RUNS}/`. The runs themselves come from \
         `crates/cli` with `--diagnose-cross-stage`; regenerating them needs a live node."
    ));
    lines.push(String::new());

    lines.push("## Safety boundary (§28)".to_string());
    lines.push(String::new());
    lines.push(format!(
        "Every run here is build-only: {} of {} route records say `successful_real_arbitrage: \
         false`, and `route-runs/*/signed-transactions.jsonl` and \
         `route-runs/*/submissions.jsonl` exist and are empty. Nothing was signed, broadcast, or \
         spent; the endpoint appears in these tables only as a digest.",
        runs.iter()
            .filter(|run| run.route_run["successful_real_arbitrage"].as_bool() == Some(false))
            .count(),
        runs.len(),
    ));
    lines.push(String::new());
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// §11: three live runs of one build, still in build-only, still with reuse on and concurrency at
/// the bound the task book names, and each directory holding the rows the tables were folded from.
#[test]
fn three_live_runs_of_one_build_are_the_input() {
    let runs = Run::live_runs();
    assert_eq!(runs.len(), LIVE_RUNS, "§11 asks for at least three");

    let revisions: BTreeSet<&str> = runs.iter().map(Run::git_revision).collect();
    assert_eq!(
        revisions.len(),
        1,
        "§11's runs are one build run three times, and these are {revisions:?}"
    );
    let modes: BTreeSet<&str> = runs.iter().map(Run::execution_mode).collect();
    assert_eq!(
        modes,
        ["build-only"].into_iter().collect::<BTreeSet<&str>>(),
        "§28's boundary: every live run here is build-only"
    );

    let mut heights: Vec<u64> = Vec::new();
    for run in &runs {
        assert_eq!(
            run.state_read_reuse(),
            json!(true),
            "{}: §11 keeps `state_read_reuse=true`, and this run ran with {:?}",
            run.name,
            run.state_read_reuse()
        );
        assert_eq!(
            run.configured_concurrency(),
            json!(1),
            "{}: §11 keeps concurrency at 1 — no new optimisation is switched on to make the \
             duplicate look smaller or larger",
            run.name
        );
        assert_eq!(
            run.source(),
            json!("live"),
            "{}: the pooled root is assembled from the live arm only, so a replay row in `runs/` \
             would be a second measurement hiding in the first",
            run.name
        );
        assert_eq!(
            run.calls.len() as u64,
            run.asks(),
            "{}: the run published {} asks over {} call rows — one row is one logical request \
             (§25), so the two counts can only agree",
            run.name,
            run.asks(),
            run.calls.len()
        );
        assert_eq!(
            run.per_run["run_provenance"]["calls"].as_u64(),
            Some(run.calls.len() as u64),
            "{}: `run_provenance.calls` disagrees with the row list beside it",
            run.name
        );
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for row in &run.calls {
            let id = row["logical_request_id"]
                .as_str()
                .unwrap_or("?")
                .to_string();
            assert!(
                seen.insert(id.clone()),
                "{}: two rows carry the logical request id {id}, so the pair table would be \
                 counting one ask twice",
                run.name
            );
        }
        heights.push(
            run.block_number()
                .as_u64()
                .unwrap_or_else(|| panic!("{}: no block number on its own trace line", run.name)),
        );
    }
    assert_eq!(
        heights.len(),
        heights.iter().copied().collect::<BTreeSet<u64>>().len(),
        "§11 allows the runs to sit on different heads, and these did not: {heights:?}. Two runs of \
         one height would make the §16 aggregate a mix of one block counted twice"
    );

    for run in &runs {
        let files: BTreeSet<String> = walk_files(&run.dir)
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .collect();
        for name in CROSS_STAGE_FILES {
            assert!(
                files.contains(name),
                "{}: §14's `{name}` is missing",
                run.name
            );
        }
        assert!(
            files.contains(CROSS_STAGE_RUN_FILE),
            "{}: §12's per-run pair list is missing",
            run.name
        );
    }
}

/// §6 and §14's 「数字必须能由 raw evidence 重算」 as bytes: the committed root against a fresh
/// assembly of the same runs' rows.
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
             or `canonicalization.rs` changed after these files were written and they have to be \
             refreshed from the runs that measured them"
        );
    }
}

/// The same assembly twice. A pooled file stamped at the moment of writing, or a table whose rows
/// came out in map order, would pass the gate above every time nobody ran it twice.
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

/// Every figure the root pools is the runs' own, and no figure appears at the root that a run did
/// not publish: the pooled totals are the per-run totals added, the pooled rows are the runs' rows
/// end to end, and each pooled cell names the runs it came from.
#[test]
fn the_pooled_tables_are_the_runs_own_rows_and_nothing_else() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("rebuild");
    let root = read_root(&dir);
    let summary = root_table(&root, DUPLICATE_SUMMARY_FILE);
    let candidates = root_table(&root, REUSE_CANDIDATES_FILE);
    let matrix = root_table(&root, DUPLICATE_MATRIX_FILE);

    let total = |pick: fn(&Run) -> u64| runs.iter().map(pick).sum::<u64>();
    let pooled = |key: &str| summary[key].as_u64().unwrap_or(0);
    assert_eq!(
        pooled("total_asks"),
        total(Run::asks),
        "the pooled ask count is not the runs' asks added"
    );
    assert_eq!(
        pooled("duplicate_pairs"),
        total(Run::pairs),
        "the pooled pair count is not the runs' pairs added"
    );
    assert_eq!(
        candidates["pairs"].as_u64(),
        Some(total(|run| {
            run.candidates["rows"]
                .as_array()
                .map_or(0, |rows| rows.len() as u64)
        })),
        "the pooled candidate rows are the runs' rows end to end"
    );
    assert_eq!(
        candidates["rows"]
            .as_array()
            .map_or(0, |rows| rows.len() as u64),
        pooled("duplicate_pairs"),
        "one row per pair, or the row list is a second derivation of them"
    );
    assert_eq!(
        matrix["totals"]["pairs"].as_u64(),
        Some(pooled("duplicate_pairs")),
        "§14's matrix and §16's summary disagree about how many pairs there were"
    );
    assert_eq!(
        shown(&summary["run_names"]),
        runs.iter()
            .map(|run| run.name.clone())
            .collect::<Vec<String>>()
            .join(", "),
        "the pooled table names the runs it was folded from, in the order the directory lists them"
    );

    // A pooled row must be traceable to a run: `pairs_per_run` is the trace, and each entry has to
    // match that run's own published figure for the same cell.
    let mut pooled_cells = 0u64;
    for row in matrix["rows"].as_array().unwrap_or(&NO_ROWS) {
        let cell_total = row["pairs"].as_u64().unwrap_or(0);
        pooled_cells += cell_total;
        let per_run = row["pairs_per_run"].as_array().unwrap_or_else(|| {
            panic!(
                "a matrix row ({:?} → {:?}) carries no `pairs_per_run`, so its {} pairs could not \
                 be traced to the run that measured them",
                row["producer_stage"], row["consumer_stage"], cell_total
            )
        });
        let from_runs: u64 = per_run
            .iter()
            .map(|entry| entry["pairs"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(
            from_runs, cell_total,
            "a matrix cell's per-run entries do not add up to the cell"
        );
        for entry in per_run {
            let name = entry["run"].as_str().unwrap_or("?");
            let run = runs.iter().find(|run| run.name == name).unwrap_or_else(|| {
                panic!("a matrix row names {name}, which is not one of the runs")
            });
            let own = run.matrix["rows"]
                .as_array()
                .unwrap_or(&NO_ROWS)
                .iter()
                .find(|own_row| {
                    own_row["producer_stage"] == row["producer_stage"]
                        && own_row["consumer_stage"] == row["consumer_stage"]
                        && own_row["duplicate_type"] == row["duplicate_type"]
                        && own_row["scope"] == row["scope"]
                })
                .and_then(|own_row| own_row["pairs"].as_u64())
                .unwrap_or(0);
            assert_eq!(
                own,
                entry["pairs"].as_u64().unwrap_or(0),
                "{}: the pooled row's per-run figure is not that run's own cell",
                run.name
            );
        }
    }
    assert_eq!(
        pooled_cells,
        matrix["totals"]["pairs"].as_u64().unwrap_or(0),
        "the matrix's rows do not add up to its own totals"
    );
    for run in &runs {
        assert_eq!(
            run.candidates["rows"].as_array().unwrap_or(&NO_ROWS).len(),
            run.pairs() as usize,
            "{}: one directed row per pair",
            run.name
        );
    }
}

/// §13: every named pair is reported, a pair this build cannot measure is reported with the reason
/// instead of as a zero, and what the runs produced that §13 did not name is listed rather than
/// dropped. The matrix is the cross-check: a named pair's figure is the sum of that stage
/// combination's cells.
#[test]
fn every_named_stage_pair_is_reported_even_when_this_build_has_no_such_stage() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("named-pairs");
    let root = read_root(&dir);
    let pairs_table = root_table(&root, STAGE_PAIRS_FILE);
    let matrix = root_table(&root, DUPLICATE_MATRIX_FILE);

    let rows = pairs_table["rows"].as_array().expect("§13's rows");
    assert_eq!(
        rows.len(),
        NAMED_STAGE_PAIRS.len(),
        "§13 names {} pairs and this table reports {}",
        NAMED_STAGE_PAIRS.len(),
        rows.len()
    );
    let statuses = pairs_table["statuses"]
        .as_array()
        .expect("the four statuses")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<&str>>();
    for row in rows {
        let status = row["status"].as_str().unwrap_or("?");
        assert!(
            statuses.contains(&status),
            "a stage-pair row carries {status}, which is not one of the four statuses the table \
             declares: {statuses:?}"
        );
        assert!(
            !shown(&row["why"]).is_empty(),
            "{}: a pair reported without a reason reads as a measurement, and §13's \
             not_applicable is not one",
            row["pair"]
        );
    }

    for (index, named) in NAMED_STAGE_PAIRS.iter().enumerate() {
        let row = &rows[index];
        assert_eq!(
            row["pair"].as_str(),
            Some(named.label),
            "§13's pairs are published in §13's order; position {index} is {:?}",
            row["pair"]
        );
        // The pooled matrix's cells for this stage combination, optionally narrowed to one class.
        // The two directions are separate equations and not a bound: §13's row counts the pairs
        // flowing producer → consumer, and the same row publishes the reverse count next to it.
        let cells = |from: &[&str], to: &[&str], class: Option<&str>| -> u64 {
            matrix["rows"]
                .as_array()
                .unwrap_or(&NO_ROWS)
                .iter()
                .filter(|cell| {
                    cell["producer_stage"]
                        .as_str()
                        .is_some_and(|s| from.contains(&s))
                        && cell["consumer_stage"]
                            .as_str()
                            .is_some_and(|s| to.contains(&s))
                        && class.is_none_or(|name| cell["duplicate_type"].as_str() == Some(name))
                })
                .map(|cell| cell["pairs"].as_u64().unwrap_or(0))
                .sum()
        };
        if row["status"] == json!(PAIR_STAGE_ABSENT) {
            assert_eq!(
                row["pairs"].as_u64(),
                Some(0),
                "{}: a pair with no such stage is reported as unmeasurable, so it cannot also \
                 carry a count",
                named.label
            );
            continue;
        }
        let producer_stages = row["producer_stages"]
            .as_array()
            .unwrap_or(&NO_ROWS)
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<&str>>();
        let consumer_stages = row["consumer_stages"]
            .as_array()
            .unwrap_or(&NO_ROWS)
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<&str>>();
        assert_eq!(
            row["pairs"].as_u64().unwrap_or(0),
            cells(&producer_stages, &consumer_stages, None),
            "{}: the named pair's figure is not §14's matrix cells for that stage combination — \
             the two tables read the same pair records in different shapes, so one of them has \
             been derived twice",
            named.label
        );
        assert_eq!(
            row["pairs_in_the_other_direction"].as_u64().unwrap_or(0),
            cells(&consumer_stages, &producer_stages, None),
            "{}: the reverse direction is not the matrix's cells with the stages swapped",
            named.label
        );
        let by_class = row["pairs_by_class"]
            .as_object()
            .expect("a class breakdown");
        for (class, count) in by_class {
            assert_eq!(
                count.as_u64().unwrap_or(0),
                cells(&producer_stages, &consumer_stages, Some(class)),
                "{}: the {class} figure is not the matrix's {class} cells for this combination",
                named.label
            );
        }
        let summed: u64 = by_class
            .values()
            .map(|count| count.as_u64().unwrap_or(0))
            .sum();
        assert_eq!(
            summed,
            row["pairs"].as_u64().unwrap_or(0),
            "{}: the row's class breakdown does not add up to its own pair count",
            named.label
        );
    }

    // The named rows and the un-named ones together are the whole pair set — nothing measured is
    // left out of §13's list and nothing in it was invented.
    let named: u64 = rows
        .iter()
        .map(|row| row["pairs"].as_u64().unwrap_or(0))
        .sum();
    let unnamed_rows = pairs_table["pairs_observed_but_not_named"]
        .as_array()
        .expect("a list, possibly empty");
    let unnamed: u64 = unnamed_rows
        .iter()
        .map(|row| row["pairs"].as_u64().unwrap_or(0))
        .sum();
    let summary = root_table(&root, DUPLICATE_SUMMARY_FILE);
    assert_eq!(
        named + unnamed,
        summary["duplicate_pairs"].as_u64().unwrap_or(0),
        "§13's eleven pairs count {named} and the un-named combinations count {unnamed}, which is \
         not the {} the summary publishes — a pair is either in the named list or in the un-named \
         one, and a pair in neither is a pair nobody reported",
        shown(&summary["duplicate_pairs"])
    );
    for row in unnamed_rows {
        assert_ne!(
            row["pairs"].as_u64().unwrap_or(0),
            0,
            "an un-named pair with no pairs is a row the runs did not produce"
        );
    }

    // §13's 「不要造数据」 read as a fact about this corpus rather than asserted: the four statuses
    // all appear in at least one committed run, so no status here is decoration.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for run in &runs {
        for row in run.stage_pairs["rows"].as_array().unwrap_or(&NO_ROWS) {
            if let Some(status) = row["status"].as_str() {
                seen.insert(status);
            }
        }
    }
    for status in [
        PAIR_MEASURED,
        PAIR_NO_DUPLICATES,
        PAIR_NO_ASKS_OBSERVED,
        PAIR_STAGE_ABSENT,
    ] {
        assert!(
            seen.contains(status),
            "the runs never produced a `{status}` row, so a table that lists all four statuses \
             would be reporting a category nothing in this corpus fills",
        );
    }
}

/// §23: a pair whose two asks name different heights is refused — `same_block` resolved to
/// `checked_violated`, `safe_to_reuse` is false, and the reason is the task book's own words,
/// 「different block identity」. A pair whose two asks do not name comparable heights (a tag against
/// a height, a tag against a tag, neither asking one) is reported as *not met* rather than violated:
/// the rule refuses what it can measure and declines the rest, and this gate checks both halves on
/// every pooled row.
#[test]
fn a_duplicate_across_two_blocks_is_refused_and_says_which_rule_refused_it() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("block-rule");
    let root = read_root(&dir);
    let candidates = root_table(&root, REUSE_CANDIDATES_FILE);
    let summary = root_table(&root, DUPLICATE_SUMMARY_FILE);

    let outcome_of = |row: &Value, wanted: &str| -> Option<String> {
        row["conditions"]
            .as_array()?
            .iter()
            .find(|condition| condition["condition"] == json!(wanted))
            .and_then(|condition| condition["outcome"].as_str())
            .map(str::to_string)
    };

    let mut violated = 0u64;
    let mut declined = 0u64;
    let mut met = 0u64;
    let mut safe = 0u64;
    let rows = candidates["rows"].as_array().expect("§15's rows");
    assert_eq!(
        rows.len(),
        summary["duplicate_pairs"].as_u64().unwrap_or(0) as usize,
        "one directed row per pair, or the row list is a second derivation of them"
    );
    for row in rows {
        assert_eq!(
            row["conditions"].as_array().map_or(0, Vec::len),
            5,
            "{}: §6's five conditions, all of them, on every row",
            row["candidate_id"]
        );
        let outcome = outcome_of(row, "same_block").unwrap_or_else(|| {
            panic!(
                "{}: no `same_block` condition on its row",
                row["candidate_id"]
            )
        });
        let relation = row["block_relation"].as_str().unwrap_or("?");
        if row["safe_to_reuse"].as_bool() == Some(true) {
            safe += 1;
        }
        match outcome.as_str() {
            "checked_violated" => {
                violated += 1;
                assert_eq!(
                    relation, "different_block",
                    "{}: §23 refuses on two named, unequal heights, and this row's relation is \
                     {relation}",
                    row["candidate_id"]
                );
                assert_eq!(
                    row["same_block"].as_bool(),
                    Some(false),
                    "{}: the row's own `same_block` figure disagrees with its condition",
                    row["candidate_id"]
                );
                assert_eq!(
                    row["safe_to_reuse"].as_bool(),
                    Some(false),
                    "{}: a pair across two heights was published as safe to reuse",
                    row["candidate_id"]
                );
                assert_eq!(
                    row["verdict"].as_str(),
                    Some("refused_different_block_identity"),
                    "{}: a violated block condition with verdict {:?}",
                    row["candidate_id"],
                    row["verdict"]
                );
                assert!(
                    shown(&row["reason"]).contains("different block identity"),
                    "{}: §23 asks for the reason 「different block identity」 and the row says {:?}",
                    row["candidate_id"],
                    row["reason"]
                );
            }
            "checkable_not_met" => {
                declined += 1;
                assert_ne!(
                    relation, "different_block",
                    "{}: two different heights are a violation, not a decline — the row's relation \
                     is {relation} and its condition is not met",
                    row["candidate_id"]
                );
                assert_eq!(
                    row["same_block"],
                    Value::Null,
                    "{}: a declined block comparison is one this record cannot make, so the row's \
                     own figure must stay null rather than read as a measured false",
                    row["candidate_id"]
                );
            }
            "checked_met" => {
                met += 1;
                assert_eq!(
                    relation, "same_block",
                    "{}: a met block condition on a {relation} row",
                    row["candidate_id"]
                );
            }
            other => panic!(
                "{}: `same_block` resolved to {other}, which is not one of the three outcomes §6 \
                 allows a checked condition to take",
                row["candidate_id"]
            ),
        }
    }
    assert_eq!(
        violated,
        summary["refused_by_block_identity"].as_u64().unwrap_or(0),
        "the rows refused by §23 ({violated}) are not the pooled refusal figure"
    );
    assert_eq!(
        safe,
        summary["safe_reuse_candidates"].as_u64().unwrap_or(0),
        "the pooled safe-to-reuse count is not a fold over the pooled rows"
    );
    assert_eq!(
        violated + declined + met,
        rows.len() as u64,
        "every row resolves its block condition one of three ways, and a fourth would be a rule \
         nothing here declared"
    );

    // §23's rule has to have both a refusal and a decline to say anything about, or this gate is a
    // sentence about one number.
    assert!(
        violated > 0,
        "no pair in this corpus names two different heights, so §23 was never exercised"
    );
    assert!(
        declined > 0,
        "no pair in this corpus declined the block comparison, so the difference §23 draws between \
         「violated」 and 「not met」 is untested here"
    );
    for run in &runs {
        assert!(
            run.summary["same_target_different_block_pairs"]
                .as_u64()
                .unwrap_or(0)
                > 0,
            "{}: §4.3's different-block class is 0 in a live run, which would make the pooled \
             figure come from somewhere other than these runs",
            run.name
        );
    }
}

/// §16: every per-dimension cell adds back to the pairs it split, in both directions — the class
/// columns inside a cell, and the cells across a dimension.
#[test]
fn the_summaries_cells_add_back_to_the_pairs_they_split() {
    let (dir, _) = fresh_assembly("cells");
    let root = read_root(&dir);
    let summary = root_table(&root, DUPLICATE_SUMMARY_FILE);
    let pairs = summary["duplicate_pairs"].as_u64().unwrap_or(0);

    let class_columns: [&str; 5] = [
        DUPLICATE_EXACT,
        "semantic_duplicate",
        SAME_TARGET_DIFFERENT_BLOCK,
        "same_method_different_semantics",
        SAME_TARGET_BLOCK_UNDETERMINED,
    ];
    for dimension in ["by_method", "by_producer_stage", "by_consumer_stage"] {
        let cells = summary[dimension]
            .as_array()
            .unwrap_or_else(|| panic!("§16 asks for {dimension} and the summary has none"));
        let total: u64 = cells
            .iter()
            .map(|cell| cell["pairs"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(
            total, pairs,
            "{dimension}: the cells add to {total} and the summary publishes {pairs} — a \
             dimension that does not cover every pair is a silent omission (§10's \
             「rpc_count = 0 不能省略」 in its aggregate form)"
        );
        for cell in cells {
            let inside: u64 = class_columns
                .iter()
                .map(|class| cell[*class].as_u64().unwrap_or(0))
                .sum();
            assert_eq!(
                inside,
                cell["pairs"].as_u64().unwrap_or(0),
                "{dimension}/{:?}: §4's classes are mutually exclusive and together are the cell",
                cell.get("method")
                    .or(cell.get("producer_stage"))
                    .or(cell.get("consumer_stage"))
            );
            let scope: u64 = [SCOPE_CROSS_STAGE, SCOPE_INTRA_STAGE]
                .iter()
                .map(|name| cell[name].as_u64().unwrap_or(0))
                .sum();
            assert!(
                scope <= cell["pairs"].as_u64().unwrap_or(0),
                "{dimension}: a pair counted in two scopes"
            );
        }
    }

    let scope_total = summary["cross_stage_pairs"].as_u64().unwrap_or(0)
        + summary["intra_stage_pairs"].as_u64().unwrap_or(0)
        + summary["same_logical_request_pairs"].as_u64().unwrap_or(0);
    assert_eq!(
        scope_total, pairs,
        "§17's three scopes are exhaustive and mutually exclusive, so they add to the pair count"
    );
    assert!(
        summary["intra_stage_pairs"].as_u64().unwrap_or(0) > 0,
        "§17 asks for intra-stage and cross-stage as two separate figures; with no intra-stage \
         pair in the corpus the separation is untested rather than proven"
    );

    let classes: u64 = [
        DUPLICATE_EXACT,
        "semantic_duplicate",
        SAME_TARGET_DIFFERENT_BLOCK,
        "same_method_different_semantics",
        SAME_TARGET_BLOCK_UNDETERMINED,
    ]
    .iter()
    .map(|class| {
        summary[format!("{class}_pairs").as_str()]
            .as_u64()
            .unwrap_or(0)
    })
    .sum();
    assert_eq!(
        classes, pairs,
        "§4's classes sum to {classes} against {pairs} pairs"
    );
}

/// §9: the six read categories partition a run's asks, and the method-determined ones are read off
/// the rows — `eth_chainId` is a chain-identity read and never a state read, and a height read is a
/// block read. The half that is *not* method-determined (`eth_call` for state against `eth_call`
/// while preparing a transaction) is a property of the caller, and canonicalization's own unit test
/// covers it; the figures here only check the parts a method name can settle.
#[test]
fn the_read_categories_partition_the_asks_and_chain_identity_is_not_a_state_read() {
    let runs = Run::live_runs();
    for run in &runs {
        let by_category = run.per_run["asks_by_read_category"]
            .as_array()
            .expect("§9's category list");
        let total: u64 = by_category
            .iter()
            .map(|row| row["asks"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(
            total,
            run.asks(),
            "{}: §9's categories cover {total} of {} asks",
            run.name,
            run.asks()
        );
        let count = |method: &str| {
            run.calls
                .iter()
                .filter(|row| row["method"].as_str() == Some(method))
                .count() as u64
        };
        let figure = |category: &str| {
            by_category
                .iter()
                .find(|row| row["category"].as_str() == Some(category))
                .and_then(|row| row["asks"].as_u64())
                .unwrap_or(0)
        };
        assert_eq!(
            figure(READ_CATEGORY_CHAIN_IDENTITY),
            count(CHAIN_ID_METHOD),
            "{}: §9 names `eth_chainId` as the read that is not a state read, and the category \
             total does not match the rows that asked it",
            run.name
        );
        let block_reads: u64 = BLOCK_METHODS.iter().map(|method| count(method)).sum();
        assert_eq!(
            figure(READ_CATEGORY_BLOCK_READ),
            block_reads,
            "{}: a block read is a height or a header asked of the node, counted from the rows",
            run.name
        );
        let state_method_rows: u64 = STATE_METHODS.iter().map(|method| count(method)).sum();
        let state_reads = figure(READ_CATEGORY_STATE_READ);
        assert!(
            state_reads >= state_method_rows,
            "{}: the four state methods alone account for {state_method_rows} asks but the table \
             reports {state_reads} state reads — a method §4 lists as a state read is not allowed \
             to lose its category",
            run.name
        );

        // Every pair endpoint carries the category of the ask it is, and no `eth_chainId` row is
        // ever labelled a state read anywhere in the directory.
        for row in run.candidates["rows"].as_array().unwrap_or(&NO_ROWS) {
            for side in ["producer", "consumer"] {
                let endpoint = &row[side];
                if endpoint["method"].as_str() == Some(CHAIN_ID_METHOD) {
                    assert_eq!(
                        endpoint["category"].as_str(),
                        Some(READ_CATEGORY_CHAIN_IDENTITY),
                        "{}: a {} side asked `eth_chainId` and was categorised {:?}",
                        run.name,
                        side,
                        endpoint["category"]
                    );
                }
                if let Some(method) = STATE_METHODS
                    .iter()
                    .find(|method| endpoint["method"].as_str() == Some(**method))
                {
                    assert_eq!(
                        endpoint["category"].as_str(),
                        Some(READ_CATEGORY_STATE_READ),
                        "{}: a {} side asked `{method}` and was categorised {:?}",
                        run.name,
                        side,
                        endpoint["category"]
                    );
                }
            }
        }
    }
}

/// §18: a cache hit cost no request, so it has no call row and cannot be a physical duplicate —
/// while still being the kind of reuse §1's question is about. The corpus has hits to exclude, so
/// this is a measurement and not an absence.
#[test]
fn a_cache_hit_is_absent_from_the_pair_counts_by_construction() {
    let runs = Run::live_runs();
    let mut hits = 0u64;
    for run in &runs {
        hits += run.cache_hits();
        assert_eq!(
            run.asks(),
            run.calls.len() as u64,
            "{}: asks count call rows, so a lookups table with a different figure here would mean \
             a cache hit had been counted as an ask",
            run.name
        );
        let lookups = run.cache_hits()
            + run.line()["state_read_cache"]["cache_misses"]
                .as_u64()
                .unwrap_or(0);
        assert!(
            lookups > run.asks(),
            "{}: {} lookups against {} asks — with no hits in this run the §18 boundary is not \
             being exercised here, and that belongs in the report rather than in a passing gate",
            run.name,
            lookups,
            run.asks()
        );
    }
    assert!(
        hits > 0,
        "no cache hit anywhere in the live arm, so the tables could not have distinguished a hit \
         from a duplicate in the first place"
    );
}

/// §25: a retry is one ask, so it cannot be a duplicate of itself. The rule is enforced at the row
/// layer (one row per logical request, its physical tries listed beside it — checked row by row in
/// M8.4.1's own gate), and what this milestone adds is the pair layer: the two sides of every pair
/// are two *different* logical requests, each cited by the id the call rows carry, with the try
/// count it published. Nothing retried in these three runs, which the gate reports rather than
/// passes over.
#[test]
fn a_retry_is_one_ask_and_two_sides_of_a_pair_are_never_the_same_ask() {
    let runs = Run::live_runs();
    let mut retried_total = 0_u64;
    let mut tries_total = 0_u64;
    let mut rows_total = 0_u64;
    for run in &runs {
        let by_id: BTreeMap<&str, &Value> = run
            .calls
            .iter()
            .map(|row| (row["logical_request_id"].as_str().unwrap_or("?"), row))
            .collect();
        assert_eq!(
            by_id.len(),
            run.calls.len(),
            "{}: {} call rows under {} logical request ids — a second row for one id is the one \
             shape of duplicate §25 forbids outright",
            run.name,
            run.calls.len(),
            by_id.len()
        );
        for row in &run.calls {
            let tries = row["attempts"].as_array().map_or(0, Vec::len) as u64;
            tries_total += tries;
            retried_total += u64::from(tries > 1);
            rows_total += 1;
        }
        let published = run.per_run["integrity"]["rows_with_more_than_one_physical_attempt"]
            .as_u64()
            .unwrap_or(u64::MAX);
        let own = run
            .calls
            .iter()
            .filter(|row| row["attempts"].as_array().map_or(0, Vec::len) > 1)
            .count() as u64;
        assert_eq!(
            published, own,
            "{}: the run published {published} rows with more than one physical attempt and its \
             own rows say {own}",
            run.name
        );

        for pair in run.candidates["rows"].as_array().unwrap_or(&NO_ROWS) {
            let producer = pair["producer"]["logical_request_id"]
                .as_str()
                .unwrap_or("?");
            let consumer = pair["consumer"]["logical_request_id"]
                .as_str()
                .unwrap_or("?");
            assert_ne!(
                producer, consumer,
                "{}: a pair whose two sides are the same logical request `{producer}` is §25's \
                 retry counted as a cross-stage duplicate",
                run.name
            );
            for side in ["producer", "consumer"] {
                let id = pair[side]["logical_request_id"].as_str().unwrap_or("?");
                let row = by_id.get(id).copied().unwrap_or_else(|| {
                    panic!(
                        "{}: {side} cites `{id}`, which no call row of this run carries — a pair \
                         between asks that were not recorded is a pair this milestone measured \
                         out of nothing",
                        run.name
                    )
                });
                assert_eq!(
                    row["attempt"].as_u64(),
                    pair[side]["physical_attempts"].as_u64(),
                    "{}: the {side} of a pair says {} tries for `{id}` and the row itself says {}",
                    run.name,
                    shown(&pair[side]["physical_attempts"]),
                    shown(&row["attempt"])
                );
                assert_eq!(
                    row["run"].as_str(),
                    pair["run"].as_str(),
                    "{}: a pair names run {:?} while its {side} cites a row of run {:?}",
                    run.name,
                    pair["run"].as_str(),
                    row["run"].as_str()
                );
            }
        }
    }
    if retried_total == 0 {
        // The honest alternative to a gate that passes on an empty set: say which case this corpus
        // cannot observe, and where the two-try case is tested instead.
        println!(
            "the {rows_total} live call rows of {EVIDENCE_DIR}/{RUNS} hold {tries_total} physical \
             tries between them, so nothing in this corpus retried: §25 is checked here as the pair \
             layer above (two sides, two different logical requests, each citing a real row and its \
             try count), and the two-try case is tested by `retry_is_not_a_duplicate` in \
             canonicalization's unit tests"
        );
    } else {
        println!(
            "{retried_total} of {rows_total} rows retried at least once, in {tries_total} tries"
        );
    }
}

/// §26 at the live layer: this milestone's switch observes, it does not ask. The control is the
/// previous milestone's own committed runs of the same CLI on the same route — a method that \
/// appears here and not there is a call the instrumentation made.
#[test]
fn the_cross_stage_switch_asked_the_node_for_no_method_the_previous_milestone_did_not_know() {
    let previous = previous_dir();
    assert!(
        previous.join(RUNS).exists(),
        "{PREVIOUS_MILESTONE}/{RUNS} is the control for this gate and is not in the tree"
    );
    let mut old_methods: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(previous.join(RUNS))
        .unwrap_or_else(|error| panic!("{previous:?}: {error}"))
    {
        let Some(entry) = entry.ok() else { continue };
        let path = entry.path().join(PIPELINE_CALLS_FILE);
        if !path.exists() {
            continue;
        }
        for row in read_json(&path)["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            old_methods.insert(row["method"].as_str().unwrap_or("?").to_string());
        }
    }
    assert!(
        old_methods.len() > 5,
        "the control read {} methods, which is too few to be the previous milestone's real list",
        old_methods.len()
    );

    for run in Run::live_runs() {
        let methods: BTreeSet<String> = run
            .calls
            .iter()
            .map(|row| row["method"].as_str().unwrap_or("?").to_string())
            .collect();
        for method in &methods {
            assert!(
                old_methods.contains(method),
                "{}: `{method}` is a method no M8.4.1 run asked for. §26's rule is \
                 instrumentation → observe, and a new method name is instrumentation → new RPC",
                run.name
            );
        }
        // Not a call at all: the two files that would hold a submission, and the method a
        // submission would use.
        assert!(
            !methods.contains("eth_sendRawTransaction"),
            "{}: build-only evidence holds a submission method",
            run.name
        );
    }
}

/// §3 and §31: this milestone regrouped rows M8.4.1 already published, and published no field of
/// its own into one of that milestone's tables. The discriminator is structural — the same file
/// names, the same set of field paths, and five names that are this milestone's alone.
#[test]
fn the_cross_stage_switch_left_m841s_tables_the_same_shape() {
    let previous = previous_dir();
    let live_runs = Run::live_runs();
    let mut previous_runs = 0;
    for entry in std::fs::read_dir(previous.join(RUNS))
        .unwrap_or_else(|error| panic!("{previous:?}: {error}"))
    {
        let Ok(entry) = entry else { continue };
        let dir = entry.path();
        if !dir.join(TRACES_FILE).exists() {
            continue;
        }
        previous_runs += 1;
        for name in DEPENDENCY_FILES {
            let here = dir.join(name);
            if !here.exists() {
                continue;
            }
            let old_shape = key_paths(&read_json(&here));
            for run in &live_runs {
                let new_shape = key_paths(&read_json(&run.dir.join(name)));
                assert_eq!(
                    old_shape, new_shape,
                    "{name}: M8.4.2's runs publish a different field set than M8.4.1's, so \
                     either this milestone wrote into that milestone's table or a field appeared \
                     or vanished there"
                );
            }
        }
    }
    assert!(
        previous_runs >= 3,
        "the control walked {previous_runs} runs in {PREVIOUS_MILESTONE}/{RUNS}"
    );

    // The same rows file, since both milestones publish it, and the file-name set beside it.
    for run in &live_runs {
        let files: BTreeSet<String> = walk_files(&run.dir)
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .collect();
        let mut this_milestones: Vec<&String> = files
            .iter()
            .filter(|name| {
                CROSS_STAGE_FILES.contains(&name.as_str()) || name.as_str() == CROSS_STAGE_RUN_FILE
            })
            .collect();
        this_milestones.sort();
        assert_eq!(
            this_milestones.len(),
            CROSS_STAGE_FILES.len() + 1,
            "{}: §14's four tables and §12's per-run list are five names, and this directory \
             holds {:?}",
            run.name,
            this_milestones
        );
        assert!(
            files.contains(DUPLICATES_FILE) && files.contains(OUTSIDE_FILE),
            "{}: M8.4.1's directory shape went missing (`{DUPLICATES_FILE}` and \
             `{OUTSIDE_FILE}` are both theirs)",
            run.name
        );
    }
}

/// Every field path a JSON value holds, arrays collapsed to `path[]` so a run that made more calls
/// than another still has the same shape. Values are not compared here — only which fields exist.
fn key_paths(value: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    collect_paths(value, String::new(), &mut found);
    found
}

fn collect_paths(value: &Value, path: String, found: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let prefix = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                collect_paths(child, prefix, found);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_paths(child, format!("{path}[]"), found);
            }
        }
        other => {
            if path.is_empty() {
                found.insert("<root>".to_string());
            } else {
                let _ = other;
                found.insert(path);
            }
        }
    }
}

/// §12's reproducibility read out of the committed tree rather than re-measured: the three
/// fixed-block arms' five tables are the same bytes once the four fields that cannot cross a run
/// boundary are stripped. The simulation crate's experiment regenerates these directories and
/// compares them field by field; `cross_stage_recompute.rs` folds a live corpus in the same
/// process; this gate is the third reading — a reader of the committed tree sees the same answer
/// three times, and the strip list is exhaustive because the unstripped tables differ.
#[test]
fn the_committed_fixed_block_arms_answer_the_same_way() {
    let arms = Run::fixture_runs();
    let names = [
        DUPLICATE_MATRIX_FILE,
        DUPLICATE_SUMMARY_FILE,
        REUSE_CANDIDATES_FILE,
        STAGE_PAIRS_FILE,
        CROSS_STAGE_RUN_FILE,
    ];
    // The four keys that legitimately differ, read off the arms rather than assumed: a stamp, the
    // run list, and the per-run provenance block — the same fields §12's own gate names, and
    // nothing else.
    let mut differing: BTreeSet<String> = BTreeSet::new();
    for name in names {
        let first = arms[0].dir.join(name);
        for arm in arms.iter().skip(1) {
            let left = read_json(&first);
            let right = read_json(&arm.dir.join(name));
            if let (Some(l), Some(r)) = (left.as_object(), right.as_object()) {
                for (key, value) in l {
                    if r.get(key) != Some(value) {
                        differing.insert(format!("{name}:{key}"));
                    }
                }
            }
        }
    }
    let bare: BTreeSet<String> = differing
        .iter()
        .filter_map(|found| found.split(':').next().map(str::to_string))
        .collect();
    assert_eq!(
        bare,
        names
            .iter()
            .copied()
            .map(str::to_string)
            .collect::<BTreeSet<String>>(),
        "every one of the five tables should differ from arm to arm by nothing but the stripped \
         fields — {differing:?}"
    );

    for name in names {
        let stripped: Vec<String> = arms
            .iter()
            .map(|arm| shown(&stripped(&read_json(&arm.dir.join(name)), &arm.name)))
            .collect();
        assert!(
            stripped.windows(2).all(|pair| pair[0] == pair[1]),
            "{name}: the three fixed-block arms do not publish the same table once the \
             cross-run fields are stripped"
        );
    }

    // The strip is not hiding an empty comparison: each arm asks for something.
    let asks: Vec<u64> = arms.iter().map(Run::asks).collect();
    assert!(
        asks.iter().all(|count| *count > 0) && asks.windows(2).all(|pair| pair[0] == pair[1]),
        "the fixture arms issued {asks:?} asks — the same number on every arm, or the tables are \
         being compared across runs that did not do the same work"
    );
    let live_runs = Run::live_runs();
    let live_endpoint = live_runs[0].endpoint_id();
    for arm in &arms {
        assert_ne!(
            arm.endpoint_id(),
            live_endpoint,
            "{}: a fixed-block arm carries the live endpoint's digest, so it is not the replay \
             §12 asks for",
            arm.name
        );
        assert_eq!(
            arm.source(),
            json!("fixture"),
            "{}: §12's replay arm is a fixture run",
            arm.name
        );
    }
    let heights: Vec<Value> = arms.iter().map(Run::block_number).collect();
    assert!(
        heights.windows(2).all(|pair| pair[0] == pair[1]),
        "§12 asks for the same block in every replay arm, and these are {heights:?}"
    );
}

/// The four keys that cannot cross a run boundary, plus the run name wherever a value quotes it.
/// `started_ns` is nanoseconds since its own run's monotonic origin and `endpoint_id` is a fixture
/// port, so neither is a figure about the route.
fn stripped(value: &Value, run: &str) -> Value {
    let mut value = value.clone();
    strip(&mut value, run);
    value
}

fn strip(value: &mut Value, run: &str) {
    match value {
        Value::Object(map) => {
            for key in [
                "generated_at_unix_ms",
                "started_ns",
                "finished_ns",
                "duration_ns",
                "per_run",
                "run_names",
                "run_provenance",
            ] {
                map.remove(key);
            }
            for child in map.values_mut() {
                strip(child, run);
            }
        }
        Value::Array(values) => {
            for child in values {
                strip(child, run);
            }
        }
        Value::String(text) if text.contains(run) => {
            *value = Value::String(text.replace(run, "arm"));
        }
        _ => {}
    }
}

/// §28, read out of this directory: build-only on every record, the two submission files present
/// and empty, and no secret or host name outside the two ladder files that have always quoted the
/// URL.
#[test]
fn nothing_was_signed_broadcast_or_written_into_these_files() {
    let runs = Run::live_runs();
    let mut signed = 0;
    let mut submitted = 0;
    for run in &runs {
        let dir = evidence_dir().join(ROUTE_RUNS).join(&run.name);
        signed += jsonl_lines(&dir.join(SIGNED_FILE));
        submitted += jsonl_lines(&dir.join(SUBMISSIONS_FILE));
        let route_run = read_json(&dir.join(ROUTE_RUN_FILE));
        assert_eq!(
            route_run["mode"].as_str(),
            Some("build-only"),
            "{}: the ladder's own record says {:?}",
            run.name,
            route_run["mode"]
        );
        assert_eq!(
            route_run["successful_real_arbitrage"].as_bool(),
            Some(false),
            "{}: §1's list ends at 真实套利, and this run says it did one",
            run.name
        );
    }
    assert_eq!(
        (signed, submitted),
        (0, 0),
        "§28's boundary: the signature and submission files exist and are empty, which is the \
         shape that distinguishes 「did not happen」 from 「was not recorded」"
    );

    let files = walk_files(&evidence_dir());
    assert!(
        files.len() > 100,
        "the scan walked {} files, fewer than this directory holds — a listing rule that silently \
         misses a file turns a secret scan into a sentence",
        files.len()
    );
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
        runs.len() * 2,
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
             value"
        );
        if shown.ends_with(".json") || shown.ends_with(".jsonl") {
            if let Some((key, number)) = floats_in(&text).into_iter().next() {
                panic!(
                    "{shown}: {key} is a float ({number}) — §14's figures are integer counts and \
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
            .flat_map(|(index, child)| floats_in_value(child, format!("{path}[{index}]")))
            .collect(),
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, child)| {
                let prefix = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                floats_in_value(child, prefix)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// §30's limit at the evidence layer: the generated README reports duplicates and candidates and
/// stops. A prose file is where a milestone drifts into a recommendation, so the check is on the
/// bytes rather than on the intent.
#[test]
fn the_readme_reports_duplicates_and_candidates_without_recommending_a_cache() {
    let (dir, _) = fresh_assembly("readme-words");
    let root = read_root(&dir);
    let text = std::fs::read_to_string(dir.join(README_FILE))
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for name in GENERATED_FILES {
        assert!(
            text.contains(name),
            "the README never names `{name}`, so a reader holding the directory cannot find it"
        );
    }
    let summary = root_table(&root, DUPLICATE_SUMMARY_FILE);
    // The headline figures are checked as sentences, not as substrings: `0` appears in every
    // Markdown table in the file, so a bare `contains` would pass on any of them.
    for sentence in [
        format!(
            "{} are the same ask as an earlier one",
            shown(&summary["duplicate_pairs"])
        ),
        format!(
            "**{}** are safe to reuse",
            shown(&summary["safe_reuse_candidates"])
        ),
        format!(
            "{} pairs are refused at §23's block rule",
            shown(&summary["refused_by_block_identity"])
        ),
    ] {
        assert!(
            text.contains(&sentence),
            "the README does not say 「{sentence}」, though {} prints {sentence:?}",
            DUPLICATE_SUMMARY_FILE,
        );
    }
    for forbidden in [
        "implement a cache",
        "should cache",
        "add a cache",
        "build a cache layer",
        "cache is worth it",
        "因此应该实现 cache",
    ] {
        assert!(
            !text.contains(forbidden),
            "the README says 「{forbidden}」 — §30 allows the duplicates and the candidates to be \
             stated and nothing beyond them, and §35 forbids an experiment designed to reach that \
             conclusion"
        );
    }
    // The stronger form of the same limit, and the one that catches a sentence the list above \
    // never names: every prose line that mentions a cache has to say what it is not.
    for line in text.lines() {
        if line.starts_with('|') || !line.to_lowercase().contains("cache") {
            continue;
        }
        let lower = line.to_lowercase();
        assert!(
            ["not ", " no ", "never", "nothing", "without", "≠"]
                .iter()
                .any(|negation| lower.contains(negation)),
            "the README line 「{line}」 raises a cache and neither denies nor bounds it — §30 stops \
             at the duplicates and the candidates"
        );
    }
    for row in NAMED_STAGE_PAIRS.iter() {
        assert!(
            text.contains(row.label),
            "the README drops §13's `{}`, which is the pair a reader of this directory came to \
             check",
            row.label
        );
    }
}

/// The root's four tables name the runs they came from, in the file itself rather than only in the
/// directory listing, so one table read alone is placeable.
#[test]
fn each_root_table_carries_the_runs_it_was_folded_from() {
    let runs = Run::live_runs();
    let (dir, _) = fresh_assembly("provenance");
    let root = read_root(&dir);
    for name in CROSS_STAGE_FILES {
        let table = root_table(&root, name);
        let entries = table["assembled_from"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} carries no `assembled_from`"));
        assert_eq!(
            entries.len(),
            runs.len(),
            "{name}: one provenance row per run it pools"
        );
        for entry in entries {
            let run = runs
                .iter()
                .find(|run| run.name == entry["run"].as_str().unwrap_or("?"))
                .unwrap_or_else(|| panic!("{name} names {:?}", entry["run"]));
            assert_eq!(
                entry["block_number"],
                run.block_number(),
                "{name}: the provenance row for {} does not carry that run's own height",
                run.name
            );
            assert_eq!(
                entry["endpoint_id"],
                run.endpoint_id(),
                "{name}: the provenance row for {} does not carry that run's own endpoint digest",
                run.name
            );
            assert_eq!(
                entry["git_revision"].as_str(),
                Some(run.git_revision()),
                "{name}: a provenance row that does not name the build",
            );
        }
    }
    let by_run: BTreeMap<&str, u64> = runs
        .iter()
        .map(|run| (run.name.as_str(), run.asks()))
        .collect();
    assert!(
        by_run.values().all(|count| *count > 0),
        "a run with no asks would make the pooled figures a partial read: {by_run:?}"
    );
}
