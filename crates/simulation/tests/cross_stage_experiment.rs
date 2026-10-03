//! M8.4.2 §12's Experiment B, §26's no-extra-RPC control and §27's correctness pair — the
//! fixed-block half of this milestone's evidence, on the recorded state rather than on a node.
//!
//! ```text
//! the block     37 191 169, the M4 pin, served by `support::stub` out of
//!               `fixtures/simulation-m4/dump-37191169.json` — the answers a real node gave at
//!               that height, so this is the same state M8.3.1–M8.4.1 replayed
//! the route     the one the detector reports for that block, asked for 1 wei out
//! the settings  §11's three carried over unchanged: state_read_reuse = true, concurrency = 1,
//!               build-only. No new optimization is switched on here, including the one this
//!               milestone exists to find out about.
//! ```
//!
//! Three arms of that one run differ in exactly one thing — which tables the writer publishes:
//!
//! ```text
//! silent          no sink: nothing recorded, nothing written
//! baseline        a sink, and M8.4.1's six dependency tables
//! instrumented    the same sink, M8.4.1's six tables and this milestone's five
//! ```
//!
//! That is §27's 「fixed-block baseline / fixed-block instrumented」, and the silent arm is
//! beside it because a comparison of two instrumented arms could miss a difference that the
//! sink itself makes. What each pair of arms owes:
//!
//! * **§26** — the calls that reach the endpoint. [`test_no_extra_rpc`] compares the stub's own
//!   arrival list against each arm's recorded list and against the other arms', position by
//!   position, because a count can stay the same while a method swaps.
//! * **§27** — the answer. [`the_cross_stage_instrumentation_changed_no_answer_field_by_field`]
//!   compares §27's twelve fields, then the whole serialized result.
//!   [`a_different_answer_would_be_flagged_field_by_field`] is §29 group 4's requirement that a
//!   comparison which cannot fail is not a comparison: it breaks one field at a time and asks
//!   that the row notice each time.
//! * **§12** — the relations. Three instrumented arms of one block write three directories, and
//!   [`three_fixed_block_arms_publish_the_same_cross_stage_tables`] requires the five tables to
//!   come out the same each time.
//! * **§3** — that this milestone *adds* tables and rewrites none. The 14 data files M8.4.1's
//!   arms write are compared between the baseline and instrumented arms on their field paths and
//!   on every count in them ([`adding_the_cross_stage_tables_left_m841s_shape_and_counts_alone`]),
//!   and no duration is collected, because two arms are two clocks.
//!
//! ## The number this corpus cannot produce, and why that is reported rather than fixed
//!
//! Every ask in a fixture arm belongs to one stage — `simulation` — because this is the
//! simulation engine reached through one adapter, with no detection span and no preflight span.
//! A cross-stage pair needs two stages, so §12's arms reproduce a *zero*: 0 pairs, 0 reuse
//! candidates, and eleven `stage-pairs.json` rows that each say why they are empty. That is
//! still §12's claim — the relations found at a block come out the same when the block is
//! replayed — but it is the narrow form of it, and the wide form lives in the live runs:
//! `crates/pipeline/tests/cross_stage_recompute.rs` folds M8.4.1's three live runs and finds 69
//! pairs in the same process that these tests read 0, which is what keeps the zero here a
//! finding rather than a broken fold.
//!
//! Nothing here manufactures the second stage. A lifecycle sink could be attached to a call
//! this harness does not make, and the pair table would then be non-empty and would describe a
//! pipeline that does not exist — §35's 「不要为了证明应该做 cache 而设计实验」, and the reason
//! the live half of this milestone's evidence is the half that answers the question.
//!
//! ## Where the files go
//!
//! Always written, so the assertions below run against the bytes a reader gets and not against
//! the in-memory objects that produced them. With `M842_FIXTURE_EVIDENCE=<dir>` set the
//! fixed-block and correctness arms land under that directory, which is how
//! `data/evidence/m8/cross-stage/fixed-block/` is regenerated; the three control arms and every
//! plain `cargo test` go under `target/simulation-tests/`, where nothing can overwrite committed
//! evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use alloy_primitives::U256;
use serde_json::{json, Value};

use evm_chain::{
    BlockContext, ChainAdapter, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource,
};
use evm_core::BlockNumber;
use evm_execution::ExecutionMode;
use evm_pipeline::canonicalization::{
    CROSS_STAGE_RUN_FILE, DUPLICATE_MATRIX_FILE, DUPLICATE_SUMMARY_FILE, PAIR_MEASURED,
    PAIR_NO_ASKS_OBSERVED, PAIR_NO_DUPLICATES, PAIR_STAGE_ABSENT, REUSE_CANDIDATES_FILE,
    STAGE_PAIRS_FILE,
};
use evm_pipeline::diagnosis::{
    DiagnosisEvidence, SimulationDiagnosis, SimulationWindow, ACCOUNT_MATRIX_FILE, BOTTLENECK_FILE,
    CROSS_STAGE_FILES, DEPENDENCY_FILES, DEPENDENCY_MAP_FILE, DUPLICATES_FILE, OUTSIDE_FILE,
    PIPELINE_CALLS_FILE, README_FILE, RPC_GAPS_FILE, RPC_SUMMARY_FILE, SIMULATION_SUMMARY_FILE,
    STORAGE_BREAKDOWN_FILE, STORAGE_READS_FILE, TRACES_FILE,
};
use evm_pipeline::latency::git_revision;
use evm_simulation::{
    engine::run, BlockPin, PricedRoute, RpcStateProvider, SimulationResult, StateProvider,
};

mod support;
use support::stub::{ServedState, Stub};
use support::{request, workspace_root, BLOCK, CHAIN};

/// §12's repeat count. One arm would show a table; three show the same table three times,
/// which is the difference between a result and a reproducible one.
const B_RUNS: [&str; 3] = ["run-01", "run-02", "run-03"];

