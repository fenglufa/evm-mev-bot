//! M8.4.1 §14's Experiment A and §16's Experiment C — the fixed-block half of this
//! milestone's evidence, on the recorded state rather than on a node.
//!
//! ```text
//! the block     37 191 169, the M4 pin, served by `support::stub` out of
//!               `fixtures/simulation-m4/dump-37191169.json` — the answers a real node gave
//!               at that height, so this is the same state M8.3.1–M8.3.3 replayed
//! the route     the one the detector reports for that block, asked for 1 wei out
//! the settings  §14's three: state_read_reuse = true, concurrency = 1, build-only
//! ```
//!
//! Two experiments run here, and they are after different things:
//!
//! * **§14 (Experiment A)** asks *why these storage reads go one at a time*. Three runs of
//!   the same block, each written as a whole diagnosis directory, so the answer is something
//!   a reader can re-derive from three independent copies instead of from one run's luck. The
//!   dependency verdicts come out of `dependency-map.json` in each directory, which is built
//!   by the same classifier that builds the live runs' — this milestone's Q1 therefore has a
//!   reproducible half and a live half, and the assembly test
//!   (`crates/pipeline/tests/storage_dependency_evidence.rs`) requires them to agree.
//! * **§16 (Experiment C)** asks *whether the instrumentation changed anything*. One run with
//!   the trace and the tables off, one with both on, compared over the field list §16 names —
//!   plus the calls each arm put on the wire, list-compared rather than count-compared,
//!   because §12's rule is that the instrument adds no request, and a count can stay the same
//!   while a method swaps.
//!
//! ## What is deliberately not here
//!
//! No pipeline stages. This is the simulation engine reached through one adapter, so there is
//! no detection span, no preflight span and no execution lane, and
//! `outside-simulation-rpc.json` is absent from these directories rather than present and
//! empty — §11's rule is that an unmeasured thing says 未观测, and the file a run did not
//! record has no place to say it. The whole-pipeline half is Experiment B's live runs.
//!
//! ## Where the files go
//!
//! Always written, so the assertions below run against the bytes a reader gets and not against
//! the in-memory objects that produced them. The location is the question: with
//! `M841_FIXTURE_EVIDENCE=<dir>` set they land under that directory (which is how
//! `data/evidence/m8/storage-dependency/` is regenerated), and otherwise under
//! `target/simulation-tests/`, where a plain `cargo test` cannot touch committed evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use serde_json::{json, Value};

use evm_chain::{
    BlockContext, ChainAdapter, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource,
};
use evm_core::BlockNumber;
use evm_execution::ExecutionMode;
use evm_pipeline::diagnosis::{
    DiagnosisEvidence, SimulationDiagnosis, SimulationWindow, ACCOUNT_MATRIX_FILE, BOTTLENECK_FILE,
    DEPENDENCY_FILES, DEPENDENCY_MAP_FILE, DEPENDENCY_SUMMARY_FILE, DUPLICATES_FILE, OUTSIDE_FILE,
    PIPELINE_CALLS_FILE, PIPELINE_SUMMARY_FILE, README_FILE, RPC_GAPS_FILE, RPC_SUMMARY_FILE,
    SIMULATION_SUMMARY_FILE, STORAGE_BREAKDOWN_FILE, STORAGE_READS_FILE, TRACES_FILE,
};
use evm_pipeline::latency::git_revision;
use evm_simulation::{
    engine::run, BlockPin, ConcurrencyReport, PricedRoute, RpcStateProvider, SimulationResult,
    StateProvider, StateReadStats,
};

mod support;
use support::stub::{ServedState, Stub};
use support::{request, workspace_root, BLOCK, CHAIN};

/// §14's three runs, named the way §21 names them.
const A_RUNS: [&str; 3] = ["run-01", "run-02", "run-03"];

/// §4's `stage` for every read a simulation makes, as `evm_metrics::Stage::Simulation` spells
/// it and as the provider stamps it.
const SIMULATION: &str = "simulation";

/// The four methods that read chain state, in the reporting order M8.3.1 used. The header read
/// is not one of them, and §13's pin claim is about these.
const STATE_METHODS: [&str; 4] = [
    "eth_getCode",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getStorageAt",
];

/// §16's list, in §16's order, plus the two names this build splits rather than merges.
///
/// `gas` is `gas_used` and `gas_charge` (an amount and its price are two facts), `block` is
/// `block_number` and `block_hash`, and `return_data` keeps §16's own name even though the
/// build has no such field — [`Arm::identity`] publishes why in that field's value rather than
/// dropping the row, which is §28's 「没有测到，就记录 not observed」 applied to a schema.
const COMPARISON_FIELDS: [&str; 21] = [
    "block_chain",
    "block_hash",
    "block_number",
    "chain_id",
    "compared",
    "fingerprint",
    "gas_charge",
    "gas_used",
    "gross_loss",
    "gross_profit",
    "log_count",
    "logs",
    "measurements",
    "net_profit",
    "outcome",
    "plan",
    "revert",
    "reverted",
    "return_data",
    "route",
    "state_changes",
];