/// §27's twelve field names, in §27's order.
///
/// Four of them are split rather than merged, because the build keeps those facts apart:
/// `gas` is an amount and its price, `block` is a height and its hash, `logs` is a count and
/// the log objects, `profit` is net and gross. `return_data` has no field to read at all —
/// this build decodes a step's output into `measurements` and keeps its logs as `logs`, so the
/// row says so, and [`Arm::whole_result`] is the claim that does not depend on this file
/// having thought of a field.
const COMPARISON_FIELDS: [&str; 12] = [
    "outcome",
    "reverted",
    "gas",
    "logs",
    "return_data",
    "profit",
    "state_changes",
    "plan",
    "route",
    "block",
    "chain_id",
    "fingerprint",
];

/// Which tables the writer publishes for one arm. `silent` is spelled by the absence of a
/// directory rather than by a variant: an arm with no sink has nothing to publish, and a
/// variant for it would suggest the switch is what decides.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tables {
    /// M8.4.1's dependency tables — what `--diagnose-storage-dependency` alone builds.
    Dependency,
    /// Those six plus §14's five, which is `--diagnose-cross-stage`.
    DependencyAndCrossStage,
}

/// Where this run publishes. `M842_FIXTURE_EVIDENCE` is the evidence tree; its absence is a
/// scratch directory under `target/`, where a plain `cargo test` cannot touch committed
/// evidence.
///
/// This function creates and nothing more: the experiments run in one process, in parallel, and
/// each clears only the subtree it owns. A root-level wipe here would let whichever test started
/// second delete the first one's output.
fn evidence_root() -> PathBuf {
    match std::env::var("M842_FIXTURE_EVIDENCE") {
        Ok(dir) if !dir.is_empty() => {
            let path = PathBuf::from(dir);
            if path.is_relative() {
                workspace_root().join(path)
            } else {
                path
            }
        }
        _ => scratch_root(),
    }
}

/// `target/simulation-tests/m8.4.2-cross-stage`, always, switch or no switch.
///
/// §31 says follow the repository's existing evidence convention rather than the directory list
/// in the task document, and M8.4.1's committed tree commits exactly two subtrees — the
/// fixed-block arms and the correctness comparison. Three of this file's arms exist to prove the
/// gate would notice a change (`no-extra-rpc`, `negative-control`, `isolation`); they are
/// re-derivable from the same build in seconds and committing eleven more directories of them
/// would double the tree without adding a number the report reads, so they stay out of it.
fn scratch_root() -> PathBuf {
    let dir = workspace_root().join("target/simulation-tests/m8.4.2-cross-stage");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    dir
}

/// An empty directory of this experiment's own, so a stale file cannot be read as this run's.
fn fresh_subtree(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    dir
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The same, for either of the two shapes these directories hold: a whole-table JSON file, or
/// one line per record. A `.jsonl` is read as an array so the shape and count walk below sees
/// the same structure it sees in a `rows` list.
fn read_table(path: &Path) -> Value {
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".jsonl"))
    {
        let lines: Vec<Value> = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            })
            .collect();
        return Value::Array(lines);
    }
    read_json(path)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The files [`DiagnosisEvidence::finish`] owes a directory, spelled from the writer's own
/// constants: an arm's file set is then a claim about the switches that were on, and a fifth
/// table appearing somewhere else in the tree would be caught as an extra name rather than
/// read as expected.
fn files_the_writer_owes(tables: Option<Tables>, lifecycle_recorded: bool) -> Vec<String> {
    let Some(tables) = tables else {
        return Vec::new();
    };
    let mut names = vec![
        README_FILE.to_string(),
        RPC_SUMMARY_FILE.to_string(),
        SIMULATION_SUMMARY_FILE.to_string(),
        TRACES_FILE.to_string(),
        DUPLICATES_FILE.to_string(),
    ];
    // The four acquisition tables beside M8.4.1's six dependency tables (one of which,
    // `storage-reads.json`, is named by `DEPENDENCY_FILES` itself), so this file's directories
    // are the ones M8.4.1's gates already read plus §14's five.
    names.extend(
        [
            ACCOUNT_MATRIX_FILE,
            BOTTLENECK_FILE,
            RPC_GAPS_FILE,
            STORAGE_BREAKDOWN_FILE,
        ]
        .into_iter()
        .map(str::to_string),
    );
    names.extend(DEPENDENCY_FILES.into_iter().map(str::to_string));
    if lifecycle_recorded {
        names.push(OUTSIDE_FILE.to_string());
    }
    if tables == Tables::DependencyAndCrossStage {
        names.extend(CROSS_STAGE_FILES.into_iter().map(str::to_string));
        names.push(CROSS_STAGE_RUN_FILE.to_string());
    }
    names.sort();
    names.dedup();
    names
}

/// One arm: the run, what the endpoint and the sink each said about it, and — for an arm that
/// wrote one — the directory it left behind.
struct Arm {
    name: String,
    /// Method names in the order the endpoint accepted them, `eth_chainId` first. This is the
    /// server's own list: what it received, not what the instrumentation chose to record.
    arrivals: Vec<String>,
    /// What the sink recorded. Empty for the silent arm, which is the point.
    events: Vec<RpcCallEvent>,
    result: SimulationResult,
    dir: Option<PathBuf>,
    files: Vec<String>,
}

impl Arm {
    /// A call's identity as this file compares calls: what was asked, of which account, at
    /// which word. Method names alone would let an arm that read a different contract pass.
    fn call_identity(&self) -> Vec<(String, String, String)> {
        self.arrivals
            .iter()
            .cloned()
            .zip(self.events.iter().map(|event| {
                (
                    event.target.clone().unwrap_or_default(),
                    event.slot.clone().unwrap_or_default(),
                )
            }))
            .map(|(method, (target, slot))| (method, target, slot))
            .collect()
    }

    /// §27's twelve fields, one row per name, read off the run rather than restated.
    ///
    /// `route` is the route *as executed* — the targets and calldata the EVM ran — because the
    /// requested route is this harness's input, and comparing it between two arms would be
    /// comparing one object with itself.
    fn identity(&self) -> Value {
        json!({
            "outcome": format!("{:?}", self.result.outcome),
            "reverted": self.result.revert().is_some(),
            "gas": {
                "gas_used": self.result.gas_used(),
                "charge": format!("{:?}", self.result.gas_charge),
            },
            "logs": {
                "count": self.result.logs().len(),
                "steps": self
                    .result
                    .steps
                    .iter()
                    .map(|step| step.logs.iter().map(|log| format!("{log:?}")).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            },
            "return_data": "not recorded by this build: a step keeps the decoded value it \
                            matched as `measurements` and its logs as `logs`; the bytes behind \
                            them are compared by `whole_result`",
            "profit": {
                "net": format!("{:?}", self.result.net_profit),
                "gross_profit": self.result.gross_profit.map(|amount| amount.to_string()),
                "gross_loss": self.result.gross_loss.map(|amount| amount.to_string()),
            },
            "state_changes": format!("{:?}", self.result.state_changes),
            "plan": format!("{:?}", self.result.plan_summary),
            "route": json!({
                "executed_steps": self
                    .result
                    .steps
                    .iter()
                    .map(|step| json!({
                        "index": step.index,
                        "to": format!("{}", step.to),
                        "selector": step.selector.clone(),
                        "value": step.value.to_string(),
                        "calldata": hex::encode(&step.calldata),
                        "status": format!("{:?}", step.status),
                    }))
                    .collect::<Vec<_>>(),
            }),
            "block": {
                "number": self.result.block.number.0,
                "hash": format!("{:?}", self.result.block.hash),
            },
            "chain_id": self.result.chain_id.0,
            "fingerprint": self.result.fingerprint(),
        })
    }

    /// Every field of the result, serialized by the same code `fingerprint()` hashes — the
    /// claim that does not depend on this file having thought of a field.
    fn whole_result(&self) -> Value {
        serde_json::to_value(&self.result).expect("the result of a finished run must serialize")
    }
}

/// Run the pinned route once, through the recorded-state stub.
///
/// `traced` and `tables` are this file's only two variables. Everything else — route, pin, ask,
/// reuse bound, concurrency bound, gas ceiling — is the same call whichever way they fall,
/// which is what makes the arms comparable.
async fn run_arm(
    state: &Arc<ServedState>,
    route: &PricedRoute,
    header: &BlockContext,
    name: &str,
    traced: bool,
    tables: Option<Tables>,
    out_dir: Option<&Path>,
) -> Arm {
    let origin = Instant::now();
    let sink = traced.then(|| {
        RpcTraceSink::new(
            origin,
            format!("m8.4.2-{name}-{BLOCK}"),
            RpcTraceSource::Fixture,
            Some(CHAIN.0),
        )
    });
    let stub = Stub::spawn(Arc::clone(state));
    // `connect_with_trace` so the connect's own `eth_chainId` is on the same timeline as the
    // reads, which is what lets §26's list compare cover every call the run made. With `traced`
    // off this is the same call `connect` makes.
    let base = HttpChainAdapter::connect_with_trace(stub.url(), sink.clone())
        .await
        .expect("the stub answers eth_chainId, which is all a connect does");
    assert_eq!(
        base.chain_id(),
        CHAIN,
        "the stub is the chain the route is on"
    );
    let adapter: Arc<dyn ChainAdapter> = Arc::new(base);

    let pin = BlockPin::new(BlockNumber(BLOCK), state.block_hash);
    // §11's settings, spelled out rather than inherited from a default, so an arm that drifted
    // is caught here and not in a table three files away.
    let provider = Arc::new(RpcStateProvider::with_state_read_concurrency(
        adapter, pin, true, 1,
    ));
    assert_eq!(
        provider.state_read_concurrency().configured,
        1,
        "the {name} arm was asked for serial state reads and the provider reports otherwise"
    );
    let shared: Arc<dyn StateProvider> = provider.clone();
    let result = run(
        shared,
        &request(route.clone(), header.clone(), U256::ONE, provider.source()),
    )
    .await
    .expect("the recorded state replays this route");
    let finished_ns = origin.elapsed().as_nanos() as u64;

    let events = sink.as_ref().map(|sink| sink.events()).unwrap_or_default();
    let arrivals = stub.methods();
    let stats = provider.state_read_stats();
    let concurrency = provider.state_read_concurrency();

    let mut files = Vec::new();
    if let Some(dir) = out_dir {
        let evidence = DiagnosisEvidence::open(
            dir,
            git_revision(),
            ExecutionMode::BuildOnly.name(),
            // M8.3.2's four tables come along because M8.4.1's directories have them, and this
            // file's arms are meant to be readable next to those.
            true,
        )
        .expect("a diagnosis directory this file just created can be opened")
        .with_dependency_tables();
        let evidence = match tables {
            Some(Tables::DependencyAndCrossStage) => evidence.with_cross_stage_tables(),
            _ => evidence,
        };
        let window = SimulationWindow {
            simulation_id: format!("m8.4.2-{name}-{BLOCK}"),
            source: RpcTraceSource::Fixture,
            chain_id: Some(CHAIN.0),
            block_number: Some(BLOCK),
            state_source: Some(provider.source()),
            started_ns: 0,
            finished_ns,
        };
        let diagnosis = SimulationDiagnosis::new(window, events.clone())
            .with_state_reads(Some(stats))
            .with_concurrency(Some(concurrency))
            .with_endpoint(
                sink.as_ref()
                    .and_then(|sink| sink.endpoint_id())
                    .map(str::to_string),
            );
        // `refusals` empty: a run whose adapter could not be traced records why, and this one
        // traced everything it was asked to.
        let mut evidence = evidence;
        evidence
            .record(diagnosis, &[])
            .expect("the recorded simulation could not be written");
        evidence
            .finish()
            .expect("the diagnosis files could not be written");
        files = listing(dir);
    }

    Arm {
        name: name.to_string(),
        arrivals,
        events,
        result,
        dir: out_dir.map(Path::to_path_buf),
        files,
    }
}

/// §26's rule, in the name the task book asks for: the instrumentation observes, and never
/// asks. Three arms of one route at one block, one of them silent, and the endpoint's own
/// arrival list identical across all three — position by position, method and target and slot
/// together.
///
/// The positive control is in the same test: the instrumented arm wrote files the baseline arm
/// did not, so three matching call lists cannot be three arms that measured nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_no_extra_rpc() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let root = scratch_root();
    let (silent, baseline, instrumented) =
        run_three_arms(&fixture, &state, &root, "no-extra-rpc").await;