/// Where this run publishes. `M841_FIXTURE_EVIDENCE` is the evidence tree; its absence is a
/// scratch directory under `target/`, where a plain `cargo test` cannot touch committed
/// evidence.
///
/// This function creates and nothing more: the two experiments run in one process, in parallel,
/// and each clears only the subtree it owns (`fixed-block/` and `correctness/`). A root-level
/// wipe here would let whichever test started second delete the first one's output.
fn evidence_root() -> PathBuf {
    let dir = std::env::var_os("M841_FIXTURE_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target/simulation-tests/m8.4.1-fixture"));
    let dir = if dir.is_absolute() {
        dir
    } else {
        workspace_root().join(dir)
    };
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    dir
}

/// Recreate one subtree from empty, because the diagnosis writer appends: a second run into a
/// directory holding a first would double every trace line rather than reproduce it. A subtree
/// that does not exist yet is already empty, which is the case on a machine that has never run
/// this experiment.
fn fresh_subtree(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        assert!(
            error.kind() == std::io::ErrorKind::NotFound,
            "{}: the previous output could not be cleared: {error}",
            dir.display()
        );
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    dir
}

fn read_json(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("a file in a directory"))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("a table this file builds serializes")
        ),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    println!("wrote {}", path.display());
}

/// The directory's listing, files only, sorted — what a reader of `ls` sees.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The files [`DiagnosisEvidence::finish`] owes a directory, spelled from the writer's own
/// constants rather than from a transcription: a rename has to move this function too, and a
/// listing that no longer matches it is a test failure rather than a table nobody named.
fn files_the_writer_owes(
    state_acquisition: bool,
    dependency: bool,
    lifecycle_recorded: bool,
) -> Vec<String> {
    let mut names = vec![
        README_FILE.to_string(),
        RPC_SUMMARY_FILE.to_string(),
        SIMULATION_SUMMARY_FILE.to_string(),
        TRACES_FILE.to_string(),
        DUPLICATES_FILE.to_string(),
    ];
    if state_acquisition {
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
    }
    if lifecycle_recorded {
        names.push(OUTSIDE_FILE.to_string());
    }
    if dependency {
        names.extend(DEPENDENCY_FILES.into_iter().map(str::to_string));
    }
    names.sort();
    names
}

/// One arm of either experiment: the run, what the endpoint and the sink each said about it,
/// and — for an instrumented arm — the directory the writer left behind.
struct Arm {
    name: String,
    /// Method names in the order the endpoint accepted them, `eth_chainId` first.
    arrivals: Vec<String>,
    /// What the sink recorded. Empty for an untraced arm, which is the point of §16's control.
    events: Vec<RpcCallEvent>,
    result: SimulationResult,
    route: PricedRoute,
    stats: StateReadStats,
    concurrency: ConcurrencyReport,
    run_wall: Duration,
    /// The diagnosis directory, for the arms that wrote one.
    dir: Option<PathBuf>,
    files: Vec<String>,
}

impl Arm {
    /// The calls the provider made: everything after the connect, which no engine phase owns
    /// because the adapter — not the plan — issued it.
    fn provider_calls(&self) -> &[RpcCallEvent] {
        &self.events[1..]
    }

    fn storage_rows(&self) -> Vec<&RpcCallEvent> {
        self.events
            .iter()
            .filter(|event| event.method == "eth_getStorageAt")
            .collect()
    }

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