    for (label, arm) in [
        ("silent", &silent),
        ("baseline", &baseline),
        ("instrumented", &instrumented),
    ] {
        assert_eq!(
            arm.arrivals.len(),
            41,
            "{label}: M8.4.1's fixture arms put 41 calls on the wire, and this one put {}",
            arm.arrivals.len()
        );
    }
    assert_eq!(
        silent.arrivals, baseline.arrivals,
        "a sink changed which calls go out, or the order they go out in"
    );
    assert_eq!(
        baseline.arrivals, instrumented.arrivals,
        "§26: the cross-stage tables changed which calls go out, or their order"
    );
    assert_eq!(
        baseline.call_identity(),
        instrumented.call_identity(),
        "§26: the arms asked the same methods in the same order but of different accounts or \
         different words, which a method list alone would have hidden"
    );
    // Every recorded call is a call the endpoint received, and vice versa: the record adds no
    // entry to the wire, and the wire has no call the record missed.
    assert_eq!(
        instrumented.events.len(),
        instrumented.arrivals.len(),
        "§26: the sink recorded a different number of calls than the endpoint accepted"
    );
    assert_eq!(
        instrumented
            .events
            .iter()
            .map(|event| event.method.clone())
            .collect::<Vec<_>>(),
        instrumented.arrivals,
        "§26: the recorded methods are not the accepted ones, in order"
    );

    // The control that keeps the three matching lists from meaning nothing.
    assert!(
        silent.files.is_empty() && silent.dir.is_none(),
        "the silent arm wrote a directory, so it is not the un-instrumented control"
    );
    assert_eq!(
        baseline.files,
        files_the_writer_owes(Some(Tables::Dependency), false),
        "{}: the baseline arm's directory is not M8.4.1's file set",
        baseline.name
    );
    assert_eq!(
        instrumented.files,
        files_the_writer_owes(Some(Tables::DependencyAndCrossStage), false),
        "{}: the instrumented arm's directory is not the baseline set plus §14's five",
        instrumented.name
    );
    let added: Vec<&String> = instrumented
        .files
        .iter()
        .filter(|name| !baseline.files.contains(name))
        .collect();
    assert_eq!(
        added.len(),
        5,
        "the two directories differ by {added:?}, which is not this milestone's five tables"
    );
    // And the tables it added are not empty shells: they carry this arm's own 41 asks.
    let per_run = read_json(
        &instrumented
            .dir
            .as_ref()
            .expect("an instrumented arm has a directory")
            .join(CROSS_STAGE_RUN_FILE),
    );
    assert_eq!(
        per_run["asks"].as_u64(),
        Some(41),
        "the per-run table does not describe the 41 asks this arm made: {per_run}"
    );
}

/// The three arms, in one place, because §26, §27 and §12 all read the same two tables and a
/// fourth arm would be a fourth thing to keep identical.
async fn run_three_arms(
    fixture: &support::Fixture,
    state: &Arc<ServedState>,
    root: &Path,
    subtree: &str,
) -> (Arm, Arm, Arm) {
    let arms_root = fresh_subtree(root, subtree);
    let silent = run_arm(
        state,
        &fixture.route,
        &fixture.header,
        "silent",
        false,
        None,
        None,
    )
    .await;
    let baseline_dir = arms_root.join("baseline");
    let baseline = run_arm(
        state,
        &fixture.route,
        &fixture.header,
        "baseline",
        true,
        Some(Tables::Dependency),
        Some(&baseline_dir),
    )
    .await;
    let instrumented_dir = arms_root.join("instrumented");
    let instrumented = run_arm(
        state,
        &fixture.route,
        &fixture.header,
        "instrumented",
        true,
        Some(Tables::DependencyAndCrossStage),
        Some(&instrumented_dir),
    )
    .await;
    (silent, baseline, instrumented)
}