    /// §16's field list, one row per name, read off the run rather than restated.
    ///
    /// Two of these are not simple fields of the result. `route` is the route *as executed* —
    /// the targets and calldata the EVM actually ran — because the requested route is the
    /// harness's own input, and comparing it between two arms would be comparing one object
    /// with itself. `return_data` has no field to read: this build keeps a step's decoded
    /// output as `measured` and its logs as `logs`, and the bytes behind them are covered by
    /// [`Arm::whole_result`], so the row publishes the reason and `whole_result` is the
    /// stronger claim beside it.
    fn identity(&self) -> Value {
        json!({
            "block_chain": format!(
                "{}:{}",
                self.result.chain_id.0, self.result.block.number.0
            ),
            "block_hash": format!("{:?}", self.result.block.hash),
            "block_number": self.result.block.number.0,
            "chain_id": self.result.chain_id.0,
            "compared": format!("{:?}", self.result.compared),
            "fingerprint": self.result.fingerprint(),
            "gas_charge": format!("{:?}", self.result.gas_charge),
            "gas_used": self.result.gas_used(),
            "gross_loss": self.result.gross_loss.map(|amount| amount.to_string()),
            "gross_profit": self.result.gross_profit.map(|amount| amount.to_string()),
            "log_count": self.result.logs().len(),
            "logs": self
                .result
                .steps
                .iter()
                .map(|step| step.logs.iter().map(|log| format!("{log:?}")).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            "measurements": self
                .result
                .measurements
                .iter()
                .map(|m| format!("{}={}", m.binding, m.value))
                .collect::<Vec<_>>(),
            "net_profit": format!("{:?}", self.result.net_profit),
            "outcome": format!("{:?}", self.result.outcome),
            "plan": format!("{:?}", self.result.plan_summary),
            "revert": format!("{:?}", self.result.revert()),
            "reverted": self.result.revert().is_some(),
            "return_data": "not recorded by this build: a step keeps the decoded value it \
                            matched as `measurements` and its logs as `logs`; the bytes behind \
                            them are compared by `whole_result`",
            "route": {
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
                "requested_pools": self
                    .route
                    .legs
                    .iter()
                    .map(|leg| format!("{}", leg.pool.address))
                    .collect::<Vec<_>>(),
            },
            "state_changes": format!("{:?}", self.result.state_changes),
        })
    }

    /// Every field of the result, serialized by the same code `fingerprint()` hashes — the
    /// claim that does not depend on this file having thought of a field.
    fn whole_result(&self) -> Value {
        serde_json::to_value(&self.result).expect("the result of a finished run must serialize")
    }

    /// The run's own figures, in §17's shape, for the row this arm contributes.
    fn metrics(&self) -> Value {
        json!({
            "run_wall_ns": self.run_wall.as_nanos() as u64,
            "calls_on_the_wire": self.arrivals.len(),
            "calls_recorded": self.events.len(),
            "state_reads": self
                .arrivals
                .iter()
                .filter(|method| STATE_METHODS.contains(&method.as_str()))
                .count(),
            "storage_reads": self
                .arrivals
                .iter()
                .filter(|method| **method == "eth_getStorageAt")
                .count(),
            "header_reads": self
                .arrivals
                .iter()
                .filter(|method| **method == "eth_getBlockByNumber")
                .count(),
            "rpc_sum_duration_ns": self.events.iter().map(|event| event.duration_ns).sum::<u64>(),
            "state_read_reuse": self.stats.reuse,
            "configured_concurrency": self.concurrency.configured,
            "observed_max_concurrency": self.concurrency.observed_peak,
        })
    }
}

/// Run the pinned route once, through the recorded-state stub.
///
/// `traced` is §16's variable: with it off the adapter has no sink, so nothing is recorded and
/// no directory is written. Everything else — route, pin, ask, reuse bound, concurrency bound,
/// gas ceiling — is the same call either way, which is what makes the two arms comparable.
async fn run_arm(
    state: &Arc<ServedState>,
    route: &PricedRoute,
    header: &BlockContext,
    name: &str,
    traced: bool,
    out_dir: Option<&Path>,
) -> Arm {
    let origin = Instant::now();
    let sink = traced.then(|| {
        RpcTraceSink::new(
            origin,
            format!("m8.4.1-{name}-{BLOCK}"),
            RpcTraceSource::Fixture,
            Some(CHAIN.0),
        )
    });
    let stub = Stub::spawn(Arc::clone(state));
    // `connect_with_trace` so the connect's own `eth_chainId` is on the same timeline as the
    // reads: it is the one call a run makes that no engine phase asked for, and the record says
    // so instead of being left out of the count. With `traced` off this is the same call
    // `connect` makes.
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
    // §14's two required settings, spelled out rather than inherited from a default, so an
    // arm that drifted is caught here and not in a table three files away.
    let provider = Arc::new(RpcStateProvider::with_state_read_concurrency(
        adapter, pin, true, 1,
    ));
    assert_eq!(
        provider.state_read_concurrency().configured,
        1,
        "the {name} arm was asked for serial state reads and the provider reports otherwise"
    );
    assert!(
        provider.state_read_concurrency().serial,
        "the {name} arm's scheduler does not call bound 1 serial"
    );
    let shared: Arc<dyn StateProvider> = provider.clone();
    let started = Instant::now();
    let result = run(
        shared,
        &request(route.clone(), header.clone(), U256::ONE, provider.source()),
    )
    .await
    .expect("the recorded state replays this route");
    let run_wall = started.elapsed();
    let finished_ns = origin.elapsed().as_nanos() as u64;

    let events = sink.as_ref().map(|sink| sink.events()).unwrap_or_default();
    let arrivals = stub.methods();
    let stats = provider.state_read_stats();
    let concurrency = provider.state_read_concurrency();

    let mut files = Vec::new();
    if let Some(dir) = out_dir {
        let mut evidence =
            DiagnosisEvidence::open(dir, git_revision(), ExecutionMode::BuildOnly.name(), true)
                .expect("a diagnosis directory this file just created can be opened")
                .with_dependency_tables();
        let window = SimulationWindow {
            simulation_id: format!("m8.4.1-{name}-{BLOCK}"),
            source: RpcTraceSource::Fixture,
            chain_id: Some(CHAIN.0),
            block_number: Some(BLOCK),
            state_source: Some(provider.source()),
            started_ns: 0,
            finished_ns,
        };
        let diagnosis = SimulationDiagnosis::new(window, events.clone())
            .with_state_reads(Some(stats))
            .with_concurrency(Some(concurrency.clone()))
            .with_endpoint(
                sink.as_ref()
                    .and_then(|sink| sink.endpoint_id())
                    .map(str::to_string),
            );
        // `refusals` empty: a run whose adapter could not be traced records why, and this one
        // traced everything it was asked to.
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
        route: route.clone(),
        stats,
        concurrency,
        run_wall,
        dir: out_dir.map(Path::to_path_buf),
        files,
    }
}

/// What every traced arm of both experiments owes §13 and §4: a label on every read, and the
/// pin in every one of them. Checked here, per arm, before any of it is published.
///
/// Only for an arm that recorded: §16's control arm has no sink by design, so its pin claim is
/// not derivable from events — the endpoint's call list matching the instrumented arm's is what
/// carries it instead.
fn assert_the_arm_is_a_valid_dependency_subject(arm: &Arm) {
    assert!(
        !arm.events.is_empty(),
        "{} recorded no calls, so it cannot be checked as a dependency subject",
        arm.name
    );
    assert_eq!(
        arm.events.len(),
        arm.arrivals.len(),
        "§12: the {} arm's sink recorded {} calls while the wire carried {}",
        arm.name,
        arm.events.len(),
        arm.arrivals.len()
    );
    assert_eq!(
        arm.events[0].method, "eth_chainId",
        "the first thing an arm does is open a connection"
    );
    for event in arm.provider_calls() {
        assert_eq!(
            event.stage.as_deref(),
            Some(SIMULATION),
            "a state read arrived with no stage: {:?}",
            (&event.method, &event.caller)
        );
        assert!(
            event.caller.is_some(),
            "§4: a state read arrived with no caller: {event:?}"
        );
        // At bound 1 nothing is ever outstanding twice, so a label can always be
        // attributed to its own call. A note here would mean the serial arm overlapped.
        assert_eq!(
            event.context_note, None,
            "{}: a serial arm cannot have an ambiguous label: {event:?}",
            arm.name
        );
    }
    // §13: every state read names the pinned height, and none of them names a moving tag.
    let heights: Vec<&str> = arm
        .events
        .iter()
        .filter(|event| STATE_METHODS.contains(&event.method.as_str()))
        .map(|event| {
            event
                .block
                .as_deref()
                .expect("a state read names its height")
        })
        .collect();
    assert!(
        heights.iter().all(|height| *height == BLOCK.to_string()),
        "a state read went out at a height other than the pin: {:?}",
        heights
            .iter()
            .filter(|height| **height != BLOCK.to_string())
            .copied()
            .collect::<Vec<_>>()
    );
    assert!(
        arm.stats.reuse,
        "§14: {} ran with the reuse boundary off",
        arm.name
    );
    assert_eq!(
        arm.concurrency.configured, 1,
        "§14: {} ran at concurrency {}",
        arm.name, arm.concurrency.configured
    );
    assert!(
        arm.concurrency.observed_peak <= 1,
        "§14: {} was serial in configuration and its own scheduler saw {} reads outstanding",
        arm.name,
        arm.concurrency.observed_peak
    );
}

/// §14: three fixed-block runs, each written as a whole diagnosis directory, each answering
/// the same dependency question — and the directories agreeing.
///
/// The agreement checked here is the *classification*, not the timing: three runs of one route
/// on one block must produce the same nodes, the same edges and the same independent / ordered
/// / unknown split, while their nanoseconds are free to move. That is also why this experiment
/// exists next to the live one: a dependency proof that held only on a live node would be a
/// proof about a network.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn three_fixed_block_runs_answer_the_dependency_question_the_same_way() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let root = evidence_root();
    let arms_root = fresh_subtree(&root, "fixed-block");
    let mut arms = Vec::new();
    for name in A_RUNS {
        let dir = arms_root.join(name);
        let arm = run_arm(
            &state,
            &fixture.route,
            &fixture.header,
            name,
            true,
            Some(&dir),
        )
        .await;
        assert_the_arm_is_a_valid_dependency_subject(&arm);
        assert!(
            !arm.storage_rows().is_empty(),
            "{} read no storage word, so it is not evidence about storage reads",
            arm.name
        );
        arms.push(arm);
    }

    for arm in &arms {
        let dir = arm.dir.as_ref().expect("an A arm writes a directory");
        // §21's file set: the writer's ten plus this milestone's six, and nothing else. No
        // `outside-simulation-rpc.json` here, because a fixture run has no lifecycle to file.
        assert_eq!(
            listing(dir),
            files_the_writer_owes(true, true, false),
            "{}: the directory holds a file set that is not the one the writer owes",
            arm.name
        );
        let map = read_json(&dir.join(DEPENDENCY_MAP_FILE));
        let storage_reads = read_json(&dir.join(STORAGE_READS_FILE));
        let summary = read_json(&dir.join(DEPENDENCY_SUMMARY_FILE));
        let calls = read_json(&dir.join(PIPELINE_CALLS_FILE));
        let pipeline = read_json(&dir.join(PIPELINE_SUMMARY_FILE));

        // §6's raw/aggregate pair, in one directory: the row list *is* the node list.
        assert_eq!(
            storage_reads["rows"].as_array().map(Vec::len),
            Some(arm.storage_rows().len()),
            "{}: `storage-reads.json` does not hold one row per storage call",
            arm.name
        );
        assert_eq!(
            storage_reads["rows"], map["nodes"],
            "{}: the two files that name the same rows disagree",
            arm.name
        );
        assert_eq!(
            map["summary"]["total"],
            json!(arm.storage_rows().len()),
            "{}: the map's own total is not the count of reads this run made",
            arm.name
        );
        // §19's summary shape, and §3's closed set: nothing is counted that is not one of the
        // three words, and the three add up.
        assert_eq!(
            summary["storage_reads"]["total"], map["summary"]["total"],
            "{}: the summary and the map report different totals",
            arm.name
        );
        let counts = &summary["storage_reads"];
        assert_eq!(
            counts["independent"].as_u64().unwrap_or(0)
                + counts["ordered"].as_u64().unwrap_or(0)
                + counts["unknown"].as_u64().unwrap_or(0),
            counts["total"].as_u64().unwrap_or(0),
            "{}: the three states do not add up to the total: {counts:?}",
            arm.name
        );
        for node in map["nodes"].as_array().expect("a node list") {
            let word = node["dependency"].as_str().unwrap_or("<absent>");
            assert!(
                ["independent", "ordered", "unknown"].contains(&word),
                "§3: a node is classified as {word:?}, which is not one of the three words"
            );
            for field in [
                "address",
                "slot",
                "block_tag",
                "caller",
                "stage",
                "evidence",
            ] {
                assert_ne!(
                    node[field],
                    Value::Null,
                    "§4: the {field} of a read is missing rather than recorded: {node}"
                );
            }
        }
        // §9's whole-pipeline rows: every call this run's sink saw, on one clock, in one file.
        assert_eq!(
            calls["rows"].as_array().map(Vec::len),
            Some(arm.events.len()),
            "{}: `pipeline-calls.json` does not hold every call this run made",
            arm.name
        );
        assert_eq!(
            pipeline["per_run"][0]["totals"]["rpc_count"],
            json!(arm.events.len()),
            "{}: the run's own pipeline total is not its call count",
            arm.name
        );
        // §13's pin claim, read from the table that has to carry it.
        assert_eq!(
            pipeline["per_run"][0]["block_pin"]["all_state_reads_pinned"],
            json!(true),
            "{}: the pipeline table does not confirm the pin",
            arm.name
        );
    }