/// §27's field-by-field comparison: `fixed-block baseline` against `fixed-block instrumented`,
/// with the silent arm beside them, every field of the twelve reported as identical or named.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_cross_stage_instrumentation_changed_no_answer_field_by_field() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let root = evidence_root();
    let (silent, baseline, instrumented) =
        run_three_arms(&fixture, &state, &root, "correctness").await;

    let rows = [
        ("silent", silent.identity()),
        ("baseline", baseline.identity()),
        ("instrumented", instrumented.identity()),
    ];
    let names = rows[0]
        .1
        .as_object()
        .expect("a row of §27's fields")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut expected = COMPARISON_FIELDS.to_vec();
    expected.sort();
    assert_eq!(
        names, expected,
        "the row §27 compares is not the field list this file names: {names:?} vs {expected:?}"
    );

    let mut verdicts = serde_json::Map::new();
    for name in &expected {
        let values: Vec<&Value> = rows.iter().map(|(_, row)| &row[*name]).collect();
        let identical = values.iter().all(|value| *value == values[0]);
        if !identical {
            verdicts.insert(
                (*name).to_string(),
                json!({
                    "identical": false,
                    "silent": values[0],
                    "baseline": values[1],
                    "instrumented": values[2],
                }),
            );
        }
    }
    assert!(
        verdicts.is_empty(),
        "§27: the cross-stage instrumentation changed a field: {verdicts:?}"
    );

    // The two stronger claims beside the named list: the result struct's own equality, and its
    // whole serialization, which is what a field this file did not think of would fail on.
    assert_eq!(
        baseline.result, instrumented.result,
        "§27: the results differ even though every named field matched"
    );
    assert_eq!(
        silent.whole_result(),
        instrumented.whole_result(),
        "§27: the silent arm's serialized result differs from the instrumented arm's"
    );
    assert_eq!(
        baseline.whole_result(),
        instrumented.whole_result(),
        "§27: the baseline arm's serialized result differs from the instrumented arm's"
    );
    assert_eq!(
        baseline.result.fingerprint(),
        instrumented.result.fingerprint(),
        "§27: the fingerprint — a hash of every field above — differs"
    );
    // §27's `identical` is about the answer, and the answer has to be an answer: a run that
    // reverted in all three arms would agree with itself everywhere.
    assert!(
        instrumented.result.revert().is_none(),
        "all three arms reverted, so §27 compared three failures"
    );

    // And the tables the instrumented arm added describe *this* run rather than an empty shell.
    let instrumented_dir = instrumented
        .dir
        .expect("an instrumented arm has a directory");
    let calls = read_json(&instrumented_dir.join(PIPELINE_CALLS_FILE));
    assert_eq!(
        calls["rows"].as_array().map(Vec::len),
        Some(instrumented.events.len()),
        "the call table does not hold one row per call this arm made"
    );
    let matrix = read_json(&instrumented_dir.join(DUPLICATE_MATRIX_FILE));
    let summary = read_json(&instrumented_dir.join(DUPLICATE_SUMMARY_FILE));
    assert_eq!(
        matrix["totals"]["pairs"], summary["duplicate_pairs"],
        "the two tables that count the same pairs disagree"
    );
}

/// §29 group 4: prove the comparison above can fail. One field of one arm's row is broken at a
/// time, and the diffing function §27's test uses has to name that field every time — a
/// comparison that reports nothing for a broken value is not a comparison, and M8.4.1's
/// §16 gate is the precedent for checking that.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_different_answer_would_be_flagged_field_by_field() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let root = scratch_root();
    let (_silent, baseline, instrumented) =
        run_three_arms(&fixture, &state, &root, "negative-control").await;
    let left = baseline.identity();
    let right = instrumented.identity();
    assert_eq!(
        left, right,
        "the two arms disagree before the test breaks anything"
    );

    for name in COMPARISON_FIELDS {
        let mut broken = right.clone();
        break_one_field(&mut broken, name);
        assert_ne!(
            broken[name], left[name],
            "§29: breaking `{name}` did not change the value §27 compares, so that row is not \
             reading the answer — it is reading something that cannot disagree"
        );
    }
}

/// Break one of §27's twelve rows, and break it in the row's own value: the composite rows
/// (`gas`, `logs`, `profit`, `route`, `block`) are broken at their first scalar, in the order
/// the table's keys happen to be written in — which is enough, because §27's compare is over
/// the whole row, not over one of its members.
///
/// `return_data` is the row this proves less than the others. This build publishes the *reason*
/// there is no such field, so breaking it shows only that §27's compare reads the row; the
/// claim that the bytes behind a step's output are the same in both arms is
/// [`Arm::whole_result`]'s, and §27's test compares that too.
fn break_one_field(row: &mut Value, name: &str) {
    fn first_scalar(value: &mut Value) -> Option<&mut Value> {
        match value {
            leaf @ (Value::Number(_) | Value::String(_) | Value::Bool(_) | Value::Null) => {
                Some(leaf)
            }
            Value::Object(map) => map.values_mut().find_map(first_scalar),
            Value::Array(values) => values.iter_mut().find_map(first_scalar),
        }
    }
    fn bump(value: &mut Value) {
        match value {
            Value::Number(number) => {
                *value = json!(number.as_u64().map_or(1, |n| n + 1) + 1);
            }
            Value::String(text) => *value = json!(format!("{text}-broken")),
            Value::Bool(flag) => *value = json!(!*flag),
            Value::Null => *value = json!("broken"),
            _ => {}
        }
    }
    let object = row.as_object_mut().expect("a row");
    let child = &mut object[name];
    match first_scalar(child) {
        Some(scalar) => bump(scalar),
        // A row with no scalar anywhere is a row this file would rather not have written.
        None => *child = json!("broken"),
    }
}