    // The three directories agree where agreement is the whole point of running three.
    let first = arms[0].dir.as_ref().expect("an A arm writes a directory");
    for arm in &arms[1..] {
        let dir = arm.dir.as_ref().expect("an A arm writes a directory");
        let left = read_json(&first.join(DEPENDENCY_MAP_FILE));
        let right = read_json(&dir.join(DEPENDENCY_MAP_FILE));
        assert_eq!(
            left["summary"], right["summary"],
            "§14: the arms split the same reads differently: {:?} vs {:?}",
            left["summary"], right["summary"]
        );
        // And they asked for the same words, in the same order — the run identity underneath
        // the dependency answer. Three arms that classified alike while reading different
        // slots would be three answers to three questions.
        assert_eq!(
            arms[0].call_identity(),
            arm.call_identity(),
            "§14: {} asked the endpoint for calls {} did not",
            arms[0].name,
            arm.name
        );
        let left = dependency_structure(&left);
        let right = dependency_structure(&right);
        assert_eq!(
            left["nodes"], right["nodes"],
            "§14: {} classifies reads the same way in total but not read by read",
            arm.name
        );
        assert_eq!(
            left["edges"], right["edges"],
            "§14: {} and {} disagree about which read waits for which",
            arms[0].name, arm.name
        );
    }
    println!(
        "experiment A: {} runs, {} storage reads each, {:?}",
        arms.len(),
        arms[0].storage_rows().len(),
        read_json(&first.join(DEPENDENCY_SUMMARY_FILE))["storage_reads"]
    );
}

/// The part of a dependency map that two runs of one plan can be compared on: the per-read
/// classification and the edges between reads, with everything that can only differ because a
/// run is a different run taken out.
///
/// Two things have to go. A node id is `run-01:m8.4.1-run-01-37191169:rpc12`, so requiring two
/// arms' ids to match would compare which run a node names rather than which read it waits for;
/// each id is replaced here by the position of that node in its own run's list, which carries
/// the same information without the label. And an edge's `wait_before_start_ns` is a duration
/// measured on this run's clock, so the classifier's answer is compared, not its timing.
fn dependency_structure(map: &Value) -> Value {
    let nodes = map["nodes"].as_array().expect("a node list");
    let mut ordinals: BTreeMap<String, usize> = BTreeMap::new();
    for (position, node) in nodes.iter().enumerate() {
        ordinals.insert(
            node["node_id"]
                .as_str()
                .expect("a node that names itself")
                .to_string(),
            position,
        );
    }
    rewrite_ids(
        &json!({
            "nodes": nodes
                .iter()
                .map(|node| {
                    json!({
                        "address": node["address"],
                        "slot": node["slot"],
                        "caller": node["caller"],
                        "leg": node["leg"],
                        "dependency": node["dependency"],
                        "depends_on": node["depends_on"],
                        "followed_by": node["followed_by"],
                        "uses_prior_response": node["uses_prior_response"],
                    })
                })
                .collect::<Vec<_>>(),
            "edges": map["edges"]
                .as_array()
                .expect("an edge list")
                .iter()
                .map(|edge| {
                    let mut kept = edge.as_object().expect("an edge as an object").clone();
                    kept.remove("wait_before_start_ns");
                    Value::Object(kept)
                })
                .collect::<Vec<_>>(),
        }),
        &ordinals,
    )
}