/// §12's Experiment B, in the form this milestone can stand behind: one block, three arms, and
/// the five tables this milestone publishes come out the same every time.
///
/// The tables are read back out of the three directories rather than recomputed here, so what
/// is compared is what a reader gets. The four fields a replay cannot reproduce — the three
/// monotonic timings and the wall-clock stamp — and the run's own name are stripped before the
/// compare, and `stripped` says why each is a fact about the process rather than about the
/// reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn three_fixed_block_arms_publish_the_same_cross_stage_tables() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let arms_root = fresh_subtree(&evidence_root(), "fixed-block");
    let mut arms = Vec::new();
    for name in B_RUNS {
        let dir = arms_root.join(name);
        let arm = run_arm(
            &state,
            &fixture.route,
            &fixture.header,
            name,
            true,
            Some(Tables::DependencyAndCrossStage),
            Some(&dir),
        )
        .await;
        assert_eq!(
            listing(&dir),
            files_the_writer_owes(Some(Tables::DependencyAndCrossStage), false),
            "{name}: not the file set one instrumented arm writes"
        );
        assert_eq!(
            arm.arrivals.len(),
            41,
            "{name}: a §12 arm that put another number of calls on the wire is not the same run"
        );
        arms.push((dir, arm));
    }

    // §12's four controls. Three of them are read out of each arm's own per-run table — the
    // chain, the height, and which state source answered — and the fourth, the route, from the
    // arm's result fingerprint, because the run table classifies calls and does not carry the
    // transaction the calls were made for.
    let mut fingerprints = Vec::new();
    for (dir, arm) in &arms {
        let per_run = read_json(&dir.join(CROSS_STAGE_RUN_FILE));
        let provenance = &per_run["run_provenance"];
        assert_eq!(
            provenance["chain_id"],
            json!(CHAIN.0),
            "§12: chain_id identical"
        );
        assert_eq!(
            provenance["block_number"],
            json!(BLOCK),
            "§12: block identical"
        );
        assert_eq!(
            provenance["source"],
            json!("fixture"),
            "§12: one state snapshot"
        );
        assert_eq!(
            provenance["execution_mode"],
            json!(ExecutionMode::BuildOnly.name()),
            "§12: §28's build-only on every arm"
        );
        assert_eq!(
            per_run["asks"].as_u64(),
            Some(41),
            "the arm's own table does not count the 41 asks it made: {per_run}"
        );
        fingerprints.push(arm.result.fingerprint());
    }
    assert!(
        fingerprints.windows(2).all(|pair| pair[0] == pair[1]),
        "§12: route identical — the three arms' result fingerprints differ: {fingerprints:?}"
    );
    // The endpoint each arm talked to, spelled out rather than stripped silently: `Stub::spawn`
    // binds `127.0.0.1:0`, so the port — and with it the digest — is a fact about the process,
    // which is why `UNREPRODUCIBLE` removes it before the table compare. What §6's
    // same-endpoint condition actually needs is that *one arm's* rows share one digest, and
    // that is checked here per arm rather than across arms.
    const LIVE_ENDPOINT: &str = "rpc-faa716cada04a9ef";
    for (dir, _) in &arms {
        let calls = read_json(&dir.join(PIPELINE_CALLS_FILE));
        let digests: std::collections::BTreeSet<String> = calls["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["endpoint_id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            digests.len(),
            1,
            "{}: one arm reached {digests:?}",
            dir.display()
        );
        let digest = digests.into_iter().next().expect("one digest");
        assert!(
            digest.starts_with("rpc-"),
            "an arm published an endpoint that is not a stub digest: {digest}"
        );
        assert_ne!(
            digest, LIVE_ENDPOINT,
            "a fixture arm reached the live node, which §11 and §28 both forbid"
        );
        println!("  {} endpoint digest {digest}", dir.display());
    }

    // §14's five tables, arm after arm.
    let dirs: Vec<PathBuf> = arms.iter().map(|(dir, _)| dir.clone()).collect();
    let names: Vec<String> = dirs
        .iter()
        .map(|dir| {
            dir.file_name()
                .expect("a run directory")
                .to_string_lossy()
                .to_string()
        })
        .collect();
    let digests: Vec<String> = dirs
        .iter()
        .zip(&names)
        .map(|(dir, name)| {
            let mut text = String::new();
            for file in CROSS_STAGE_FILES.into_iter().chain([CROSS_STAGE_RUN_FILE]) {
                let table = read_json(&dir.join(file));
                text.push_str(&serde_json::to_string(&stripped(table, name)).expect("a table"));
            }
            text
        })
        .collect();
    assert!(
        digests.windows(2).all(|pair| pair[0] == pair[1]),
        "§12: the relations are not reproducible across three arms of one block"
    );
    assert!(
        digests[0].contains("\"asks\":41"),
        "the five tables that just matched do not carry the arm's own ask count, so the compare \
         above was against something smaller than a table"
    );

    // The zero, with its reason on the record rather than inferred.
    let first = &dirs[0];
    let summary = read_json(&first.join(DUPLICATE_SUMMARY_FILE));
    let candidates = read_json(&first.join(REUSE_CANDIDATES_FILE));
    assert_eq!(
        summary["duplicate_pairs"].as_u64(),
        Some(0),
        "a one-stage fixture arm found a duplicate: {summary}"
    );
    assert_eq!(candidates["candidates"].as_u64(), Some(0));
    assert_eq!(candidates["safe_to_reuse"].as_u64(), Some(0));
    let calls = read_json(&first.join(PIPELINE_CALLS_FILE));
    let sinks: Vec<&str> = {
        let mut seen: Vec<&str> = calls["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| row["sink"].as_str().unwrap_or_default())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    };
    assert_eq!(
        sinks,
        vec!["simulation"],
        "the fold found a second sink in a fixture arm, and §12's zero needs re-explaining"
    );
    // M8.4.1's own count in the same directory is the corroboration that no duplicate was
    // missed rather than merely not cross-stage: within the one stage, this route asks each
    // word once.
    let duplicates = read_json(&first.join(DUPLICATES_FILE));
    for source in duplicates["per_source"].as_array().expect("per_source") {
        assert_eq!(
            source["duplicate_state_reads"].as_u64(),
            Some(0),
            "M8.4.1 measured a duplicate this fold did not see: {source}"
        );
    }

    // §13's rule applied to a corpus with no path: eleven named rows, none of them `measured`,
    // each with the reason it has no number — and the two reasons this corpus earns
    // (`not_applicable_no_such_stage` for a stage nothing stamps, and
    // `measured_one_side_issued_no_calls`) both present.
    let stage_pairs = read_json(&first.join(STAGE_PAIRS_FILE));
    let rows = stage_pairs["rows"].as_array().expect("named pair rows");
    assert_eq!(rows.len(), 11, "§13 names eleven stage pairs");
    let mut statuses = Vec::new();
    for row in rows {
        let status = row["status"].as_str().unwrap_or_default();
        assert_ne!(
            status, PAIR_MEASURED,
            "{}: a one-sink arm measured a cross-stage pair",
            row["pair"]
        );
        assert_ne!(
            status, PAIR_NO_DUPLICATES,
            "{}: `measured_no_duplicates` claims both sides asked something, and one side of \
             every named pair here asked nothing",
            row["pair"]
        );
        assert!(
            row["why"].as_str().is_some_and(|why| !why.is_empty()),
            "{}: a row with no number and no reason",
            row["pair"]
        );
        statuses.push(status.to_string());
    }
    assert!(
        statuses.iter().any(|s| s == PAIR_STAGE_ABSENT),
        "statuses were {statuses:?}"
    );
    assert!(
        statuses.iter().any(|s| s == PAIR_NO_ASKS_OBSERVED),
        "statuses were {statuses:?}"
    );
}

/// §3's promise, checked at the file level rather than asserted in a comment: this milestone's
/// switch *adds* tables and rewrites none of M8.4.1's.
///
/// The fifteen shared files are compared between the baseline arm and the instrumented arm on
/// the two things a reader of those directories recomputes: the shape of every object — every
/// key, at every depth, so a field cannot appear or disappear when the switch is turned on —
/// and every count: the length of every list and every integer leaf whose name says it is a
/// count rather than a duration. What is deliberately not compared is any duration; two arms are
/// two processes, and M8.4.1 §7's rule is that its monotonic clock restarts with each run, so
/// `duration_ns` is expected to differ and is not collected here at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn adding_the_cross_stage_tables_left_m841s_shape_and_counts_alone() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let root = scratch_root();
    let (_silent, baseline, instrumented) =
        run_three_arms(&fixture, &state, &root, "isolation").await;
    let baseline_dir = baseline.dir.expect("a baseline arm has a directory");
    let instrumented_dir = instrumented
        .dir
        .expect("an instrumented arm has a directory");

    // The shared set is M8.4.1's fourteen data files — README is prose that lists the
    // directory's files, so it is expected to differ and is checked separately below.
    let shared: Vec<String> = baseline
        .files
        .iter()
        .filter(|name| name != &&README_FILE.to_string())
        .cloned()
        .collect();
    assert_eq!(
        shared.len(),
        14,
        "M8.4.1's fixture directory is fifteen files of which one is the README; the shared set \
         was {shared:?}"
    );

    let mut collected = 0usize;
    for name in &shared {
        let left = read_table(&baseline_dir.join(name));
        let right = read_table(&instrumented_dir.join(name));
        let (left_shape, left_numbers) = shape_and_counts(&left);
        let (right_shape, right_numbers) = shape_and_counts(&right);
        collected += left_numbers.len();
        assert_eq!(
            left_shape,
            right_shape,
            "{name}: the cross-stage switch changed which fields this table publishes — {:?}",
            left_shape
                .symmetric_difference(&right_shape)
                .collect::<Vec<_>>()
        );
        let moved: Vec<(&String, Option<i128>, Option<i128>)> = left_numbers
            .iter()
            .filter(|(path, value)| right_numbers.get(*path) != Some(*value))
            .map(|(path, value)| (path, Some(*value), right_numbers.get(path).copied()))
            .collect();
        assert!(
            moved.is_empty(),
            "{name}: a count moved between the two arms: {moved:?}"
        );
    }
    assert!(
        collected > 40,
        "the two arms agreed on {collected} counts, which is not enough fields for that to mean \
         anything — the collector is reading a shape it should not be"
    );

    // The positive controls, on the numbers M8.4.1 published for this same fixture: a comparison
    // that collected only zeros would pass everything above.
    let calls = read_json(&instrumented_dir.join(PIPELINE_CALLS_FILE));
    assert_eq!(
        calls["rows"].as_array().map(Vec::len),
        Some(41),
        "the call table the instrumented arm wrote is not the 41-row table M8.4.1's arms write"
    );
    let storage = read_json(&instrumented_dir.join(STORAGE_READS_FILE));
    assert_eq!(
        storage["rows"].as_array().map(Vec::len),
        Some(21),
        "the storage table is not the 21-row table M8.4.1's arms write"
    );
    let map = read_json(&instrumented_dir.join(DEPENDENCY_MAP_FILE));
    assert_eq!(
        map["summary"]["total"].as_u64(),
        Some(21),
        "the dependency map does not count the same 21 nodes its own rows hold"
    );

    // And the README, which describes the switches the directory was written under. Both arms
    // name this milestone's five files — the writer documents the switch either way — so what
    // has to differ is the claim about whether it was on: the baseline's says it was not, the
    // instrumented's says it was, and each matches the files in its own directory above.
    let base_readme =
        std::fs::read_to_string(baseline_dir.join(README_FILE)).expect("the baseline README");
    let inst_readme = std::fs::read_to_string(instrumented_dir.join(README_FILE))
        .expect("the instrumented README");
    assert!(
        base_readme.contains("did not ask for M8.4.2"),
        "the baseline README does not say the switch was off, so a reader cannot tell from the \
         directory itself which tables it is missing"
    );
    assert!(
        !inst_readme.contains("did not ask for M8.4.2"),
        "the instrumented README says the switch was off while its own directory holds five \
         tables that only that switch writes"
    );
    for name in CROSS_STAGE_FILES.into_iter().chain([CROSS_STAGE_RUN_FILE]) {
        assert!(
            inst_readme.contains(name),
            "the instrumented README does not mention {name}, which is in its own directory"
        );
        assert!(
            instrumented.files.iter().any(|file| file == name),
            "the README describes {name} but the directory does not hold it"
        );
        assert!(
            !baseline.files.iter().any(|file| file == name),
            "the baseline arm published {name}, which only this milestone's switch writes"
        );
    }
}