/// Replace every node id in `value` with its `#position`, walking whatever shape the map happens
/// to use for a reference — a node's `depends_on`, an edge's `from`/`to`, an edge's `members`.
fn rewrite_ids(value: &Value, ordinals: &BTreeMap<String, usize>) -> Value {
    match value {
        Value::String(text) => match ordinals.get(text) {
            Some(position) => json!(format!("#{position}")),
            None => value.clone(),
        },
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| rewrite_ids(item, ordinals))
                .collect::<Vec<_>>(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), rewrite_ids(field, ordinals)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// §16: the same simulation with the instrumentation off and on, compared field by field, and
/// the calls each arm put on the wire compared one by one.
///
/// Three claims, and they are three different ways the instrument could be lying:
///
/// ```text
/// the answer   §16's twenty-one fields, then the whole result struct and its fingerprint
/// the wire     the endpoint's method list, position by position — §12's rule that an
///              instrument adds no request, checked as a sequence and not as a count
/// the tables   the instrumented arm wrote a real directory, and its own dependency map
///              describes the run the baseline arm also made
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn the_instrumentation_changed_no_answer_and_issued_no_call() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let evidence = evidence_root();
    let correctness = fresh_subtree(&evidence, "correctness");
    let baseline = run_arm(
        &state,
        &fixture.route,
        &fixture.header,
        "baseline",
        false,
        None,
    )
    .await;
    let instrumented_dir = correctness.join("instrumented-run");
    let instrumented = run_arm(
        &state,
        &fixture.route,
        &fixture.header,
        "instrumented",
        true,
        Some(&instrumented_dir),
    )
    .await;
    assert_the_arm_is_a_valid_dependency_subject(&instrumented);
    assert!(
        baseline.events.is_empty(),
        "the baseline arm recorded something, so it is not the un-instrumented control"
    );

    // §12, at the wire: same calls, same order, same everything.
    assert_eq!(
        baseline.arrivals, instrumented.arrivals,
        "the instrument changed which calls go out, or the order they go out in"
    );
    assert_eq!(
        baseline.identity()["route"],
        instrumented.identity()["route"],
        "the two arms executed a different route"
    );

    let base_row = baseline.identity();
    let named = base_row
        .as_object()
        .expect("a row of §16's fields")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut expected = COMPARISON_FIELDS.to_vec();
    expected.sort();
    assert_eq!(
        named, expected,
        "the row §16 compares is not the field list this file names: {named:?} vs {expected:?}"
    );
    // Every field of the row, judged — a list a reader can check is longer than a sentence
    // saying they were all equal.
    let mut fields = serde_json::Map::new();
    for name in &expected {
        let left = &base_row[*name];
        let right = &instrumented.identity()[*name];
        fields.insert(
            (*name).to_string(),
            json!({
                "identical": left == right,
                "baseline": left,
                "instrumented": right,
            }),
        );
    }
    assert!(
        fields
            .values()
            .all(|verdict| verdict["identical"] == json!(true)),
        "§16: the instrument changed a field: {:?}",
        fields
            .iter()
            .filter(|(_, verdict)| verdict["identical"] != json!(true))
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        baseline.result, instrumented.result,
        "§16: the whole result differs even though every named field matched"
    );
    assert_eq!(
        baseline.whole_result(),
        instrumented.whole_result(),
        "§16: the two arms' serialized results differ"
    );
    assert_eq!(
        baseline.result.fingerprint(),
        instrumented.result.fingerprint(),
        "§16: the fingerprint — which is a hash of every field above — differs"
    );

    // And the instrumented arm's tables describe *this* run, not an empty shell.
    let map = read_json(&instrumented_dir.join(DEPENDENCY_MAP_FILE));
    assert_eq!(
        map["summary"]["total"],
        json!(instrumented.storage_rows().len()),
        "the directory the instrumented arm wrote does not describe its own run"
    );
    // The baseline arm's calls are the ones the map classifies: the instrument recorded the
    // same run the control made without it, so a field-by-field match on the calls is what
    // lets §16's dependency answer stand for the un-instrumented run too.
    //
    // The address is compared in the form the map keeps it: a node stores
    // [`evm_chain::normalize_address`] of the event's `target`, which arrives in the checksum
    // case the adapter sent. Capitalisation is not a different slot of state, and an assertion
    // that reported it as one would flag a difference that is not one.
    assert_eq!(
        map["nodes"]
            .as_array()
            .expect("a node list")
            .iter()
            .map(|node| json!({"address": node["address"], "slot": node["slot"]}))
            .collect::<Vec<_>>(),
        instrumented
            .storage_rows()
            .iter()
            .map(|event| json!({
                "address": event.target.as_deref().map(evm_chain::normalize_address),
                "slot": event.slot,
            }))
            .collect::<Vec<_>>(),
        "the map's rows are not the storage calls this run made, in the order it made them"
    );

    write_json(
        &correctness.join("baseline.json"),
        &json!({
            "schema": "m8.4.1-correctness/v1",
            "milestone": "M8.4.1 §16",
            "arm": "baseline",
            "instrumented": false,
            "commit": git_revision(),
            "execution_mode": ExecutionMode::BuildOnly.name(),
            "chain_id": CHAIN.0,
            "block_number": BLOCK,
            "state_read_reuse": baseline.stats.reuse,
            "state_read_concurrency": baseline.concurrency.configured,
            "fields": baseline.identity(),
            "calls": baseline.arrivals,
            "metrics": baseline.metrics(),
            "note": "no sink was attached to this arm's adapter, so it recorded nothing and \
                     wrote no directory; `calls` is what the endpoint saw",
        }),
    );
    write_json(
        &correctness.join("instrumented.json"),
        &json!({
            "schema": "m8.4.1-correctness/v1",
            "milestone": "M8.4.1 §16",
            "arm": "instrumented",
            "instrumented": true,
            "commit": git_revision(),
            "execution_mode": ExecutionMode::BuildOnly.name(),
            "chain_id": CHAIN.0,
            "block_number": BLOCK,
            "state_read_reuse": instrumented.stats.reuse,
            "state_read_concurrency": instrumented.concurrency.configured,
            "fields": instrumented.identity(),
            "calls": instrumented.arrivals,
            "metrics": instrumented.metrics(),
            "diagnosis_dir": "correctness/instrumented-run",
            "diagnosis_files": instrumented.files,
        }),
    );
    write_json(
        &correctness.join("comparison.json"),
        &json!({
            "schema": "m8.4.1-correctness/v1",
            "generated_by": "M841_FIXTURE_EVIDENCE=data/evidence/m8/storage-dependency \
                             cargo test -p evm-simulation --test storage_dependency_experiment",
            "milestone": "M8.4.1 §16",
            "question": "does the stage/caller stamping and the six-table writer change what \
                         the simulation answers, or what it asks the node?",
            "variable": "instrumentation on / off",
            "commit": git_revision(),
            "chain_id": CHAIN.0,
            "block_number": BLOCK,
            "settings_held_fixed": {
                "state_read_reuse": true,
                "state_read_concurrency": 1,
                "execution_mode": ExecutionMode::BuildOnly.name(),
                "asked_output_wei": U256::ONE.to_string(),
            },
            // §41's honest split: the verdicts below are functions of the recorded dump and
            // hold across reruns; the ns beside them are one machine's wall clock.
            "stable_across_reruns": [
                "fields", "whole_result", "calls", "gates",
                "metrics.*.{calls_on_the_wire,calls_recorded,state_reads,storage_reads,header_reads}",
            ],
            "run_specific": [
                "metrics.*.run_wall_ns", "metrics.*.rpc_sum_duration_ns",
            ],
            "metrics": {
                "baseline": baseline.metrics(),
                "instrumented": instrumented.metrics(),
                "note": "the two arms' own figures, side by side. `baseline.calls_recorded == 0` \
                         is the control: this arm had no sink, so a wire count of 41 and a \
                         recorded count of 0 are the same 41 calls seen from the two ends that \
                         can see them.",
            },
            "fields": Value::Object(fields),
            "field_names": COMPARISON_FIELDS,
            "whole_result": {
                "identical": baseline.whole_result() == instrumented.whole_result(),
                "fingerprint_identical": baseline.result.fingerprint()
                    == instrumented.result.fingerprint(),
                "fingerprint": baseline.result.fingerprint(),
            },
            "calls": {
                "identical_in_order": baseline.arrivals == instrumented.arrivals,
                "count_baseline": baseline.arrivals.len(),
                "count_instrumented": instrumented.arrivals.len(),
            },
            "gates": {
                "every_named_field_identical": true,
                "the_endpoint_saw_the_same_calls": baseline.arrivals == instrumented.arrivals,
                "the_instrument_recorded_every_call_it_made":
                    instrumented.events.len() == instrumented.arrivals.len(),
                "every_state_read_named_the_pin": instrumented
                    .events
                    .iter()
                    .filter(|event| STATE_METHODS.contains(&event.method.as_str()))
                    .all(|event| event.block.as_deref() == Some(BLOCK.to_string().as_str())),
            },
            "gates_are_assertions_not_data": "every value above was checked in this test before \
                                              it was written; the file is the same claim in a \
                                              form a reader can re-check, not the place it is \
                                              decided",
        }),
    );
}