/// The field names that hold counts. A key is collected if it names a count outright or ends in
/// one of the count suffixes the writers use; anything that ends `_ns` or `_ms`, or is a
/// percentile, a ratio term, or an endpoint digest is *not* collected, because those are the
/// two arms' clocks and ports rather than their answers.
fn is_count_key(key: &str) -> bool {
    if matches!(
        key,
        "count"
            | "asks"
            | "asked"
            | "calls"
            | "samples"
            | "total"
            | "independent"
            | "ordered"
            | "unknown"
            | "simulations"
            | "simulations_recorded"
            | "duplicates"
            | "pairs"
            | "candidates"
            | "safe_to_reuse"
            | "refused_by_block_identity"
    ) {
        return true;
    }
    key.ends_with("_count")
        || key.ends_with("_counts")
        || key.ends_with("_asks")
        || key.ends_with("_calls")
        || key.ends_with("_reads")
        || key.ends_with("_pairs")
        || key.ends_with("_simulations")
        || key.ends_with("_nodes")
        || key.ends_with("_edges")
        || key.ends_with("_slots")
        || key.ends_with("_addresses")
}

/// One table, reduced to (a) the set of field paths it publishes, at every depth, and (b) every
/// count in it: the length of every list, indexed by position so two lists of different lengths
/// cannot agree by overwriting each other's entry, and every integer leaf [`is_count_key`]
/// accepts.
fn shape_and_counts(value: &Value) -> (BTreeSet<String>, BTreeMap<String, i128>) {
    fn walk(
        value: &Value,
        path: &str,
        shape: &mut BTreeSet<String>,
        counts: &mut BTreeMap<String, i128>,
    ) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let here = format!("{path}.{key}");
                    shape.insert(here.clone());
                    walk(child, &here, shape, counts);
                }
            }
            Value::Array(values) => {
                for (index, child) in values.iter().enumerate() {
                    counts.insert(format!("{path}[{index}]"), values.len() as i128);
                    walk(child, &format!("{path}[]"), shape, counts);
                }
            }
            Value::Number(number) => {
                let key = path.rsplit('.').next().unwrap_or(path);
                if is_count_key(key) {
                    if let Some(integer) = number.as_i128() {
                        counts.insert(path.to_string(), integer);
                    }
                }
            }
            _ => {}
        }
    }
    let mut shape = BTreeSet::new();
    let mut counts = BTreeMap::new();
    walk(value, "", &mut shape, &mut counts);
    (shape, counts)
}