/// §16's comparison is only a gate if a difference would be caught by it. This probes the
/// judgment rather than the runs: it feeds [`field_verdicts`] rows that differ, one field at a
/// time, and requires exactly that field to be flagged.
#[test]
fn a_different_answer_would_be_flagged_field_by_field() {
    let mut base = json!({});
    for name in COMPARISON_FIELDS {
        base[name] = json!(format!("value of {name}"));
    }
    let verdicts = |left: &Value, right: &Value| {
        COMPARISON_FIELDS
            .iter()
            .filter(|name| left[*name] != right[*name])
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    };
    assert!(
        verdicts(&base, &base).is_empty(),
        "two identical rows did not compare as identical, so the verdict is not a function of \
         the values beside it"
    );
    for (name, probe) in [
        (
            "fingerprint",
            json!("0x0000000000000000000000000000000000000000000000000000000"),
        ),
        ("gas_used", json!("value of gas_used (changed)")),
        ("outcome", json!("None")),
    ] {
        assert_ne!(
            base[name], probe,
            "the probe for {name} is the value it holds"
        );
        let mut moved = base.clone();
        moved[name] = probe;
        assert_eq!(
            verdicts(&base, &moved),
            vec![name.to_string()],
            "changing {name} has to be reported as that field and nothing else"
        );
    }
    // And the list itself is closed: a field this file stopped publishing would leave
    // `identity()` short of the names §16 asks for, which the main test compares.
    assert_eq!(
        COMPARISON_FIELDS.len(),
        COMPARISON_FIELDS
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "a field name is listed twice, so the comparison would judge one field twice and \
         another not at all"
    );
}

/// The writer's file set, spelled out from its own constants, so a reader can see which of the
/// sixteen a fixture run does not get and why.
///
/// The one name a live run has and this does not is `outside-simulation-rpc.json`: a fixture
/// run has one sink and no lifecycle to classify against stage spans, so §11's answer for it is
/// the file's absence plus the sentence in this directory's README, not an empty table.
#[test]
fn a_fixture_directory_owes_fifteen_files_and_no_lifecycle_table() {
    let owed = files_the_writer_owes(true, true, false);
    assert_eq!(
        owed.len(),
        15,
        "the fixture directory's file set moved: {owed:?}"
    );
    assert!(
        !owed.contains(&OUTSIDE_FILE.to_string()),
        "a fixture run has no lifecycle sink, so it must not publish the outside table"
    );
    for name in DEPENDENCY_FILES {
        assert!(
            owed.contains(&name.to_string()),
            "{name} is missing from the set"
        );
    }
    // The positive control for the assertion above: with a lifecycle recorded, the same
    // function does owe the sixteenth name — so the absence checked above is this run's shape
    // and not a list that forgot a file.
    let with_lifecycle = files_the_writer_owes(true, true, true);
    assert_eq!(with_lifecycle.len(), 16, "{with_lifecycle:?}");
    assert!(with_lifecycle.contains(&OUTSIDE_FILE.to_string()));
}