/// The fields of these tables a replay of the same input cannot reproduce, each because it is
/// a fact about the process that ran: the three monotonic timings, measured from an origin that
/// restarts with the run (M8.4.1 §7's rule, inherited here); the wall-clock stamp of the
/// assembly; and the endpoint digest, which for these arms is a hash of the stub's URL and
/// `Stub::start` binds `127.0.0.1:0`, so every arm is handed a different ephemeral port. The
/// §6 same-endpoint condition is not dropped by this — its *outcome* survives in every table,
/// because each arm's rows all carry one digest and the pair builder refuses two rows whose
/// digests differ; and the digests themselves are asserted above the compare, so the strip is
/// not hiding a second endpoint. `run` is stripped by name because it is the arm's own label,
/// which appears inside every `logical_request_id`.
const UNREPRODUCIBLE: [&str; 5] = [
    "started_ns",
    "finished_ns",
    "duration_ns",
    "generated_at_unix_ms",
    "endpoint_id",
];

/// A table with those fields removed at every depth, so three arms' tables can be compared
/// without the clock or the directory name setting the answer.
fn stripped(value: Value, run: &str) -> Value {
    fn strip(value: &mut Value, run: &str) {
        match value {
            Value::Object(map) => {
                map.retain(|key, _| !UNREPRODUCIBLE.contains(&key.as_str()));
                for (_, child) in map.iter_mut() {
                    strip(child, run);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(|child| strip(child, run)),
            Value::String(text) => {
                let replaced = text.replace(run, "arm");
                *text = replaced;
            }
            _ => {}
        }
    }
    let mut value = value;
    strip(&mut value, run);
    value
}

/// The committed-directory gate this file's own §21 analogue: `M842_FIXTURE_EVIDENCE` is what
/// regenerates the evidence tree, and a plain `cargo test` writes under `target/`. This test
/// only records which of the two a reader is looking at, so a directory that appears to hold
/// three arms but was written by a different build says so.
#[test]
fn these_arms_went_where_the_reader_can_find_them() {
    let root = evidence_root();
    let scratch = workspace_root().join("target/simulation-tests/m8.4.2-cross-stage");
    let committed = root.starts_with(workspace_root().join("data/evidence"));
    // The switch is the only thing that decides, so a reader can tell from the environment
    // which tree a set of directories came from — and this test cannot pass by accident,
    // because it asks for one of exactly two paths rather than for a directory to exist.
    match std::env::var("M842_FIXTURE_EVIDENCE") {
        Ok(dir) if !dir.is_empty() => assert!(
            committed,
            "M842_FIXTURE_EVIDENCE={dir} points outside the committed evidence tree at {}",
            root.display()
        ),
        _ => assert_eq!(
            root,
            scratch,
            "with the switch unset these arms belong under `target/`, and they landed at {}",
            root.display()
        ),
    }
    if committed {
        // §31's shape of this milestone's tree: the arms that exist to prove a gate would
        // notice (the three controls) are re-derivable in seconds from the same build and are
        // not part of the evidence, so they must not leak in beside the two that are.
        for control in ["no-extra-rpc", "negative-control", "isolation"] {
            assert!(
                !root.join(control).exists(),
                "{control} is a control arm, not evidence; it was published into the committed \
                 tree at {}, which would make the tree hold directories the report never reads",
                root.display()
            );
        }
    }
    println!(
        "M8.4.2's fixed-block arms are under {} ({})",
        root.display(),
        if committed {
            "the committed evidence tree"
        } else {
            "a scratch tree; set M842_FIXTURE_EVIDENCE to regenerate the committed one"
        }
    );
}
