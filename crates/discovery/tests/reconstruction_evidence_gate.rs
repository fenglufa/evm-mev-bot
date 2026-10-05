//! M9.2 §23/§24/§27/§32 — the committed evidence directory, assembled from the raw
//! records and gated against them.
//!
//! # What this file does, in order
//!
//! It reads the raw documents `tests/sync_reconstruction_live.rs` wrote (no network, no
//! clock), rebuilds the whole M9.2 pipeline from them with the production functions —
//! hydrate a `Reconstruction` out of the saved rows, run `integrate_at_target` on the
//! 80 pools M9.1 verified — and writes the committed tables from what that rebuild
//! produced. In default mode it writes nothing and instead compares every committed byte
//! against a fresh rebuild. So a committed number is either the arithmetic of the raw
//! records or this test fails.
//!
//! ```text
//! M92_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test reconstruction_evidence_gate \
//!     -- --test-threads=1
//! cargo test -p evm-discovery --test reconstruction_evidence_gate -- --test-threads=1
//! ```
//!
//! # Why the rebuild is the interesting part
//!
//! `Reconstruction` deliberately does not derive `Deserialize`: its two counters
//! (`chunks_scanned`, `logs_returned`) belong to a run rather than to a pool, so a
//! struct-level derive would have let a row claim a request count it did not pay. This
//! gate therefore rebuilds it field by field from the document's own recorded counters,
//! and then asks the production pipeline to reproduce the graph. If the committed rows
//! could not regenerate the committed graph, the directory would be a transcript of a run
//! rather than evidence for one — which is the difference §32 asks for.
//!
//! # Why the recomputation appears twice, in two styles
//!
//! [`Rebuilt::load`] calls the crate. The tests under *independent recompute* below call
//! no discovery function at all: they read the same raw JSON as `serde_json::Value` and
//! derive every class from the row fields — `selected`, `search_from`, `search_to`,
//! `target`, the two reserves — and compare against the committed counts. That half is
//! the independent witness, and one more test injects a wrong number into a copy of the
//! raw records to prove the comparison actually bites.
//!
//! # What is never written
//!
//! The endpoint. Every committed file names it as a digest, and a test asserts the URL
//! itself appears in none of them.

#![recursion_limit = "2048"]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use evm_chain::MAX_EVENTS_PER_SINK;
use evm_core::{BlockNumber, ChainId, PoolId};
use evm_discovery::{
    integrate_at_target, DiscoveredState, GraphOutcome, PoolStateAtTarget, PoolSyncAtTarget,
    Reconstruction, VerifiedPool, CHUNK_BLOCKS, NODE_LOG_LIMIT,
};
use evm_graph::GraphEdge;
use evm_protocol::Registry;
use evm_state::UpdatePosition;

const EVIDENCE_REL: &str = "data/evidence/m9/m9.2";
/// The input set is M9.1's own verified pools, read out of its committed raw pass. §20
/// asks for the same 80, so the denominator here is M9.1's number rather than a new one.
const M91_PASS_REL: &str = "data/evidence/m9/m9.1/raw/pass-a.json";
/// The same milestone's published integration table, cross-checked against its own raw
/// pass before either is quoted as this run's baseline.
const M91_INTEGRATION_REL: &str = "data/evidence/m9/m9.1/state-graph-integration.json";

const PASS_A: &str = "raw/reconstruction-pass-a.json";
const PASS_B: &str = "raw/reconstruction-pass-b.json";
const CALLS_A: &str = "raw/rpc-calls-pass-a.jsonl";
const CALLS_B: &str = "raw/rpc-calls-pass-b.jsonl";
/// Written only by a chain-wide pass, which is the one that sees other pools' logs.
const SYNC_LOGS_A: &str = "raw/sync-logs-pass-a.jsonl";
const SYNC_LOGS_B: &str = "raw/sync-logs-pass-b.jsonl";
const STRATEGY_POOL: &str = "raw/strategy-a-pool.json";
const STRATEGY_CENSUS: &str = "raw/strategy-b-census.json";
const STRATEGY_POOL_CALLS: &str = "raw/strategy-a-rpc.jsonl";
const STRATEGY_CENSUS_CALLS: &str = "raw/strategy-b-rpc.jsonl";
const STRATEGY_CENSUS_CHUNKS: &str = "raw/strategy-b-census-chunks.json";
const PROBE: &str = "raw/probe-node-capacity.json";
const PROBE_CALLS: &str = "raw/probe-node-capacity-rpc.jsonl";
/// Every trace this directory commits. Both the read-only count in the tables and the test
/// that checks it walk this one list, so a method cannot hide in a file no table names.
const TRACES: [&str; 5] = [
    CALLS_A,
    CALLS_B,
    STRATEGY_POOL_CALLS,
    STRATEGY_CENSUS_CALLS,
    PROBE_CALLS,
];

const SUMMARY: &str = "summary.json";
const RECONSTRUCTION: &str = "reconstruction.json";
const SYNC_EVENTS: &str = "sync-events.json";
const INTEGRATION: &str = "graph-integration.json";
const RPC_TABLE: &str = "rpc-calls.json";
const UNAVAILABLE_TABLE: &str = "rejected-pools.json";
const STRATEGY_TABLE: &str = "strategy-comparison.json";
const CAPACITY_TABLE: &str = "node-capacity.json";
const MANIFEST: &str = "manifest.json";
const README: &str = "README.md";
const TABLES: [&str; 10] = [
    SUMMARY,
    RECONSTRUCTION,
    SYNC_EVENTS,
    INTEGRATION,
    RPC_TABLE,
    UNAVAILABLE_TABLE,
    STRATEGY_TABLE,
    CAPACITY_TABLE,
    MANIFEST,
    README,
];

/// The methods a reconstruction is allowed to put on the wire. A `getReserves()` here
/// would mean market state came from a contract's own claim instead of from a `Sync`
/// the chain recorded (§5), so this is a correctness list, not a cost list.
const READ_ONLY_METHODS: [&str; 2] = ["eth_chainId", "eth_getLogs"];

/// The §19 controls, named where a reader can run them. This gate does not execute them
/// — they are ordinary tests in two other targets, and the workspace run is what proves
/// they pass; what is recorded here is which test carries which control.
const NEGATIVE_CONTROLS: [(&str, &str, &str); 7] = [
    (
        "NC1",
        "a Sync after the target is not selected",
        "sync_reconstruction::a_sync_after_the_target_is_neither_selected_nor_read, \
         sync_reconstruction::a_sync_after_the_target_keeps_the_pool_out_of_the_graph_through_the_door",
    ),
    (
        "NC2",
        "no Sync at or before the target stays unavailable",
        "sync_reconstruction::a_pool_that_published_nothing_is_scanned_to_its_creation_not_to_genesis, \
         sync_reconstruction::a_run_counts_its_own_work_and_names_which_pools_it_could_not_price",
    ),
    (
        "NC3",
        "an older Sync with a covered gap is valid at the target",
        "sync_reconstruction::an_older_sync_with_a_covered_gap_prices_the_target, \
         sync_reconstruction::a_reconstructed_pool_reaches_the_graph_through_the_real_door",
    ),
    (
        "NC4",
        "the last Sync of a block is the one selected",
        "sync_reconstruction::the_last_sync_of_a_block_is_the_one_selected, \
         graph::state_at_target::the_final_sync_of_a_block_is_the_one_that_prices_it",
    ),
    (
        "NC5",
        "a future Sync cannot leak into past state",
        "sync_reconstruction::no_request_ever_names_a_block_above_the_target, \
         graph::state_at_target::a_sync_after_the_target_never_prices_it",
    ),
    (
        "NC6",
        "a pool whose state is not proven at the target stays out of the graph",
        "sync_reconstruction::a_reconstructed_pool_joins_the_target_graph_and_an_unscanned_one_does_not, \
         graph::state_at_target::a_target_graph_never_mixes_unproven_moments",
    ),
    (
        "NC7",
        "getReserves() never grants graph eligibility",
        "sync_reconstruction::coverage_alone_never_grants_graph_eligibility, \
         graph::state_at_target::coverage_alone_cannot_invent_a_price",
    ),
];

/// Where a control's test actually lives. A control that names a test nobody wrote is a
/// claim, not a control, so the table is checked against the source files.
fn test_source_file(module: &str) -> &'static str {
    match module {
        "sync_reconstruction" => "crates/discovery/tests/sync_reconstruction.rs",
        "graph::state_at_target" => "crates/graph/tests/state_at_target.rs",
        other => panic!("{other} is not a test module this gate knows about"),
    }
}

/// §31's thirteen topics. The task book asks for a focused test for each; this table is the
/// answer, and `every_test_matrix_topic_names_a_test_that_runs` reads the names out of the
/// source files so a topic cannot be marked covered by a test that was renamed away.
const TEST_MATRIX: [(&str, &str); 13] = [
    (
        "target block selection",
        "sync_reconstruction::an_older_sync_with_a_covered_gap_prices_the_target, \
         graph::state_at_target::a_proven_unchanged_older_sync_prices_the_target_block",
    ),
    (
        "latest Sync <= target",
        "sync_reconstruction::the_last_sync_of_a_block_is_the_one_selected, \
         sync_reconstruction::a_sync_exactly_on_a_chunk_edge_is_found",
    ),
    (
        "Sync > target rejection",
        "sync_reconstruction::a_sync_after_the_target_is_neither_selected_nor_read, \
         sync_reconstruction::a_sync_after_the_target_keeps_the_pool_out_of_the_graph_through_the_door",
    ),
    (
        "no Sync",
        "sync_reconstruction::a_pool_that_published_nothing_is_scanned_to_its_creation_not_to_genesis, \
         graph::state_at_target::an_unscanned_gap_keeps_the_pool_out_of_the_target_graph",
    ),
    (
        "multiple Sync same block",
        "sync_reconstruction::the_last_sync_of_a_block_is_the_one_selected, \
         graph::state_at_target::the_final_sync_of_a_block_is_the_one_that_prices_it",
    ),
    (
        "same-pool ordering",
        "sync_reconstruction::a_node_that_answers_in_reverse_order_gives_the_same_selection, \
         sync_reconstruction::a_scan_never_becomes_another_pools_statement",
    ),
    (
        "historical block pinning",
        "sync_reconstruction::no_request_ever_names_a_block_above_the_target, \
         sync_reconstruction::both_strategies_ask_the_sync_topic_and_only_that_one",
    ),
    (
        "future-state isolation",
        "graph::state_at_target::a_sync_after_the_target_never_prices_it, \
         graph::state_at_target::a_later_sync_does_not_move_an_earlier_graph",
    ),
    (
        "state-at-target projection",
        "sync_reconstruction::a_reconstruction_projects_a_snapshot_without_moving_its_observations, \
         graph::state_at_target::without_a_named_target_only_the_applied_block_prices_the_graph",
    ),
    (
        "Graph inclusion",
        "sync_reconstruction::a_reconstructed_pool_reaches_the_graph_through_the_real_door, \
         sync_reconstruction::a_reconstructed_pool_joins_the_target_graph_and_an_unscanned_one_does_not",
    ),
    (
        "Graph rejection",
        "sync_reconstruction::a_pool_the_reconstruction_did_not_cover_is_registered_and_skipped, \
         graph::state_at_target::a_target_graph_never_mixes_unproven_moments",
    ),
    (
        "getReserves fallback rejection",
        "sync_reconstruction::coverage_alone_never_grants_graph_eligibility, \
         graph::state_at_target::coverage_alone_cannot_invent_a_price",
    ),
    (
        "determinism",
        "sync_reconstruction::the_same_inputs_produce_the_same_rows_in_the_same_order, \
         sync_reconstruction::two_door_calls_on_the_same_inputs_agree_entirely, \
         graph::state_at_target::the_same_inputs_build_the_same_graph_and_the_same_accounting",
    ),
];

fn refresh() -> bool {
    std::env::var("M92_EVIDENCE_REFRESH").is_ok_and(|value| value == "1")
}

/// In an assembly run, writing the directory is not a step a reader has to remember to run
/// first: the first committed table anyone asks for is assembled from the raw records before
/// its bytes are read. Without this, `cargo test`'s alphabetical order decides which tables
/// exist when a test reads one, and the failures such a run reports describe the order of the
/// tests rather than the evidence. A validation run (no `M92_EVIDENCE_REFRESH`) writes nothing
/// here and compares, which is what §23's "every aggregate recomputable from raw evidence"
/// asks the committed bytes to survive.
static ASSEMBLED: std::sync::Once = std::sync::Once::new();

fn assemble_once() {
    ASSEMBLED.call_once(|| {
        if refresh() {
            let case = Case::load();
            let tables = case.tables();
            case.commit(&tables);
        }
    });
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_REL)
}

/// The raw records this directory is expected to carry, in one fixed order, kept to the
/// ones that exist: only a chain-wide pass writes `sync-logs-pass-*`, and an arm of the §12
/// experiment that was never run has no document. Both `manifest.json#raw_files` and every
/// table's `_provenance.raw_records` come from this list, so neither can name a file the
/// other does not know about.
fn raw_record_names() -> Vec<&'static str> {
    [
        PASS_A,
        PASS_B,
        CALLS_A,
        CALLS_B,
        SYNC_LOGS_A,
        SYNC_LOGS_B,
        STRATEGY_POOL,
        STRATEGY_POOL_CALLS,
        STRATEGY_CENSUS,
        STRATEGY_CENSUS_CALLS,
        STRATEGY_CENSUS_CHUNKS,
        PROBE,
        PROBE_CALLS,
    ]
    .into_iter()
    .filter(|name| Path::new(&evidence_dir().join(name)).exists())
    .collect()
}

fn missing(name: &str) -> String {
    format!(
        "{EVIDENCE_REL}/{name} is not readable. Run the live targets first (§12, then §20), \
         then assemble: M92_EVIDENCE_REFRESH=1 cargo test -p evm-discovery \
         --test reconstruction_evidence_gate -- --test-threads=1"
    )
}

fn read_text(relative: &str) -> String {
    if TABLES.contains(&relative) {
        assemble_once();
    }
    let path = if relative.starts_with("data/") {
        workspace_root().join(relative)
    } else {
        evidence_dir().join(relative)
    };
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// A committed table's bytes, read through the one path that assembles the directory first
/// when this run is an assembly run. A test that reached for `std::fs` directly would read
/// whatever table order happened to leave behind.
fn committed_table(name: &str) -> String {
    assert!(
        TABLES.contains(&name),
        "{name} is not one of the §23 table names"
    );
    read_text(name)
}

fn read_json<T>(relative: &str) -> T
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(&read_text(relative))
        .unwrap_or_else(|err| panic!("{} does not parse: {err}", relative))
}

/// A file from this workspace's own source, for the handful of claims that can only be
/// proved about the code rather than about a run — which block arguments a request type can
/// even carry, for instance.
fn source_text(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Every `.rs` file under a directory, in path order, so a source scan has a list it can
/// report the length of. Order matters only in that a fixed list makes a re-run comparable.
fn rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![directory.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
        {
            let path = entry
                .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn read_jsonl(relative: &str) -> Vec<Value> {
    read_text(relative)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|err| panic!("{} has a line that does not parse: {err}", relative))
        })
        .collect()
}

/// The endpoint as this directory is allowed to name it: the same digest rule the chain
/// crate uses (`crates/chain/src/rpc_trace.rs:933`), copied here so the gate can be read
/// next to the rule it applies.
fn digest_of(url: &str) -> String {
    format!(
        "rpc-{}",
        &alloy_primitives::keccak256(url.as_bytes()).to_string()[2..18]
    )
}

/// The endpoint M9.1 recorded, read from its own committed table rather than restated.
fn endpoint_digest() -> String {
    let census: Value = read_json("data/evidence/m7/census-pair-created.json");
    let url = census["_provenance"]["rpc_endpoint"]
        .as_str()
        .expect("M7's census names its endpoint")
        .to_string();
    digest_of(&url)
}

/// Every call every trace in this directory recorded, with the file it came from. A count
/// of "read-only" that walked only the files a table happens to name would leave the
/// probe and the experiment uncounted, so both the tables and the gate use this one list.
fn traced_calls() -> Vec<(&'static str, Value)> {
    let mut calls = Vec::new();
    for relative in TRACES {
        if !Path::new(&evidence_dir().join(relative)).exists() {
            continue;
        }
        calls.extend(
            read_jsonl(relative)
                .into_iter()
                .map(|line| (relative, line)),
        );
    }
    calls
}

/// One run's calls, counted from its own trace file and reconciled against the request
/// count in its document. Four numbers have to close here:
///
/// ```text
/// lines in the file                       == events_recorded   (the document's own count)
/// events_recorded + events_dropped        == requests + chain-id lines
/// events_recorded + events_dropped        <= MAX_EVENTS_PER_SINK + events_dropped
/// cap the document names                  == MAX_EVENTS_PER_SINK
/// ```
///
/// The middle line is the one that matters: a sink holds [`MAX_EVENTS_PER_SINK`] events and
/// counts the rest, so a trace file can be shorter than the calls there were, and only the
/// run's own dropped-counter says by how much. `requests` comes from the scan's arithmetic,
/// never from the trace, so the equality compares two independent records of one run — a
/// call the scan does not explain (a signature, a broadcast, a second node read) makes the
/// left side larger and fails here as well as in `sync_reconstruction_live::audit_trace`.
fn call_accounting(trace_relative: &str, requests: u64, recorded: &TraceAccountingDoc) -> Value {
    let lines = read_jsonl(trace_relative);
    let stamps = lines
        .iter()
        .filter(|line| line["method"].as_str() == Some("eth_chainId"))
        .count() as u64;
    let held = lines.len() as u64;
    let cap = MAX_EVENTS_PER_SINK as u64;
    assert_eq!(
        recorded.max_events_per_sink as u64, cap,
        "{trace_relative}: the run names a cap of {}, and this build caps a sink at {cap}",
        recorded.max_events_per_sink
    );
    assert_eq!(
        recorded.events_recorded as u64, held,
        "{trace_relative}: the run says it recorded {} events and the file has {held} lines",
        recorded.events_recorded
    );
    assert_eq!(
        recorded.eth_getlogs_scans, requests,
        "{trace_relative}: the accounting sums {} `eth_getLogs` scans while the same \
         document records {requests} requests — the two halves of one run's cost disagree",
        recorded.eth_getlogs_scans
    );
    let accounted = requests + stamps;
    assert_eq!(
        held + recorded.events_dropped,
        accounted,
        "{trace_relative}: {held} recorded lines plus {} counted beyond the cap are {} \
         calls against the {accounted} the scan accounts for ({requests} requests plus \
         {stamps} chain-id reads)",
        recorded.events_dropped,
        held + recorded.events_dropped,
    );
    json!({
        "trace_file": trace_relative,
        "lines_in_the_trace_file": held,
        "eth_chainId_lines": stamps,
        "eth_getlogs_requests": requests,
        "calls_the_scan_accounts_for": accounted,
        "events_recorded": recorded.events_recorded,
        "events_dropped_beyond_the_cap": recorded.events_dropped,
        "cap_per_sink": cap,
        "trace_is_complete": recorded.events_dropped == 0,
        "scope": if recorded.events_dropped == 0 {
            "every call this run put on the wire is one line in this file"
        } else {
            "the file is the first lines of the run's calls, up to the per-sink cap; the \
             remainder is counted in `events_dropped_beyond_the_cap`, and the two together \
             are `calls_the_scan_accounts_for` — no call is unaccounted for, and no call in \
             the file is claimed to be all of them"
        },
    })
}

// ---------------------------------------------------------------------------
// the raw documents
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct M91Pass {
    chain_id: u64,
    verified: Vec<VerifiedPool>,
    state: M91RawState,
}

/// M9.1's own integration outcome, as its raw pass recorded it.
#[derive(Deserialize)]
struct M91RawState {
    attested: Vec<Value>,
    edges: Vec<Value>,
    skipped: Vec<RawSkipped>,
}

/// The same outcome as M9.1 published it in its table for reviewers. Quoting a baseline
/// from memory is how a comparison rots, so the two documents are checked against each
/// other here before either number is used.
#[derive(Deserialize)]
struct M91Published {
    attested: Vec<Value>,
    graph: M91PublishedGraph,
}

#[derive(Deserialize)]
struct M91PublishedGraph {
    block: u64,
    pool_count: usize,
    edges: Vec<Value>,
    skipped: Vec<Value>,
}

/// One RPC call as the trace writes it. Only the fields the gate can check matter here;
/// durations are read off the same line by [`call_tally`].
#[derive(Deserialize)]
struct CallLine {
    method: String,
    #[serde(default)]
    block: Option<String>,
    #[serde(default)]
    duration_ns: u64,
}

/// One skipped pool, with its reason in the shape the graph type actually writes.
///
/// The reason is kept as a `Value` rather than a `String` because it is not always a string:
/// `SkipReason::NotAtTargetBlock` serializes as a unit variant (a JSON string), while
/// `SkipReason::Rejected(EdgeRejection)` serializes as `{"Rejected": "EmptySide"}` (an
/// object). The graph type has no `Deserialize` on purpose — a reason is a conclusion, and
/// conclusions are not read from a file — so the gate compares the two serialized forms
/// instead of reconstructing the enum, and a form that could not round-trip would fail the
/// comparison rather than crash before it.
#[derive(Deserialize, Debug, PartialEq)]
struct RawSkipped {
    pool: PoolId,
    reason: Value,
    state_position: Option<UpdatePosition>,
}

#[derive(Deserialize)]
struct RawState {
    attested: Vec<Value>,
    duplicates: Vec<Value>,
    store_rejections: Vec<Value>,
    snapshot_position: Option<UpdatePosition>,
    snapshot_target_block: Option<u64>,
    graph_block: Option<u64>,
    graph_pool_count: usize,
    edges: Vec<GraphEdge>,
    skipped: Vec<RawSkipped>,
}

/// One run's own account of what its trace holds, as `sync_reconstruction_live.rs` writes
/// it beside the rows. A sink keeps [`MAX_EVENTS_PER_SINK`] events and *counts* the rest,
/// so a trace file's length is not the number of calls a run made — and a table that said
/// otherwise would be describing a file rather than the run that produced it. This document
/// carries both halves, which lets the gate require `recorded + dropped == calls the scan
/// accounts for` instead of quietly publishing a shorter list.
#[derive(Deserialize)]
struct TraceAccountingDoc {
    events_recorded: usize,
    events_dropped: u64,
    max_events_per_sink: usize,
    eth_getlogs_scans: u64,
}

/// One live pass, exactly as `sync_reconstruction_live.rs` wrote it. The two census-only
/// fields are absent for a per-pool strategy, which is why they are not modeled here:
/// serde ignores what a document does not carry, and a census's own per-chunk volume is
/// read from its own file.
#[derive(Deserialize)]
struct RawPass {
    pass: String,
    strategy: String,
    chunk_blocks: u64,
    chain_id: u64,
    target: u64,
    pools_input: usize,
    pools_input_source: String,
    earliest_discovery_block: u64,
    eth_getlogs_requests: usize,
    /// What the node was actually asked for. The harness re-asks a pool the node refused, and
    /// the refused attempt's already-spent requests are disclosed here rather than folded into
    /// the reconstruction's own count. A document without this field has no refusals, so the
    /// two figures agree by construction.
    eth_getlogs_requests_on_the_wire: Option<u64>,
    pool_call_refusals: Option<usize>,
    logs_returned: usize,
    blocks_scanned: u64,
    wall_ms: u64,
    trace: TraceAccountingDoc,
    target_state_valid: usize,
    state_unavailable: usize,
    state_invalid: usize,
    rows: Vec<PoolSyncAtTarget>,
    state: RawState,
}

#[derive(Deserialize)]
struct StrategyRunDoc {
    strategy: String,
    chunk_blocks: u64,
    target: u64,
    pools: usize,
    eth_getlogs_requests: usize,
    logs_returned: usize,
    blocks_scanned: u64,
    blocks_covered_by_the_pass: Option<u64>,
    irrelevant_logs: Option<usize>,
    undecodable_logs: Option<usize>,
    duplicate_logs: Option<usize>,
    wall_ms: u64,
    trace: TraceAccountingDoc,
    target_state_valid: usize,
    state_unavailable: usize,
    state_invalid: usize,
    scan_incomplete: usize,
    rows: Vec<PoolSyncAtTarget>,
}

#[derive(Deserialize)]
struct CapacityProbeRow {
    filter: String,
    from_block: u64,
    to_block: u64,
    blocks: u64,
    #[serde(default)]
    address: Option<String>,
    accepted: bool,
    logs_returned: usize,
    duration_ms: u64,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// the rebuild
// ---------------------------------------------------------------------------

/// One live pass, rebuilt from its own raw records with the production functions.
struct Rebuilt {
    raw: RawPass,
    calls: Vec<CallLine>,
    reconstruction: Reconstruction,
    state: DiscoveredState,
}

impl Rebuilt {
    fn load(pass_relative: &str, calls_relative: &str, verified: &[VerifiedPool]) -> Self {
        let raw: RawPass = read_json(pass_relative);
        let calls: Vec<CallLine> = read_jsonl(calls_relative)
            .into_iter()
            .map(|line| {
                serde_json::from_value(line)
                    .unwrap_or_else(|err| panic!("{pass_relative} trace line: {err}"))
            })
            .collect();
        let chain_id = ChainId(raw.chain_id);

        // The run's own arithmetic, before anything is compared with the tables.
        assert_eq!(
            raw.chain_id,
            verified
                .first()
                .expect("M9.1 verified at least one pool")
                .candidate
                .pool
                .chain_id
                .0,
            "pass {}: the run reconstructed a different chain than the pools it read",
            raw.pass
        );
        assert_eq!(
            raw.pools_input,
            verified.len(),
            "pass {}: the run's input set is not the 80 pools M9.1 verified (§20)",
            raw.pass
        );
        assert_eq!(
            raw.rows.len(),
            raw.pools_input,
            "pass {}: a pool in the input set has no reconstruction row",
            raw.pass
        );

        let reconstruction = Reconstruction {
            chain_id,
            target: BlockNumber(raw.target),
            rows: raw.rows.clone(),
            chunks_scanned: raw.eth_getlogs_requests,
            logs_returned: raw.logs_returned,
        };
        assert_eq!(
            reconstruction.counted(PoolStateAtTarget::Reconstructed),
            raw.target_state_valid,
            "pass {}: the recorded `target_state_valid` is not what the rows say",
            raw.pass
        );
        assert_eq!(
            reconstruction.counted(PoolStateAtTarget::NothingPublished),
            raw.state_unavailable,
            "pass {}: the recorded `state_unavailable` is not what the rows say",
            raw.pass
        );
        assert_eq!(
            reconstruction.counted(PoolStateAtTarget::EmptyReserves),
            raw.state_invalid,
            "pass {}: the recorded `state_invalid` is not what the rows say",
            raw.pass
        );
        assert_eq!(
            reconstruction.counted(PoolStateAtTarget::ScanDoesNotReachTarget),
            0,
            "pass {}: a completed scan always reaches its target, so a row claiming \
             otherwise means the scan was cut short and the run kept going",
            raw.pass
        );

        let state = integrate_at_target(chain_id, &Registry::default(), verified, &reconstruction)
            .unwrap_or_else(|err| panic!("pass {}: integrate_at_target(): {err}", raw.pass));

        // The recorded state is the rebuild's, not a second description of it.
        let recorded = &raw.state;
        assert_eq!(
            serde_json::to_value(&state.attested).expect("attestations serialize"),
            serde_json::to_value(&recorded.attested).expect("the raw document is json"),
            "pass {}: the attestation set in the raw document is not what the rows produce",
            raw.pass
        );
        assert_eq!(
            serde_json::to_value(&state.duplicates).expect("duplicates serialize"),
            serde_json::to_value(&recorded.duplicates).expect("the raw document is json"),
            "pass {}: the duplicate claims in the raw document are not what the rows produce",
            raw.pass
        );
        assert_eq!(
            serde_json::to_value(&state.store_rejections).expect("rejections serialize"),
            serde_json::to_value(&recorded.store_rejections).expect("the raw document is json"),
            "pass {}: the store's refusals in the raw document are not what the rows produce",
            raw.pass
        );
        assert_eq!(
            state.snapshot.position, recorded.snapshot_position,
            "pass {}: the snapshot position moved",
            raw.pass
        );
        assert_eq!(
            state.snapshot.target_block().map(|block| block.0),
            recorded.snapshot_target_block,
            "pass {}: the snapshot is not priced at the recorded target",
            raw.pass
        );
        let edges = edges_of(&state);
        assert_eq!(
            edges, recorded.edges,
            "pass {}: the graph's edges moved",
            raw.pass
        );
        let skipped: Vec<RawSkipped> = skipped_of(&state)
            .iter()
            .map(|row| RawSkipped {
                pool: row.pool,
                reason: serde_json::to_value(row.reason).expect("a skip reason serializes"),
                state_position: row.state_position,
            })
            .collect();
        assert_eq!(
            skipped, recorded.skipped,
            "pass {}: the graph's skips moved",
            raw.pass
        );
        assert_eq!(
            state
                .graph
                .build()
                .map(|build| build.graph.block_number().0),
            recorded.graph_block,
            "pass {}: the graph in the raw document is taken at a different block than the \
             rebuild's",
            raw.pass
        );
        assert_eq!(
            state
                .graph
                .build()
                .map_or(0, |build| build.graph.pool_count()),
            recorded.graph_pool_count,
            "pass {}: the graph's pool count moved",
            raw.pass
        );

        Self {
            raw,
            calls,
            reconstruction,
            state,
        }
    }
}

fn edges_of(state: &DiscoveredState) -> Vec<GraphEdge> {
    match &state.graph {
        GraphOutcome::Built(build) => build.graph.edges().copied().collect(),
        GraphOutcome::NoStateApplied => Vec::new(),
    }
}

fn skipped_of(state: &DiscoveredState) -> Vec<evm_graph::SkippedPool> {
    match &state.graph {
        GraphOutcome::Built(build) => build.skipped.clone(),
        GraphOutcome::NoStateApplied => Vec::new(),
    }
}

/// The calls one pass put on the wire: count, per method, and the latency spread.
fn call_tally(calls: &[CallLine]) -> Value {
    let mut by_method: BTreeMap<String, usize> = BTreeMap::new();
    let mut durations: Vec<u64> = Vec::with_capacity(calls.len());
    let mut block_arguments = 0usize;
    for call in calls {
        *by_method.entry(call.method.clone()).or_default() += 1;
        durations.push(call.duration_ns);
        if call.block.is_some() {
            block_arguments += 1;
        }
    }
    durations.sort_unstable();
    let median_ns = durations
        .get(durations.len() / 2)
        .copied()
        .unwrap_or_default();
    let max_ns = durations.last().copied().unwrap_or_default();
    json!({
        "calls": calls.len(),
        "by_method": by_method,
        "calls_with_a_pinned_block": block_arguments,
        "median_duration_ms": median_ns / 1_000_000,
        "max_duration_ms": max_ns / 1_000_000,
    })
}

/// A row's identity as the tables print it, and as the recompute compares it. Semantic
/// keys only: which pool, at which target, selected at which chain position. A
/// row-count or a sorted position is never an identity — two runs have to agree on which
/// rows are the same rows before any of their numbers mean anything.
fn row_key(row: &PoolSyncAtTarget) -> String {
    let selected = match row.selected {
        Some(sync) => format!(
            "{}/{}/{}",
            sync.block_number.0, sync.tx_index.0, sync.log_index.0
        ),
        None => "none".to_string(),
    };
    format!(
        "{}/{}/{}/{}",
        row.pool.chain_id.0, row.pool.address, row.target.0, selected
    )
}

// ---------------------------------------------------------------------------
// the tables
// ---------------------------------------------------------------------------

/// Quote the run's input file the way this repository's evidence directories quote every
/// other file — by its path inside the repo, so a reviewer on another machine can open
/// what is named. A live run may record either form, so the absolute prefix is stripped
/// when present, and the remainder has to name M9.1's raw pass: shortening a path without
/// checking which file it points at would let a coverage denominator describe a pool set
/// nobody verified.
fn repo_relative_input(source: &str) -> String {
    let relative = source
        .strip_prefix(&format!("{}/", workspace_root().display()))
        .unwrap_or(source);
    assert!(
        relative.starts_with(M91_PASS_REL),
        "the run's input is recorded as {source}, which is not this repository's \
         {M91_PASS_REL} — the pool set it reconstructed is therefore not the set M9.1 \
         published"
    );
    relative.to_string()
}

struct Case {
    chain_id: u64,
    target: u64,
    strategy: String,
    chunk_blocks: u64,
    verified: Vec<VerifiedPool>,
    m91: M91Baseline,
    a: Rebuilt,
    b: Rebuilt,
    pool_arm: StrategyRunDoc,
    census_arm: Option<StrategyRunDoc>,
    capacity: Vec<CapacityProbeRow>,
    calls_read: usize,
    calls_not_read: usize,
    endpoint: String,
}

/// What M9.1 ended up with, measured from M9.1's committed files rather than recited:
/// this run's tables quote it as the baseline a reader compares against.
struct M91Baseline {
    attested: usize,
    graph_pools: usize,
    skipped: usize,
    skip_reasons: BTreeMap<String, usize>,
    edges: usize,
    graph_block: u64,
}

impl Case {
    fn load() -> Self {
        let m91: M91Pass = read_json(M91_PASS_REL);
        assert!(!m91.verified.is_empty(), "M9.1's raw pass carries no pools");
        let published: M91Published = read_json(M91_INTEGRATION_REL);
        // M9.1's two committed documents have to agree with each other before either is
        // quoted as this run's baseline; a restated number that no longer matches its own
        // source is the failure mode §23 names.
        assert_eq!(
            m91.state.attested.len(),
            published.attested.len(),
            "M9.1 attested {} pools in its raw pass and {number} in its published table",
            m91.state.attested.len(),
            number = published.attested.len()
        );
        assert_eq!(
            m91.verified.len(),
            m91.state.attested.len(),
            "M9.1's raw pass lists {} verified pools and attested {number}",
            m91.verified.len(),
            number = m91.state.attested.len()
        );
        assert_eq!(
            published.graph.pool_count + published.graph.skipped.len(),
            published.attested.len(),
            "M9.1's own graph table breaks the §22 equation ({} + {} against {} attested), \
             so its baseline cannot be quoted here",
            published.graph.pool_count,
            published.graph.skipped.len(),
            published.attested.len()
        );
        assert_eq!(
            (m91.state.skipped.len(), m91.state.edges.len()),
            (published.graph.skipped.len(), published.graph.edges.len()),
            "M9.1's raw pass reports {} skips and {} edges, its published table {} and {}",
            m91.state.skipped.len(),
            m91.state.edges.len(),
            published.graph.skipped.len(),
            published.graph.edges.len()
        );
        let mut skip_reasons: BTreeMap<String, usize> = BTreeMap::new();
        for skipped in &m91.state.skipped {
            // The key is the reason's serialized form, which is a bare name for a unit
            // variant and a short object for `Rejected(..)`.
            *skip_reasons.entry(skipped.reason.to_string()).or_default() += 1;
        }
        let m91_baseline = M91Baseline {
            attested: published.attested.len(),
            graph_pools: published.graph.pool_count,
            skipped: published.graph.skipped.len(),
            skip_reasons,
            edges: published.graph.edges.len(),
            graph_block: published.graph.block,
        };
        let a: Rebuilt = Rebuilt::load(PASS_A, CALLS_A, &m91.verified);
        let b: Rebuilt = Rebuilt::load(PASS_B, CALLS_B, &m91.verified);
        assert_eq!(
            m91.chain_id, a.raw.chain_id,
            "the input pools are from chain {} but the run reconstructed chain {}",
            m91.chain_id, a.raw.chain_id
        );
        assert_eq!(
            a.raw.target, b.raw.target,
            "the two passes reconstructed different target blocks, so §27 compares nothing"
        );
        assert_eq!(
            a.raw.strategy, b.raw.strategy,
            "the two passes used different strategies, so §27 compares nothing"
        );
        assert_eq!(
            a.raw.chunk_blocks, b.raw.chunk_blocks,
            "the two passes used different chunk widths"
        );
        // §27, recomputed here rather than trusted from the run that wrote the files.
        assert_eq!(
            a.reconstruction, b.reconstruction,
            "the reconstruction moved between the two passes"
        );
        assert_eq!(
            a.state, b.state,
            "the state and graph moved between the two passes"
        );
        let pool_arm: StrategyRunDoc = read_json(STRATEGY_POOL);
        let census_arm: Option<StrategyRunDoc> =
            if Path::new(&evidence_dir().join(STRATEGY_CENSUS)).exists() {
                Some(read_json(STRATEGY_CENSUS))
            } else {
                None
            };
        let capacity: Vec<CapacityProbeRow> = read_json(PROBE);
        let chain_id = a.raw.chain_id;
        let target = a.raw.target;
        let strategy = a.raw.strategy.clone();
        let chunk_blocks = a.raw.chunk_blocks;
        // The §12 experiment and the §20 target run have to be about the same question:
        // the same block and the same pool set, or the strategy numbers say nothing about
        // the run that used them.
        assert_eq!(
            pool_arm.target, target,
            "the §12 experiment ran at block {} but the target run reconstructed {target}",
            pool_arm.target
        );
        assert_eq!(
            pool_arm.pools,
            a.reconstruction.rows.len(),
            "the §12 experiment asked about {} pools and the target run reconstructed {}",
            pool_arm.pools,
            a.reconstruction.rows.len()
        );
        if let Some(census) = census_arm.as_ref() {
            assert_eq!(
                (census.target, census.pools),
                (pool_arm.target, pool_arm.pools),
                "the two arms of the §12 experiment were not run on the same question"
            );
        }
        // §20 asks for the same target block M9.1 used. The claim is checkable against
        // that milestone's own graph block, so it is checked rather than asserted in prose.
        assert_eq!(
            m91_baseline.graph_block, target,
            "M9.1's graph is taken at block {} and this run reconstructs {target}, so the \
             two are not the same target block (§20)",
            m91_baseline.graph_block
        );
        let calls = traced_calls();
        let calls_read = calls
            .iter()
            .filter(|(_, line)| READ_ONLY_METHODS.contains(&line["method"].as_str().unwrap_or("")))
            .count();
        Self {
            chain_id,
            target,
            strategy,
            chunk_blocks,
            verified: m91.verified,
            m91: m91_baseline,
            a,
            b,
            pool_arm,
            census_arm,
            capacity,
            calls_read,
            calls_not_read: calls.len() - calls_read,
            endpoint: endpoint_digest(),
        }
    }

    fn provenance(&self) -> Value {
        json!({
            "milestone": "M9.2",
            "asked": "§23/§24: evidence that lets a reviewer answer, for every pool M9.1 \
                      verified, which Sync the run selected as its authoritative state, at \
                      which block and log index, with what scan proving nothing later \
                      happened up to the target block, and which of those pools the target \
                      block's graph then took — with every number recomputable from the raw \
                      records beside it.",
            "run_command": "GIWA_RPC_URL=<endpoint from the environment> M92_STRATEGY=<pool|census> \
                            M92_TARGET=37224031 cargo test -p evm-discovery --test \
                            sync_reconstruction_live -- --ignored --nocapture --test-threads=1 \
                            <target name>  (§11 probe, then §12 both arms, then §20/§27)",
            "assemble_command": "M92_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test \
                                 reconstruction_evidence_gate -- --test-threads=1",
            "check_command": "cargo test -p evm-discovery --test reconstruction_evidence_gate -- \
                              --test-threads=1",
            "raw_records": raw_record_names(),
            "assembled_by": "crates/discovery/tests/reconstruction_evidence_gate.rs",
            "chain_id": self.chain_id,
            "endpoint": self.endpoint,
            "endpoint_source": "GIWA_RPC_URL at run time, which is the endpoint recorded at \
             data/evidence/m7/census-pair-created.json#_provenance.rpc_endpoint; this directory \
             names it only as that digest, in the shape RpcTraceSink::endpoint_id uses \
             (crates/chain/src/rpc_trace.rs:933).",
            "endpoint_link": "data/evidence/m7/census-pair-created.json#_provenance.rpc_endpoint",
            "no_key_in_this_file": true,
            "written_at": "the run, not the review: no field in this directory is a restatement \
                           of a number a reader could not recompute from the rows beside it",
        })
    }

    /// §8's lower bound, stated in the shape the run actually has. A per-pool walk never
    /// reads below its own pool's discovery block; a chain-wide census reads one shared
    /// range for the whole asked set, so its bound is held for the pass instead of per
    /// pool. Both are numbers, and neither starts at genesis.
    fn search_from_rule(&self) -> String {
        match self.strategy.as_str() {
            "pool" => "this run scans each pool from that pool's own `discovery_block` up, \
                      so `search_from` is at or above `discovery_block` on every row (§8)"
                .to_string(),
            other => format!(
                "this is a `{other}` run: one chain-wide pass reads a single shared range for \
                 the whole asked set, so every row's `search_from` is the earliest \
                 `discovery_block` in that set rather than the pool's own — the §8 bound is \
                 held for the pass, and no row starts below the earliest discovery block \
                 either"
            ),
        }
    }

    /// §24's per-pool row, in `PoolId` order — the order `Reconstruction` itself keeps,
    /// so the table's order is a property of the model rather than of a HashMap.
    fn reconstruction_rows(&self) -> Vec<Value> {
        self.a
            .reconstruction
            .rows
            .iter()
            .map(|row| {
                let selected = row.selected;
                json!({
                    "chain_id": row.pool.chain_id.0,
                    "pool": row.pool.address,
                    "target_block": row.target.0,
                    "discovery_block": row.discovery_block.0,
                    "search_from": row.search_from.0,
                    "search_to": row.search_to.0,
                    "sync_events_seen": row.sync_logs_seen,
                    "selected_sync_block": selected.map(|sync| sync.block_number.0),
                    "selected_sync_tx_index": selected.map(|sync| sync.tx_index.0),
                    "selected_sync_log_index": selected.map(|sync| sync.log_index.0),
                    "selected_sync_tx_hash": selected.map(|sync| format!("{:?}", sync.tx_hash)),
                    "reserve0": selected.map(|sync| sync.reserve0),
                    "reserve1": selected.map(|sync| sync.reserve1),
                    "state_valid_at_target": row.prices_target(),
                    "reason": row.as_str(),
                    "identity": row_key(row),
                })
            })
            .collect()
    }

    fn reconstruction_table(&self) -> String {
        let rows = self.reconstruction_rows();
        // Seeded with every class the model can put a row in, so a class that took no
        // pool this run reads as `0` — a measurement — rather than as a missing key,
        // which a reader could only take on faith. §21's equation has to be checkable
        // from the committed bytes with no knowledge of which classes happened to occur.
        let mut tally: BTreeMap<String, usize> = [
            "target_state_valid",
            "state_unavailable",
            "state_invalid",
            "scan_incomplete",
        ]
        .iter()
        .map(|name| (name.to_string(), 0usize))
        .collect();
        for row in &rows {
            let reason = row["reason"].as_str().expect("a reason string").to_string();
            *tally.entry(reason).or_default() += 1;
        }
        pretty(
            "reconstruction",
            json!({
                "_provenance": self.provenance(),
                "table": "reconstruction",
                "row_identity": "chain + pool + target block + the chain position of the Sync \
                                it selected (`none` when nothing was selected) — so a row that \
                                changed its selection is a different finding, and two runs of \
                                the same history line up without counting lines",
                "ordering": "PoolId order, which is `Reconstruction::rows`' own order",
                "target_block": self.target,
                "strategy": self.strategy,
                "chunk_blocks": self.chunk_blocks,
                "pools": rows.len(),
                "by_reason": tally,
                "sync_events_seen_total": rows
                    .iter()
                    .map(|row| row["sync_events_seen"].as_u64().expect("a count") as usize)
                    .sum::<usize>(),
                "ordering_rule": "a Sync is selected by (block number, log index) ascending, \
                                  which is the ordering crates/chain/src/recorded.rs already \
                                  treats as the chain's total order for the replay path (§7). \
                                  tx_index is recorded because the log carries it, never \
                                  because it is consulted: a selection that moved when \
                                  tx_index moved would fail \
                                  sync_reconstruction::a_node_that_answers_in_reverse_order_gives_the_same_selection.",
                "fields": format!(
                    "§24. `search_from` is the lowest block actually scanned, \
                     `discovery_block` is the height this pool's history starts at, and the \
                     two are kept apart because they are different facts. {}",
                    self.search_from_rule()
                ),
                "rows": rows,
            }),
        )
    }

    /// The selected `Sync` records themselves: the log the chain emitted, its position,
    /// and the scan that says nothing came after it.
    fn sync_events_table(&self) -> String {
        let rows: Vec<Value> = self
            .a
            .reconstruction
            .rows
            .iter()
            .filter_map(|row| {
                let sync = row.selected?;
                Some(json!({
                    "chain_id": row.pool.chain_id.0,
                    "pool": row.pool.address,
                    "emitter": row.pool.address,
                    "event": "Sync(uint112,uint112)",
                    "topic0": format!("{:?}", evm_protocol::V2Topics::default().sync),
                    "block_number": sync.block_number.0,
                    "tx_index": sync.tx_index.0,
                    "log_index": sync.log_index.0,
                    "tx_hash": format!("{:?}", sync.tx_hash),
                    "reserve0": sync.reserve0,
                    "reserve1": sync.reserve1,
                    "target_block": row.target.0,
                    "blocks_between_state_and_target": row.target.0 - sync.block_number.0,
                    "scan_covered": [row.search_from.0, row.search_to.0],
                    "sync_logs_in_scan": row.sync_logs_seen,
                    "identity": row_key(row),
                }))
            })
            .collect();
        let mut spans: Vec<u64> = rows
            .iter()
            .map(|row| {
                row["blocks_between_state_and_target"]
                    .as_u64()
                    .expect("a span")
            })
            .collect();
        // The middle of the sorted spans, the same upper-median convention the probe
        // statistics below use. Pick the middle of the row order instead and this field is
        // the span of whichever pool sorts to the middle — a pool id, not a percentile.
        spans.sort_unstable();
        pretty(
            "sync-events",
            json!({
                "_provenance": self.provenance(),
                "table": "sync-events",
                "row_identity": "the same key as the reconstruction row it belongs to, so one \
                                Sync and the state it publishes can never be matched to the \
                                wrong pool or the wrong target",
                "selected_events": rows.len(),
                "blocks_behind_target": {
                    "min": spans.first().copied(),
                    "max": spans.last().copied(),
                    "median": spans.get(spans.len() / 2).copied(),
                    "sum": spans.iter().sum::<u64>(),
                    "n": spans.len(),
                    "median_rule": "the middle element of the spans sorted ascending, so an even \
                                   count reports the upper of its two middle values",
                },
                "note": "every row here is the *latest* Sync at or before the target that the \
                        scan found, and `scan_covered` is the range in which it found no other. \
                        `blocks_behind_target` is how stale the authoritative state is — a \
                        description of the market, not a defect: M9.1's 56 `NotAtTargetBlock` \
                        skips become priceable exactly where this table shows a covered gap.",
                "rows": rows,
            }),
        )
    }

    fn graph_table(&self) -> String {
        let build = self.a.state.graph.build();
        let skipped = skipped_of(&self.a.state);
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for row in &skipped {
            let reason = serde_json::to_value(row.reason)
                .expect("a skip reason serializes")
                .to_string();
            *reasons.entry(reason).or_default() += 1;
        }
        pretty(
            "graph-integration",
            json!({
                "_provenance": self.provenance(),
                "table": "graph-integration",
                "flow": "reconstructed rows -> Registry::validate/merge -> InMemoryStateStore \
                         -> StateSnapshot::at_target -> MarketGraphBuilder. The doors are the \
                         ones M1..M9.1 already had; `integrate_at_target` is the only new one, \
                         and it changes what is handed to the store, not what the store does \
                         with it (§17).",
                "target_block": self.target,
                "attested": self.a.state.attested.len(),
                "duplicates": self.a.raw.state.duplicates.len(),
                "store_rejections": self.a.state.store_rejections.len(),
                "snapshot_position": self.a.state.snapshot.position,
                "snapshot_target_block": self.a.state.snapshot.target_block(),
                "graph_block": build.map(|build| build.graph.block_number()),
                "graph_pool_count": build.map_or(0, |build| build.graph.pool_count()),
                "edges": edges_of(&self.a.state),
                "skipped": skipped,
                "skip_reason_tally": reasons,
                "accounting": "graph pools + skipped pools = attested pools (§22), and every \
                               skipped pool appears once with one reason",
                "note": "state at the target is not the same statement as a pool M9.1 \
                         verified: see `rejected-pools.json` for the pools this run could not \
                         price, and `summary.json#not_claimed` for what no number here \
                         claims.",
            }),
        )
    }

    fn rpc_table(&self) -> String {
        let a = call_tally(&self.a.calls);
        let b = call_tally(&self.b.calls);
        // Quoted from [`Case::call_accounts`] rather than re-derived here, because that is the
        // function that names which trace file belongs to which run and which of a run's two
        // request figures is the one the trace must reconcile with — a second reading of the
        // same fields here could quietly pick the other one.
        let accounts = self.call_accounts();
        let accounting = |label: &str| -> Value {
            accounts
                .iter()
                .find(|row| row["run"].as_str() == Some(label))
                .cloned()
                .unwrap_or_else(|| panic!("`call_accounts` published no row for {label}"))
        };
        let accounting_a = accounting("pass a");
        let accounting_b = accounting("pass b");
        pretty(
            "rpc-calls",
            json!({
                "_provenance": self.provenance(),
                "table": "rpc-calls",
                "read_only_methods": READ_ONLY_METHODS,
                "calls_outside_the_read_only_set": self.calls_not_read,
                "scope_of_that_count": "every trace file in raw/ — both reconstruction passes, \
                                        both arms of the §12 experiment, and the §11 probe, as \
                                        `manifest.json#raw_files` lists them. §28 asks for \
                                        signatures, broadcasts and real arbitrage to be zero; \
                                        anything signed or broadcast would have reached the \
                                        node through a method in one of these traces, and the \
                                        run held no signing key and had no broadcast path.",
                "passes": [
                    {
                        "pass": self.a.raw.pass,
                        "strategy": self.a.raw.strategy,
                        "chunk_blocks": self.a.raw.chunk_blocks,
                        "target_block": self.a.raw.target,
                        "tally": a,
                        "accounting": accounting_a,
                        "wall_ms": self.a.raw.wall_ms,
                        "logs_returned": self.a.raw.logs_returned,
                        "blocks_scanned": self.a.raw.blocks_scanned,
                    },
                    {
                        "pass": self.b.raw.pass,
                        "strategy": self.b.raw.strategy,
                        "chunk_blocks": self.b.raw.chunk_blocks,
                        "target_block": self.b.raw.target,
                        "tally": b,
                        "accounting": accounting_b,
                        "wall_ms": self.b.raw.wall_ms,
                        "logs_returned": self.b.raw.logs_returned,
                        "blocks_scanned": self.b.raw.blocks_scanned,
                    },
                ],
                "trace_is_the_source": "the per-method counts above are read off the \
                                        adapter's own trace lines, and `accounting` then \
                                        reconciles those lines against the request count in \
                                        the same run's document. One sink holds \
                                        `cap_per_sink` events and counts the rest, so a \
                                        longer run's file is the first `cap_per_sink` calls \
                                        and `events_dropped_beyond_the_cap` names the rest — \
                                        the file is never presented as the whole run. Both \
                                        halves have to add up to the calls the scan accounts \
                                        for, and `sync_reconstruction_live::audit_trace` \
                                        requires the same sum before a run commits anything: \
                                        a method outside `read_only_methods`, a block \
                                        argument that is not a pinned number, a lost event or \
                                        a refused window stops the run.",
                "no_latest_state": format!(
                    "a log query's block range is not a field the sink stamps (`eth_getLogs` \
                     lines carry `block: null`; `tally.calls_with_a_pinned_block` counts the \
                     lines that do carry one, which is how much the trace can show and no \
                     more). The pinned-range claim rests on the two things that do record it: \
                     every row in `reconstruction.json` names `search_from`/`search_to` as \
                     numbers, `search_to` is equal to the target, and {bound}; their sum is \
                     this table's `blocks_scanned`. And the type that reaches the wire is \
                     `LogFilter {{ from_block: BlockNumber, to_block: BlockNumber }}` \
                     (`crates/chain/src/types.rs:135`), whose two fields `block_param` \
                     formats as hex numbers (`crates/chain/src/rpc.rs:396`) — a tag has no \
                     type here, so no historical read in this milestone could be answered by \
                     `latest` (§10).",
                    bound = self.search_from_rule(),
                ),
            }),
        )
    }

    /// The pools this run could not price at the target, each with the reason the scan
    /// itself gave. §21's other two classes.
    fn unavailable_table(&self) -> String {
        let rows: Vec<Value> = self
            .a
            .reconstruction
            .rows
            .iter()
            .filter(|row| !row.prices_target())
            .map(|row| {
                let selected = row.selected;
                json!({
                    "chain_id": row.pool.chain_id.0,
                    "pool": row.pool.address,
                    "target_block": row.target.0,
                    "discovery_block": row.discovery_block.0,
                    "search_from": row.search_from.0,
                    "search_to": row.search_to.0,
                    "sync_events_seen": row.sync_logs_seen,
                    "selected_sync_block": selected.map(|sync| sync.block_number.0),
                    "reserve0": selected.map(|sync| sync.reserve0),
                    "reserve1": selected.map(|sync| sync.reserve1),
                    "reason": row.as_str(),
                    "what_would_have_made_it_valid": match row.outcome() {
                        PoolStateAtTarget::NothingPublished =>
                            "a Sync the pool published in its own history at or before the \
                             target — the scan covered every block from `search_from` to the \
                             target and reported none, so no amount of waiting on this range \
                             changes the answer",
                        PoolStateAtTarget::EmptyReserves =>
                            "non-zero reserves on both sides; the state layer refuses an empty \
                             side rather than pricing it, which is `empty_reserves` in \
                             `graph-integration.json#store_rejections`",
                        PoolStateAtTarget::Reconstructed => "not applicable — this row is valid",
                        PoolStateAtTarget::ScanDoesNotReachTarget =>
                            "a scan that reached the target; a production run cannot produce \
                             this row, and its appearance here means a scan was cut short",
                    },
                    "verified_by_m9_1": true,
                    "identity": row_key(row),
                })
            })
            .collect();
        pretty(
            "rejected-pools",
            json!({
                "_provenance": self.provenance(),
                "table": "rejected-pools",
                "row_identity": "the same semantic key as the pool's reconstruction row",
                "note": "these are not verification rejections — every pool here is one M9.1 \
                        verified and attested. This is the state layer's answer about a \
                        target block: a market that published nothing in the range, or \
                        published an empty side. `verified_by_m9_1` is written as `true` \
                        because §36 forbids a table that lets a reader collapse \
                        Verified into State valid at target; the two columns are this row's \
                        whole point.",
                "count": rows.len(),
                "rows": rows,
            }),
        )
    }

    fn strategy_table(&self) -> String {
        // `row_key` carries the pool *and* the chain position of the Sync that arm selected, so
        // two arms whose lists agree selected the same state for every pool — the comparison is
        // of the selections themselves, not of how many rows each arm happened to write (§12).
        let selections = |arm: &StrategyRunDoc| -> Vec<String> {
            let mut keys: Vec<String> = arm.rows.iter().map(row_key).collect();
            keys.sort();
            keys
        };
        let pool_keys = selections(&self.pool_arm);
        // Three states, not two. `false` is the answer to "the two arms disagreed", which is a
        // finding; `null` is the answer when there is no second arm to compare against, which
        // is the absence of a measurement. Collapsing the two would let a table say the census
        // arm chose different Sync events from the per-pool arm when nothing was ever compared.
        let agree: Option<bool> = self
            .census_arm
            .as_ref()
            .map(|census| selections(census) == pool_keys);
        // §12's cost figures were measured at one chunk width; the target run may scan at
        // another (a wider chunk is the other way to buy the same reconstruction, and §11
        // invites a re-run at a measured width). The swap is only free if it selects the same
        // `Sync`, so the two documents are compared here and the answer is published rather
        // than left to a reader who assumes chunking cannot change a result.
        let target_keys = {
            let mut keys: Vec<String> = self.a.reconstruction.rows.iter().map(row_key).collect();
            keys.sort();
            keys
        };
        let rows = std::iter::once(&self.pool_arm)
            .chain(self.census_arm.iter())
            .map(|arm| {
                let (trace_file, shape) = match arm.strategy.as_str() {
                    "pool" => (STRATEGY_POOL_CALLS, "per-pool address filter"),
                    _ => (STRATEGY_CENSUS_CALLS, "chain-wide Sync scan"),
                };
                json!({
                    "strategy": arm.strategy,
                    "shape": match arm.strategy.as_str() {
                        "pool" => "address = the pool, topic0 = Sync(uint112,uint112)",
                        _ => "address = empty, topic0 = Sync(uint112,uint112)",
                    },
                    "chunk_blocks": arm.chunk_blocks,
                    "target_block": arm.target,
                    "pools": arm.pools,
                    "eth_getlogs_requests": arm.eth_getlogs_requests,
                    "logs_returned": arm.logs_returned,
                    "blocks_scanned": arm.blocks_scanned,
                    "blocks_covered_by_the_pass": arm.blocks_covered_by_the_pass,
                    "irrelevant_logs": arm.irrelevant_logs,
                    "undecodable_logs": arm.undecodable_logs,
                    "duplicate_logs": arm.duplicate_logs,
                    "wall_ms": arm.wall_ms,
                    "accounting": call_accounting(
                        trace_file,
                        arm.eth_getlogs_requests as u64,
                        &arm.trace,
                    ),
                    "trace_scope": format!(
                        "the {shape} arm's calls, counted from `{trace_file}`"
                    ),
                    "target_state_valid": arm.target_state_valid,
                    "state_unavailable": arm.state_unavailable,
                    "state_invalid": arm.state_invalid,
                    "scan_incomplete": arm.scan_incomplete,
                })
            })
            .collect::<Vec<Value>>();
        // §12's decider, read off the two arms rather than from a sentence written before
        // either of them ran. Both arms are correct by the check above, so what is left is a
        // cost choice — and the figure two arms may be compared on is the one that does not
        // depend on when either was measured: a count. `wall_ms` is published beside it and is
        // deliberately not the gate. Each arm's duration belongs to its own node window, and
        // the same arm run twice in different windows reproduced its request count exactly
        // (21,439 both times, 80 pools, target 37,224,031) while its duration moved 10.9% —
        // 6,505,859 ms against 7,216,588 ms. A ratio between two arms' durations is therefore
        // a statement about two windows, not about two strategies, and §12's "not by intuition
        // alone" is answered by the count, which is reproducible.
        let cheaper = self.census_arm.as_ref().map(|census| {
            if census.eth_getlogs_requests < self.pool_arm.eth_getlogs_requests {
                census.strategy.clone()
            } else {
                self.pool_arm.strategy.clone()
            }
        });
        if let Some(cheaper) = cheaper.as_ref() {
            let (fewer, more) = if cheaper == &self.pool_arm.strategy {
                (
                    &self.pool_arm,
                    self.census_arm
                        .as_ref()
                        .expect("the second of two measured arms"),
                )
            } else {
                (
                    self.census_arm
                        .as_ref()
                        .expect("the second of two measured arms"),
                    &self.pool_arm,
                )
            };
            assert_eq!(
                self.strategy, *cheaper,
                "the §12 experiment measured `{cheaper}` as the arm that asked for the same \
                 correct reconstruction in {} `eth_getLogs` requests against {} for `{}`. §12 \
                 forbids choosing by intuition: rerun §20 with the arm that asked for less, or \
                 answer this with a written reason against the two rows in \
                 `strategy-comparison.json`.",
                fewer.eth_getlogs_requests, more.eth_getlogs_requests, more.strategy
            );
        }
        // The census arm gets a row here only if it left a document. When it did not, the
        // most that can still be said about it has to be read off files that do exist: the
        // request count a full census would ask is arithmetic over the range it covers, and
        // the per-chunk log volume and duration are this node's own answers in
        // `node-capacity.json`. Those are costs of one chunk, not of a completed pass: the
        // probe's windows sit mostly in the recent, dense part of the chain, so multiplying
        // them out would be an era-biased guess dressed as a measurement, and this table
        // does not do that. What ended the census attempt is a claim about a run that wrote
        // no file, so it belongs in the completion report, which is allowed to say that.
        let census_unmeasured = self.census_arm.is_none().then(|| {
            let earliest = self
                .pool_arm
                .rows
                .iter()
                .map(|row| row.discovery_block.0)
                .min()
                .unwrap_or(self.target);
            let blocks = self.target.saturating_add(1).saturating_sub(earliest);
            let experiment_width = self.pool_arm.chunk_blocks;
            let run_width = self.chunk_blocks;
            let busiest = self
                .capacity
                .iter()
                .filter(|row| row.accepted && row.filter == "chain-wide")
                .map(|row| row.logs_returned as u64)
                .max()
                .unwrap_or_default();
            let ceiling = NODE_LOG_LIMIT as u64;
            let stats = |values: &mut Vec<u64>| -> Value {
                values.sort_unstable();
                json!({
                    "min": values.first().copied(),
                    "median": values.get(values.len() / 2).copied(),
                    "max": values.last().copied(),
                    "samples": values.len(),
                })
            };
            let mut volumes = self
                .capacity
                .iter()
                .filter(|row| row.accepted && row.filter == "chain-wide")
                .map(|row| row.logs_returned as u64)
                .collect::<Vec<u64>>();
            let mut durations = self
                .capacity
                .iter()
                .filter(|row| row.accepted && row.filter == "chain-wide")
                .map(|row| row.duration_ms)
                .collect::<Vec<u64>>();
            let widest = self
                .capacity
                .iter()
                .filter(|row| row.accepted && row.filter == "chain-wide")
                .map(|row| {
                    (
                        row.from_block,
                        row.to_block,
                        row.logs_returned,
                        row.duration_ms,
                    )
                })
                .collect::<Vec<(u64, u64, usize, u64)>>();
            json!({
                "strategy": "census",
                "shape": "address = empty, topic0 = Sync(uint112,uint128-free V2 Sync)",
                "status": "no completed run: `raw/strategy-b-census.json` is absent from this \
                           directory, so this arm has no selections to compare and no wall \
                           time to publish",
                "correctness": "not established — an arm with no document cannot be shown to \
                                have picked the same Sync as the other arm, which is the half \
                                of §12 that comes first",
                "requests_a_full_pass_would_ask": {
                    "value": blocks.div_ceil(experiment_width),
                    "how": format!(
                        "arithmetic, not a measurement: the census covers \
                         `earliest_discovery_block..=target` as one range in chunks, so the \
                         count is ({target} + 1 - {earliest} = {blocks} blocks) divided by \
                         {experiment_width} rounded up. `earliest_discovery_block` and the \
                         target are read from `raw/strategy-a-pool.json`'s rows.",
                        target = self.target,
                        earliest = earliest,
                        blocks = blocks,
                        experiment_width = experiment_width,
                    ),
                    "earliest_discovery_block": earliest,
                    "blocks": blocks,
                    "chunk_blocks": experiment_width,
                    // A request count is only comparable with another count taken at the same
                    // chunk width, and §12's one measured arm was measured at its width. If
                    // the target run chose a different width, that number still describes the
                    // experiment — and the difference is named rather than left to a reader
                    // who multiplies logs per chunk by the wrong chunk count.
                    "the_target_run_used_this_width": run_width,
                    "width_note": match run_width == experiment_width {
                        true => "the target run scanned at the same width the per-pool arm was \
                                measured at, so the count above and the measured arm's row are \
                                the same experiment."
                            .to_string(),
                        false => format!(
                            "the target run scanned at {run_width} blocks per chunk against the \
                             per-pool arm's {experiment_width}, so `value` is the census's cost \
                             at the measured arm's width and is not what a run at {run_width} \
                             would ask. The two widths agree on every selection only if the \
                             invariance test below says so, and the wider number is admissible \
                             for this filter shape alone: the probe's busiest accepted \
                             chain-wide chunk returned {busiest} of the node's {ceiling} logs \
                             per request, so a chain-wide pass at the wider width would spend \
                             its headroom reading denser eras, while a per-pool chunk of the \
                             same width carries one pool's `Sync` at a time."
                        ),
                    },
                },
                "per_chunk_cost_measured_by_the_probe": {
                    "logs_returned": stats(&mut volumes),
                    "duration_ms": stats(&mut durations),
                    "windows": widest
                        .iter()
                        .map(|(from, to, logs, ms)| json!({
                            "from_block": from,
                            "to_block": to,
                            "logs_returned": logs,
                            "duration_ms": ms,
                        }))
                        .collect::<Vec<Value>>(),
                    "do_not_multiply_out": "these windows sit in the dense, recent part of the \
                                            chain except for three older samples, so a total \
                                            built from their median would overstate the pass. \
                                            §12's four figures for this arm are therefore \
                                            published per chunk and as a request count, and not \
                                            as a completed-pass cost.",
                },
                "wall_ms": Value::Null,
                "chosen_instead_of": self.strategy,
            })
        });
        let probe_stats = |filter: &str| -> Value {
            let mut milliseconds: Vec<u64> = self
                .capacity
                .iter()
                .filter(|row| row.accepted && row.filter == filter)
                .map(|row| row.duration_ms)
                .collect();
            milliseconds.sort_unstable();
            json!({
                "accepted_requests": milliseconds.len(),
                "min_ms": milliseconds.first().copied(),
                "median_ms": milliseconds.get(milliseconds.len() / 2).copied(),
                "max_ms": milliseconds.last().copied(),
            })
        };
        pretty(
            "strategy-comparison",
            json!({
                "_provenance": self.provenance(),
                "table": "strategy-comparison",
                "row_identity": "one row per query strategy; the strategies differ by the \
                                filter shape, which is the experiment (§12)",
                "asked": "§12: which filter shape reconstructs the same market state for fewer \
                          `eth_getLogs` requests — a per-pool address filter, or one chain-wide \
                          Sync log scan shared by every pool?",
                "correctness_first": agree.map_or(Value::Null, |agreed| json!(agreed)),
                "correctness_note": match agree {
                    Some(true) => "both arms have a document beside each other and the \
                                   selections are equal by semantic key, so what is left is a \
                                   cost choice. The live test stops the run if they disagree — \
                                   a cheaper run that disagrees is a bug, not a strategy."
                        .to_string(),
                    Some(false) => "the two arms disagree about which Sync prices a pool. This \
                                    comparison cannot support a cost choice at all (§12 gives \
                                    correctness priority)."
                        .to_string(),
                    None => "there is no second document, so no comparison was made and this \
                             field is null rather than false. The absence is a measurement that \
                             was not taken, not a disagreement: only the per-pool arm has a \
                             committed record here, so the §12 experiment has one measured arm \
                             and one unmeasured one. Whatever ended the unmeasured arm is a \
                             claim about attempts that produced no file, and it belongs in the \
                             completion report rather than in a table that can only read files."
                        .to_string(),
                },
                "decision": {
                    "chosen_for_the_target_run": self.strategy,
                    "cheaper_arm_by_requests": cheaper.clone(),
                    "cost_assert_active": cheaper.is_some(),
                    "target_run_against_the_measured_arm": {
                        "same_selections": target_keys == pool_keys,
                        "chunk_blocks_measured": self.pool_arm.chunk_blocks,
                        "chunk_blocks_used_by_the_target_run": self.a.raw.chunk_blocks,
                        "note": match self.a.raw.chunk_blocks == self.pool_arm.chunk_blocks {
                            true => format!(
                                "the target run scanned at the width the per-pool arm was \
                                 measured at, and its {} selections are the same chain positions \
                                 by the semantic key of this table's own rows.",
                                target_keys.len()
                            ),
                            false => format!(
                                "the target run scanned at a different chunk width from the one \
                                 the cost was measured at, so the count it paid belongs to no \
                                 row of this table. The two documents are compared by the \
                                 semantic key of this table's own rows over {} pools, and \
                                 `same_selections` is the answer: a different width bought a \
                                 different cost only if it bought the same state, which is what \
                                 the gate test \
                                 `the_target_run_selects_the_same_sync_as_the_experiment_arm` \
                                 asserts against these bytes.",
                                target_keys.len()
                            ),
                        },
                    },
                    "duration_is_not_the_criterion": "each arm's `wall_ms` is the duration of \
                                                      its own node window, and the windows are \
                                                      not the same moment: the same arm, run \
                                                      twice at this target over these pools, \
                                                      reproduced its request count exactly and \
                                                      moved its duration 10.9%. The gate reads \
                                                      the count.",
                    "why": match self.census_arm.as_ref() {
                        Some(census) => format!(
                            "the two arms agreed on every selection, so the choice was a cost \
                             choice, made on the reproducible cost figure — requests asked for: \
                             `{}` at {} against `{}` at {}. The same pair of durations is \
                             published in the rows ({} ms against {} ms) as a diagnostic; the \
                             gate asserts the target run used the arm that asked for fewer \
                             requests, and any other choice has to be argued here, against \
                             these two rows (§12).",
                            self.pool_arm.strategy,
                            self.pool_arm.eth_getlogs_requests,
                            census.strategy,
                            census.eth_getlogs_requests,
                            self.pool_arm.wall_ms,
                            census.wall_ms
                        ),
                        None => format!(
                            "the target run used `{}` — the only arm of §12 that has a completed \
                             document on disk. The cost rule in this table asks which of two \
                             measured arms asked for fewer `eth_getLogs` requests, so with one \
                             row it has nothing to read and the assertion is inactive: an arm \
                             that produced no document has no measured correctness and no \
                             measured cost, and §12 gives correctness priority over the count. \
                             Why the second arm has no document is stated in the completion \
                             report, because a table cannot evidence an attempt that wrote no \
                             file.",
                            self.pool_arm.strategy
                        ),
                    },
                    "per_request_from_the_probe": {
                        "pool_filter": probe_stats("pool"),
                        "chain_wide": probe_stats("chain-wide"),
                        "note": "durations of accepted probe requests only; the refused \
                                 widths are in `node-capacity.json` with their errors. A \
                                 chain-wide chunk's duration also carries its log volume, so \
                                 no duration here is comparable with a duration from the other \
                                 shape — which is why `cheaper_arm_by_requests` reads the \
                                 request count and these figures are context.",
                    },
                },
                "cost_columns": "`eth_getlogs_requests` is a count of requests and \
                                 `logs_returned` a count of logs; both are the same figure \
                                 however many times the arm is measured, and the request count \
                                 is what the decision reads. `wall_ms` is published but is a \
                                 property of the window it was measured in, not of the \
                                 strategy. The block columns are not comparable either: \
                                 `blocks_scanned` is summed over pools, so a chain-wide pass \
                                 that read one range for every pool counts that range once per \
                                 pool, while `blocks_covered_by_the_pass` is the history the \
                                 pass read as a single range and is null for the per-pool arm, \
                                 which has no one pass to name \
                                 (`crates/discovery/src/reconstruct.rs:257`).",
                "rows": rows,
                "census_arm_without_a_document": census_unmeasured,
            }),
        )
    }

    fn capacity_table(&self) -> String {
        let rows: Vec<Value> = self
            .capacity
            .iter()
            .map(|row| {
                json!({
                    "filter": row.filter,
                    "from_block": row.from_block,
                    "to_block": row.to_block,
                    "blocks": row.blocks,
                    "address": row.address,
                    "accepted": row.accepted,
                    "logs_returned": row.logs_returned,
                    "duration_ms": row.duration_ms,
                    "error": row.error,
                    "fraction_of_the_node_log_limit": if row.accepted && row.logs_returned > 0 {
                        format!(
                            "{:.3}",
                            row.logs_returned as f64 / evm_discovery::NODE_LOG_LIMIT as f64
                        )
                    } else {
                        "n/a".to_string()
                    },
                })
            })
            .collect();
        let rejected: Vec<&CapacityProbeRow> =
            self.capacity.iter().filter(|row| !row.accepted).collect();
        let accepted: Vec<&CapacityProbeRow> =
            self.capacity.iter().filter(|row| row.accepted).collect();
        let widest = accepted.iter().map(|row| row.blocks).max();
        let busiest = accepted
            .iter()
            .filter(|row| row.filter == "chain-wide")
            .max_by_key(|row| row.logs_returned)
            .map(|row| (row.blocks, row.logs_returned, row.duration_ms));
        pretty(
            "node-capacity",
            json!({
                "_provenance": self.provenance(),
                "table": "node-capacity",
                "row_identity": "one row per probe request: filter shape + block range asked",
                "asked": "§11/§12: what range width does this node answer, how many Sync logs \
                         does a chain-wide chunk actually carry, and what does each request \
                         cost in wall time — the inputs to a chunk width that is not \
                         hard-coded from intuition.",
                "widest_accepted_blocks": widest,
                "narrowest_rejected_blocks": rejected.iter().map(|row| row.blocks).min(),
                "first_rejection": rejected.first().map(|row| row.error.clone()),
                "busiest_chain_wide_chunk": busiest.map(|(blocks, logs_returned, duration_ms)| json!({
                    "blocks": blocks,
                    "logs_returned": logs_returned,
                    "duration_ms": duration_ms,
                })),
                "chunk_blocks_used_by_the_run": self.chunk_blocks,
                "default_chunk_blocks_in_code": CHUNK_BLOCKS,
                "rows": rows,
                "limitation": "the node's own cap is on the requested width (an inclusive \
                               range of 10,001 blocks was accepted and 20,000 was refused \
                               with `-32602 block range greater than 10000 max`), which is \
                               wider than the 4,999 this project measures against. Width is \
                               not the binding constraint here: log volume per chunk is, \
                               because a chain-wide chunk that fills `NODE_LOG_LIMIT` would \
                               truncate, and a truncated range cannot prove the absence of a \
                               Sync (§25). The run keeps the measured chunk width and pays \
                               more requests rather than accept that risk.",
            }),
        )
    }

    /// Every run this directory commits a trace for, reconciled against its own request
    /// count. `summary.json#rpc.per_run_call_accounting` publishes this list,
    /// `rpc-calls.json` and `strategy-comparison.json` quote rows from it, and
    /// [`Case::call_accounts`] is the only place the four documents are named — so the test
    /// that requires the sums to close walks exactly the rows the tables publish.
    fn call_accounts(&self) -> Vec<Value> {
        let mut rows = Vec::new();
        // A pass's cost is read as the figure the node was asked for, not the figure the
        // reconstruction kept: the harness may re-ask a pool the node refused, and every one
        // of those requests is a line in the trace file. A document that predates the field
        // has no refusals, so the two figures are the same number.
        let wire = |raw: &RawPass| -> u64 {
            raw.eth_getlogs_requests_on_the_wire
                .unwrap_or(raw.eth_getlogs_requests as u64)
        };
        for (label, file, requests, kept, trace, refusals) in [
            (
                "pass a",
                CALLS_A,
                wire(&self.a.raw),
                self.a.raw.eth_getlogs_requests as u64,
                &self.a.raw.trace,
                self.a.raw.pool_call_refusals,
            ),
            (
                "pass b",
                CALLS_B,
                wire(&self.b.raw),
                self.b.raw.eth_getlogs_requests as u64,
                &self.b.raw.trace,
                self.b.raw.pool_call_refusals,
            ),
            (
                "arm pool",
                STRATEGY_POOL_CALLS,
                self.pool_arm.eth_getlogs_requests as u64,
                self.pool_arm.eth_getlogs_requests as u64,
                &self.pool_arm.trace,
                None,
            ),
        ] {
            let mut row = call_accounting(file, requests, trace);
            row["run"] = json!(label);
            row["pool_call_refusals"] = json!(refusals.unwrap_or_default());
            row["eth_getlogs_requests_the_rows_were_built_from"] = json!(kept);
            rows.push(row);
        }
        if let Some(census) = self.census_arm.as_ref() {
            let mut row = call_accounting(
                STRATEGY_CENSUS_CALLS,
                census.eth_getlogs_requests as u64,
                &census.trace,
            );
            row["run"] = json!("arm census");
            row["pool_call_refusals"] = json!(0);
            row["eth_getlogs_requests_the_rows_were_built_from"] =
                json!(census.eth_getlogs_requests);
            rows.push(row);
        }
        rows
    }

    fn summary_table(&self) -> String {
        let rows = self.reconstruction_rows();
        let valid = rows
            .iter()
            .filter(|row| row["state_valid_at_target"].as_bool().expect("a bool"))
            .count();
        let build = self.a.state.graph.build();
        let graph_pools = build.map_or(0, |built| built.graph.pool_count());
        let skipped = skipped_of(&self.a.state);
        // One row per run this directory commits a trace for, so a reader of `summary.json`
        // can tell how many of the milestone's calls are in the trace files and how many the
        // sinks could only count.
        let caps = self.call_accounts();
        let counted_beyond_the_caps: u64 = caps
            .iter()
            .map(|row| {
                row["events_dropped_beyond_the_cap"]
                    .as_u64()
                    .expect("a dropped count")
            })
            .sum();
        pretty(
            "summary",
            json!({
                "_provenance": self.provenance(),
                "table": "summary",
                "verdict": if graph_pools > 0 {
                    "GRAPH_TAKEN_AT_TARGET"
                } else if valid > 0 {
                    "STATE_AT_TARGET_BUT_NOTHING_PRICED"
                } else {
                    "NO_STATE_AT_TARGET"
                },
                "verdict_rule": "`GRAPH_TAKEN_AT_TARGET` requires the actual GraphSnapshot to \
                                hold at least one pool. Every state being valid at the target \
                                while the graph still refuses all of them is the middle verdict, \
                                not the first one (§36): this field is read off the built graph, \
                                not off the reconstruction's own optimism.",
                "chain_id": self.chain_id,
                "target_block": self.target,
                "strategy": self.strategy,
                "chunk_blocks": self.chunk_blocks,
                "input": {
                    "source": repo_relative_input(&self.a.raw.pools_input_source),
                    "pools_verified_by_m9_1": self.verified.len(),
                    "earliest_discovery_block": self.a.raw.earliest_discovery_block,
                    "pools_reconstructed": rows.len(),
                },
                "state_coverage": {
                    "verified": self.verified.len(),
                    "target_state_valid": valid,
                    "state_unavailable": self.a.raw.state_unavailable,
                    "state_invalid": self.a.raw.state_invalid,
                    "equation": format!(
                        "{} = {} + {} + {}",
                        self.verified.len(),
                        valid,
                        self.a.raw.state_unavailable,
                        self.a.raw.state_invalid
                    ),
                },
                "graph_coverage": {
                    "attested": self.a.state.attested.len(),
                    "graph_pool_count": graph_pools,
                    "skipped": skipped.len(),
                    "edges": edges_of(&self.a.state).len(),
                    "equation": format!(
                        "{} + {} = {}",
                        graph_pools,
                        skipped.len(),
                        self.a.state.attested.len()
                    ),
                    "m9_1_baseline": {
                        "source": [M91_PASS_REL, M91_INTEGRATION_REL],
                        "attested": self.m91.attested,
                        "graph_pool_count": self.m91.graph_pools,
                        "skipped": self.m91.skipped,
                        "skip_reasons": self.m91.skip_reasons,
                        "edges": self.m91.edges,
                        "graph_block": self.m91.graph_block,
                        "note": "M9.1 attested the same 80 pools and could offer the graph only \
                                 the Sync from each pool's creation window, which is why most \
                                 of them were skipped as it could not place them at its own \
                                 target block. What changed in this milestone is the input to \
                                 the graph, not the graph's rule: the check is still \
                                 `state.block == graph.target_block` (§2, §16).",
                    },
                },
                "freshness": {
                    "rule": "for every pool in the graph, the scan covered every block between \
                             the selected Sync and the target and returned no other Sync from \
                             that pool (§25). That is the actual query's answer, not an \
                             assumption about inactivity.",
                    "blocks_behind_target": "see sync-events.json#blocks_behind_target",
                },
                "determinism": {
                    "passes": [self.a.raw.pass.clone(), self.b.raw.pass.clone()],
                    "reconstruction_equal": self.a.reconstruction == self.b.reconstruction,
                    "state_and_graph_equal": self.a.state == self.b.state,
                    "row_order_equal": self
                        .a
                        .reconstruction
                        .rows
                        .iter()
                        .map(row_key)
                        .collect::<Vec<_>>()
                        == self
                        .b
                        .reconstruction
                        .rows
                        .iter()
                        .map(row_key)
                        .collect::<Vec<_>>(),
                    "cross_process": "this gate is a second process: it hydrates pass a's \
                                      committed rows, runs the production integration over \
                                      them, and compares the graph it builds with the graph \
                                      the run recorded — bytes in, same values out, rather \
                                      than one run's memory compared with itself.",
                },
                "rpc": {
                    "pass_a_requests": self.a.raw.eth_getlogs_requests,
                    "pass_b_requests": self.b.raw.eth_getlogs_requests,
                    "pass_a_logs_returned": self.a.raw.logs_returned,
                    "pass_b_logs_returned": self.b.raw.logs_returned,
                    "pass_a_wall_ms": self.a.raw.wall_ms,
                    "pass_b_wall_ms": self.b.raw.wall_ms,
                    "methods": READ_ONLY_METHODS,
                    "traced_calls": self.calls_read + self.calls_not_read,
                    "calls_outside_the_read_only_set": self.calls_not_read,
                    "calls_counted_beyond_the_sink_caps": counted_beyond_the_caps,
                    "per_run_call_accounting": caps,
                    "what_traced_calls_counts": "lines across the five trace files. A sink \
                        holds `MAX_EVENTS_PER_SINK` events and counts the rest, so this is \
                        every call of a run that fit under the cap and the first `cap` calls \
                        of a run that did not; `per_run_call_accounting` names, for each run, \
                        its recorded lines, its counted remainder, and the number of calls \
                        its own scan accounts for — the three add up on every row or this \
                        table is not written. A per-pool run has two request figures because \
                        the harness may re-ask a pool the node refused: \
                        `eth_getlogs_requests_the_rows_were_built_from` is the scans the \
                        reconstruction is made of, `eth_getlogs_requests` is everything the \
                        node was asked for including the refused attempts, and \
                        `pool_call_refusals` is how many pool calls were re-asked. The trace \
                        reconciles against the second number, since the refused requests went \
                        on the wire.",
                    "read_only_is_measured_not_asserted": "the count above walks every trace \
                        file in this directory — both reconstruction passes, both arms of the \
                        §12 experiment, and the §11 probe — not only the ones a table quotes. \
                        §28 asks for signatures, broadcasts and real arbitrage to be zero: a \
                        signed or broadcast transaction would have to appear in one of these \
                        traces as a method outside the read-only set, and the run held no key \
                        and had no broadcast path, so `calls_outside_the_read_only_set` is the \
                        measured form of all three. The per-run sums are the second, stronger \
                        form: a call the scan does not explain makes recorded + dropped \
                        larger than the calls the scan accounts for, whether or not the sink \
                        had room to write its name down.",
                },
                "not_claimed": [
                    "no pool here is claimed tradable — M9.2 ends at GraphSnapshot, and \
                     PathFinder is the next milestone (§30)",
                    "no pool is claimed fresh in the live sense: `state_valid_at_target` means \
                     a scan proved no later Sync up to one historical block, which is not the \
                     same statement as state valid now (§36)",
                    "the fee on every attested pool is still unattested, so the graph prices \
                     edges with `fee: null` exactly as M9.1 left them",
                    "eth_call was not used for market state at any point in this milestone \
                     (§5); the reconstruction asks for logs and nothing else",
                    "no new RPC client, provider, cache, or cross-stage reuse was added (§9), \
                     and no M8 reduction work was reopened",
                ],
                "boundaries": [
                    "Verified != State valid at target",
                    "State valid at target != Graph eligible",
                    "Graph eligible != Tradable opportunity",
                    "Scan covered != Chain idle",
                    "Duplicate != Reusable",
                ],
                "negative_controls": NEGATIVE_CONTROLS
                    .iter()
                    .map(|(id, asks, tests)| json!({"control": id, "asks": asks, "tests": tests}))
                    .collect::<Vec<Value>>(),
                "test_matrix": TEST_MATRIX
                    .iter()
                    .map(|(topic, tests)| json!({"topic": topic, "tests": tests}))
                    .collect::<Vec<Value>>(),
                "limitations": [
                    "tx_index is recorded from the log and never consulted for ordering; the \
                     repo's canonical total order for a replay path is (block number, log \
                     index), proved by crates/chain/src/recorded.rs and re-checked by \
                     sync_reconstruction::a_node_that_answers_in_reverse_order_gives_the_same_selection \
                     (§7, §24)",
                    "the search lower bound is each pool's own discovery block, so a Sync \
                     published before the claim that discovery found is outside the range by \
                     construction — a pool created by a claim this scan never saw would be \
                     reported as `state_unavailable`, not as a state (§8)",
                    "one target block, one chunk width, one node; the numbers in \
                     node-capacity.json belong to that endpoint and expire with it",
                ],
            }),
        )
    }

    fn manifest_table(&self, tables: &BTreeMap<String, String>) -> String {
        let rows: Vec<Value> = {
            let mut names: Vec<&String> = tables.keys().collect();
            names.retain(|name| name.as_str() != MANIFEST);
            names.sort();
            names
                .iter()
                .map(|name| {
                    let text = &tables[*name];
                    json!({
                        "file": name,
                        "bytes": text.len(),
                        "digest": format!("0x{:x}", alloy_primitives::keccak256(text.as_bytes())),
                    })
                })
                .collect()
        };
        let raw_rows: Vec<Value> = raw_record_names()
            .iter()
            .map(|name| {
                let text = read_text(name);
                json!({
                    "file": name,
                    "bytes": text.len(),
                    "lines": if name.ends_with(".jsonl") { text.lines().count() } else { 1 },
                    "digest": format!("0x{:x}", alloy_primitives::keccak256(text.as_bytes())),
                })
            })
            .collect();
        // Nothing in `raw/` may sit outside this list: §23 asks a reviewer to recompute
        // every aggregate from the raw records, which is only true if the manifest names
        // all of them. Dotfiles are skipped because a file manager's bookkeeping is not a
        // record of the run.
        let mut on_disk: Vec<String> = std::fs::read_dir(evidence_dir().join("raw"))
            .unwrap_or_else(|err| panic!("{}: {err}", EVIDENCE_REL))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file())
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().to_string();
                (!name.starts_with('.')).then(|| format!("raw/{name}"))
            })
            .collect();
        on_disk.sort();
        let unlisted: Vec<&String> = on_disk
            .iter()
            .filter(|name| {
                !raw_rows
                    .iter()
                    .any(|row| row["file"].as_str() == Some(name.as_str()))
            })
            .collect();
        assert!(
            unlisted.is_empty(),
            "{} are raw records of this run that the manifest does not name",
            unlisted
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<&str>>()
                .join(", ")
        );
        // The same rule one level up, for the tables themselves: a file a reviewer finds in
        // this directory that this gate no longer produces is a stale claim about the run.
        let stale: Vec<String> = std::fs::read_dir(evidence_dir())
            .unwrap_or_else(|err| panic!("{}: {err}", EVIDENCE_REL))
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_file())
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().to_string();
                (!name.starts_with('.') && !TABLES.contains(&name.as_str())).then_some(name)
            })
            .collect();
        assert!(
            stale.is_empty(),
            "{} committed but not produced by this gate — a table the run no longer builds \
             cannot be evidence of the run",
            stale.join(", ")
        );
        let questions = [
            "How many pools did M9.1 verify, and what is each one's state at block \
             {target}? — reconstruction.json, {pools} rows",
            "Which Sync was selected for each, and what proved nothing came after it? \
             — sync-events.json",
            "How many of them entered the target block's graph, and why did the rest \
             not? — graph-integration.json, rejected-pools.json",
            "What did the reads cost, and which query strategy won? — rpc-calls.json, \
             strategy-comparison.json, node-capacity.json",
            "Is the same input reproducible? — summary.json#determinism, and \
             `committed_tables_match_a_rebuild_from_the_raw_records` in this file",
        ]
        .iter()
        .map(|question| question.replace("{target}", &self.target.to_string()))
        .map(|question| question.replace("{pools}", &self.a.reconstruction.rows.len().to_string()))
        .collect::<Vec<String>>();
        pretty(
            "manifest",
            json!({
                "_provenance": self.provenance(),
                "table": "manifest",
                "files": rows,
                "raw_files": raw_rows,
                "questions_answered": questions,
                "negative_controls_run_here": "the §19 controls are tests, not rows: \
                                              crates/discovery/tests/sync_reconstruction.rs \
                                              (34 tests over scan, selection and the graph \
                                              door) and crates/graph/tests/state_at_target.rs \
                                              (9 tests over the snapshot projection). \
                                              summary.json#negative_controls maps each NC to \
                                              the tests that carry it, and \
                                              `every_negative_control_names_a_test_that_runs` \
                                              reads those names out of the source files.",
            }),
        )
    }

    fn readme(&self, tables: &BTreeMap<String, String>) -> String {
        let rows = self.reconstruction_rows();
        let valid = rows
            .iter()
            .filter(|row| row["state_valid_at_target"].as_bool().expect("a bool"))
            .count();
        let build = self.a.state.graph.build();
        let skipped = skipped_of(&self.a.state);
        // The README quotes the same per-run accounting the tables publish, so the sentence
        // about how much of each run reached the trace files cannot drift from the rows.
        let caps = self.call_accounts();
        let cap = caps
            .first()
            .and_then(|row| row["cap_per_sink"].as_u64())
            .expect("the cap every accounting row carries");
        let beyond_caps: u64 = caps
            .iter()
            .map(|row| {
                row["events_dropped_beyond_the_cap"]
                    .as_u64()
                    .expect("a dropped count")
            })
            .sum();
        format!(
            "# M9.2 证据目录（V2 历史市场状态重建 + 目标块新图）\n\n\
             ## 一句话（白话版）\n\n\
             M9.1 验出 {m91_attested} 个池子，但只有 {m91_graph} 个能进图：因为图只肯用「和它\n\
             同一个块的价格」，其余 {m91_skipped} 个池子的价格来自更早的块。M9.2 做的事，是去链上把每个\n\
             池子在目标块之前**最后一次真实报价**找回来，并且证明那之后到目标块之间它没有再报过价\n\
             ——于是图还是那张图，规则一个字没改，只是终于拿到了它能接受的输入。\n\n\
             ## 这次到底证明了什么\n\n\
             | 问题 | 数字 | 逐条证据 |\n|---|---|---|\n\
             | M9.1 验过的池子 | {verified} | 输入是 `{input}`，未重新发现 |\n\
             | 目标块 {target} 上状态可重建 | {valid} | `reconstruction.json`，每池一行 |\n\
             | 目标块上没有任何权威 Sync | {unavailable} | `rejected-pools.json` |\n\
             | 有 Sync 但某侧储备为 0 | {invalid} | `rejected-pools.json` |\n\
             | 最终进入目标块图 | {graph} | `graph-integration.json` |\n\
             | 被图跳过（每个恰好一个原因） | {skipped} | `graph-integration.json#skipped` |\n\n\
             两个等式必须成立：`{verified} = {valid} + {unavailable} + {invalid}`（§21），\n\
             `{graph} + {skipped} = {attested}`（§22）。它们由这个目录自己算出来，不是抄来的。\n\n\
             ## 为什么要分两种口径讲「状态」\n\n\
             图里的边带着它自己的时间戳：价格出自哪个块、哪条日志。这一点在 M9.2 之后仍然成立——\n\
             目标块是「这张图算哪一块」，最后报价位置是「这个数从哪来」，两个数各记各的，\n\
             谁也不许覆盖谁（§14「不许销毁来源」）。所以「在 {target} 有效」**不等于**「价格是 {target} 报的」，\n\
             它等于「{target} 之前最后一次报价是 X，并且我们扫过了 X 到 {target} 之间所有块，没有更新的报价」。\n\n\
             ## 表格清单\n\n{listing}\n\
             ## 原始记录\n\n\
             `raw/` 下每一次实跑的文档与 RPC 轨迹都在这里，`manifest.json#raw_files` 逐个列字节数与摘要。\n\
             实跑命令（§11 探针 → §12 两臂对照 → §20/§27 目标块两把）：\n\n\
             ```text\n\
             GIWA_RPC_URL=<endpoint from the environment> M92_STRATEGY=<pool|census> M92_TARGET={target} \\\n\
             cargo test -p evm-discovery --test sync_reconstruction_live -- \\\n\
             --ignored --nocapture --test-threads=1 <target name>\n\
             ```\n\n\
             ## 重算与门禁\n\n\
             ```text\n\
             M92_EVIDENCE_REFRESH=1 cargo test -p evm-discovery --test reconstruction_evidence_gate -- --test-threads=1\n\
             cargo test -p evm-discovery --test reconstruction_evidence_gate -- --test-threads=1\n\
             ```\n\n\
             第一条重建并写入；第二条只读比对，任何一格对不上就失败。此外\n\
             `the_independent_recompute_agrees_with_every_committed_number` 不调用任何 discovery\n\
             函数，直接按原始字段重算；`an_injected_wrong_number_is_caught_by_the_recompute` 往原始\n\
             记录里塞一个错数，确认那道门真的会红——只有前一道门绿的目录不能算被验过。\n\n\
             ## 这个目录没有说的事\n\n\
             - 没有任何池子被声称「可交易」：M9.2 到 GraphSnapshot 为止，寻路是下一个里程碑（§30）。\n\
             - 没有任何池子被声称「新鲜」：这里证明的是**历史块**上的状态，不是当前状态（§36）。\n\
             - 费率依旧全未举证（`fee: null`），和 M9.1 留下时一样。\n\
             - 全程只读：`eth_getLogs` / `eth_chainId`，签名 0、广播 0、真实套利 0（§28）。这条\n\
               不是自我声明：`rpc-calls.json#calls_outside_the_read_only_set` 是把 `raw/` 下\n\
               五份轨迹逐行数出来的，`manifest.json#raw_files` 列全了那五份。一个 sink 只留\n\
               {cap} 条轨迹、其余只计数不落地，所以「逐行数出来」覆盖的是轨迹里的调用；每个跑\n\
               自己记三个数——落地的行数、超出上限被计数没落地的次数、扫描按块区间本身该发多少次\n\
               请求——判据是「落地 + 计数 = 请求 + 一次链 ID」（\n\
               `summary.json#rpc.per_run_call_accounting`，四个跑一行一条，本次合计被计数而未\n\
               落地的调用 {beyond_caps} 次）。一次扫描解释不了的调用，无论有没有被写进轨迹，都会\n\
               让这个和对不上。\n\
             - 端点在本目录只以摘要 `{endpoint}` 出现，URL 字面量一个都没有（有测试逐文件检查）。\n",
            verified = self.verified.len(),
            m91_attested = self.m91.attested,
            m91_graph = self.m91.graph_pools,
            m91_skipped = self.m91.skipped,
            target = self.target,
            valid = valid,
            unavailable = self.a.raw.state_unavailable,
            invalid = self.a.raw.state_invalid,
            graph = build.map_or(0, |build| build.graph.pool_count()),
            skipped = skipped.len(),
            attested = self.a.state.attested.len(),
            input = repo_relative_input(&self.a.raw.pools_input_source),
            endpoint = self.endpoint,
            listing = tables
                .iter()
                .filter(|(name, _)| name.as_str() != README)
                .map(|(name, text)| format!(
                    "- `{name}` — {} 字节\n",
                    text.len()
                ))
                .collect::<String>(),
        )
    }

    fn tables(&self) -> BTreeMap<String, String> {
        let mut tables = BTreeMap::new();
        tables.insert(SUMMARY.to_string(), self.summary_table());
        tables.insert(RECONSTRUCTION.to_string(), self.reconstruction_table());
        tables.insert(SYNC_EVENTS.to_string(), self.sync_events_table());
        tables.insert(INTEGRATION.to_string(), self.graph_table());
        tables.insert(RPC_TABLE.to_string(), self.rpc_table());
        tables.insert(UNAVAILABLE_TABLE.to_string(), self.unavailable_table());
        tables.insert(STRATEGY_TABLE.to_string(), self.strategy_table());
        tables.insert(CAPACITY_TABLE.to_string(), self.capacity_table());
        let manifest = self.manifest_table(&tables);
        tables.insert(MANIFEST.to_string(), manifest.clone());
        let mut for_readme = tables.clone();
        for_readme.insert(MANIFEST.to_string(), manifest);
        let readme = self.readme(&for_readme);
        tables.insert(README.to_string(), readme);
        tables
    }

    /// Write in refresh mode; in gate mode the caller compares against the bytes.
    fn commit(&self, tables: &BTreeMap<String, String>) {
        if !refresh() {
            return;
        }
        let directory = evidence_dir();
        std::fs::create_dir_all(&directory).expect("the evidence directory exists");
        for (name, text) in tables {
            std::fs::write(directory.join(name), text)
                .unwrap_or_else(|err| panic!("write {name}: {err}"));
        }
    }
}

/// The one JSON shape every table shares: two-space pretty printing with a trailing
/// newline, so a byte comparison is a comparison of content rather than of a formatter's
/// mood.
fn pretty(table: &str, value: Value) -> String {
    assert_eq!(
        value["table"].as_str(),
        Some(table),
        "{table}: the document's own `table` field disagrees with its file name"
    );
    let mut text = serde_json::to_string_pretty(&value).expect("a table serializes");
    text.push('\n');
    text
}

// ---------------------------------------------------------------------------
// the independent recompute — no discovery function is called below this line
// ---------------------------------------------------------------------------

/// `serde_json` reads a number as `u64`; these tables count rows, so the recompute needs
/// the widening in one place rather than eleven `as u64 as usize` chains.
trait AsCount {
    fn as_usize(&self) -> Option<usize>;
}

impl AsCount for Value {
    fn as_usize(&self) -> Option<usize> {
        self.as_u64().map(|count| count as usize)
    }
}

/// Recompute every headline number from the raw documents as untyped JSON, and return
/// each disagreement as a sentence. The point of returning rather than asserting is the
/// negative control: a tampered copy can be run through the same function and must
/// produce exactly the failure it deserves.
fn recompute_mismatches(
    pass: &Value,
    reconstruction: &Value,
    integration: &Value,
    unavailable: &Value,
    summary: &Value,
    sync_events: &Value,
) -> Vec<String> {
    let mut mismatches = Vec::new();
    let rows = pass["rows"]
        .as_array()
        .expect("the raw pass carries its rows as an array");
    let target = pass["target"]
        .as_u64()
        .expect("the raw pass carries a target block");

    // The classes, derived from the row fields with no reference to the model.
    let mut valid = 0usize;
    let mut unavailable_count = 0usize;
    let mut invalid = 0usize;
    let mut seen_keys: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let selected = row.get("selected").filter(|value| !value.is_null());
        let key = row_key_of(row, target);
        if !seen_keys.insert(key.clone()) {
            mismatches.push(format!(
                "two raw rows carry the same identity {key}, so no number in this directory \
                 can be attributed to one pool"
            ));
        }
        if let Some(selected) = selected {
            let reserve0 = selected["reserve0"].as_str().unwrap_or_default();
            let reserve1 = selected["reserve1"].as_str().unwrap_or_default();
            let empty = reserve0 == "0x0" || reserve1 == "0x0";
            let block = selected["block_number"].as_u64().expect("a sync block");
            let search_from = row["search_from"].as_u64().expect("a scan floor");
            let search_to = row["search_to"].as_u64().expect("a scan ceiling");
            if block > target {
                mismatches.push(format!(
                    "row {key}: a Sync at block {block} is after the target {target}, so it \
                     cannot be state valid at the target (§19 NC1/NC5)"
                ));
            } else if search_to < target || block < search_from {
                mismatches.push(format!(
                    "row {key}: the scan covered {search_from}..={search_to}, which does not \
                     reach the target {target} from the Sync at {block} — the gap is not \
                     covered, so the pool cannot be called valid (§25)"
                ));
            } else if empty {
                invalid += 1;
            } else {
                valid += 1;
            }
        } else {
            unavailable_count += 1;
        }
    }

    // One line per disagreement, so a table that reports a count the raw rows cannot produce
    // is named with both numbers rather than just failing.
    macro_rules! check {
        ($name:expr, $expected:expr, $found:expr, $table:expr $(,)?) => {
            match $found {
                Some(found) if found == $expected => {}
                found => mismatches.push(format!(
                    "{} is {} from the raw rows but {:?} in {}",
                    $name, $expected, found, $table
                )),
            }
        };
    }
    check!(
        "target_state_valid",
        valid,
        reconstruction["by_reason"]["target_state_valid"].as_usize(),
        RECONSTRUCTION,
    );
    check!(
        "state_unavailable",
        unavailable_count,
        reconstruction["by_reason"]["state_unavailable"].as_usize(),
        RECONSTRUCTION,
    );
    check!(
        "state_invalid",
        invalid,
        reconstruction["by_reason"]["state_invalid"].as_usize(),
        RECONSTRUCTION,
    );
    check!(
        "pools",
        rows.len(),
        reconstruction["pools"].as_usize(),
        RECONSTRUCTION,
    );

    // The identities the reconstruction table prints have to be the identities the raw
    // records imply. A reader diffs two runs by this string (§27), so a label no outside
    // evidence agrees with is worse than no label.
    let derived: BTreeSet<String> = rows.iter().map(|row| row_key_of(row, target)).collect();
    let printed: BTreeSet<String> = reconstruction["rows"]
        .as_array()
        .expect("a row array")
        .iter()
        .map(|row| {
            row["identity"]
                .as_str()
                .expect("a printed identity")
                .to_lowercase()
        })
        .collect();
    if derived != printed {
        let only_derived = derived.difference(&printed).next();
        let only_printed = printed.difference(&derived).next();
        mismatches.push(format!(
            "{RECONSTRUCTION} prints {} row identities and the raw pass implies {}, and they \
             are not the same set (raw-only: {only_derived:?}, table-only: {only_printed:?})",
            printed.len(),
            derived.len()
        ));
    }
    check!(
        "target_state_valid (summary)",
        valid,
        summary["state_coverage"]["target_state_valid"].as_usize(),
        SUMMARY,
    );
    check!(
        "state_unavailable (summary)",
        unavailable_count,
        summary["state_coverage"]["state_unavailable"].as_usize(),
        SUMMARY,
    );
    check!(
        "state_invalid (summary)",
        invalid,
        summary["state_coverage"]["state_invalid"].as_usize(),
        SUMMARY,
    );
    check!(
        "verified (summary)",
        rows.len(),
        summary["state_coverage"]["verified"].as_usize(),
        SUMMARY,
    );

    // The table of rows that could not be priced must carry exactly the rows the
    // derivation put outside `target_state_valid`.
    let table_rows = unavailable["rows"].as_array().expect("a row array");
    check!(
        "rejected-pools rows",
        unavailable_count + invalid,
        Some(table_rows.len()),
        UNAVAILABLE_TABLE,
    );
    for row in table_rows {
        if row["state_valid_at_target"].as_bool() == Some(true) {
            mismatches.push(format!(
                "a pool that prices the target is sitting in {UNAVAILABLE_TABLE}"
            ));
        }
    }

    // §21 and §22 as arithmetic, not as prose.
    let graph = integration["graph_pool_count"]
        .as_usize()
        .unwrap_or_default();
    let skipped = integration["skipped"]
        .as_array()
        .map(|rows| rows.len())
        .unwrap_or_default();
    let attested = integration["attested"].as_usize().unwrap_or_default();
    if graph + skipped != attested {
        mismatches.push(format!(
            "§22 does not add up in {INTEGRATION}: {graph} graph pools + {skipped} skipped \
             is not the {attested} attested pools"
        ));
    }
    if valid + unavailable_count + invalid != rows.len() {
        mismatches.push(format!(
            "§21 does not add up in the raw rows: {valid} + {unavailable_count} + {invalid} \
             is not {pools} pools",
            pools = rows.len()
        ));
    }

    // One selected Sync per priced pool, and nothing else in the events table.
    check!(
        "selected sync events",
        valid + invalid,
        sync_events["selected_events"].as_usize(),
        SYNC_EVENTS,
    );
    let events = sync_events["rows"].as_array().expect("a row array");
    let selected_in_events: BTreeSet<String> =
        events.iter().map(|row| row_key_of(row, target)).collect();
    let selected_in_rows: BTreeSet<String> = rows
        .iter()
        .filter(|row| row.get("selected").is_some_and(|value| !value.is_null()))
        .map(|row| row_key_of(row, target))
        .collect();
    if selected_in_events != selected_in_rows {
        mismatches.push(format!(
            "{SYNC_EVENTS} and the raw pass disagree about which Syncs were selected: {} vs {}",
            selected_in_events.len(),
            selected_in_rows.len()
        ));
    }

    // §25's staleness figures, rebuilt from this table's own rows instead of echoed back. A
    // row's span has to equal the gap between its own two blocks, and the summary has to be
    // the arithmetic of the sorted spans — `median` taken from the row order would be the
    // staleness of whichever pool sorts to the middle, which is a pool id, not a percentile.
    let mut spans: Vec<u64> = Vec::new();
    for row in events {
        let sync_block = row["block_number"].as_u64().expect("a sync block");
        let stated = row["blocks_between_state_and_target"]
            .as_u64()
            .expect("a per-pool span");
        if stated != target.saturating_sub(sync_block) {
            mismatches.push(format!(
                "{SYNC_EVENTS} calls one pool {stated} blocks behind the target while its own \
                 sync block {sync_block} and the target {target} differ by {}",
                target.saturating_sub(sync_block),
            ));
        }
        spans.push(stated);
    }
    spans.sort_unstable();
    let behind = &sync_events["blocks_behind_target"];
    check!(
        "minimum span",
        *spans.first().expect("a span"),
        behind["min"].as_u64(),
        SYNC_EVENTS
    );
    check!(
        "maximum span",
        *spans.last().expect("a span"),
        behind["max"].as_u64(),
        SYNC_EVENTS
    );
    check!(
        "median span",
        spans[spans.len() / 2],
        behind["median"].as_u64(),
        SYNC_EVENTS,
    );
    check!(
        "total span",
        spans.iter().sum::<u64>(),
        behind["sum"].as_u64(),
        SYNC_EVENTS
    );
    check!(
        "spanned pools",
        spans.len(),
        behind["n"].as_usize(),
        SYNC_EVENTS
    );

    // Every edge's state position must be at or before the target, and the graph must be
    // the target's — §15 and §26, read off the committed bytes.
    let block = integration["graph_block"].as_u64();
    if block != Some(target) {
        mismatches.push(format!(
            "the graph in {INTEGRATION} is taken at {block:?}, not at the target {target}"
        ));
    }
    for edge in integration["edges"].as_array().expect("an edge array") {
        let state_block = edge["state_position"]["block_number"]
            .as_u64()
            .expect("an edge names its state position");
        if state_block > target {
            mismatches.push(format!(
                "an edge priced from block {state_block} sits in the graph of block {target}"
            ));
        }
    }
    if integration["target_block"].as_u64() != Some(target) {
        mismatches.push(format!(
            "{INTEGRATION} declares its target as {:?}, which is not the pass target {target}",
            integration["target_block"]
        ));
    }
    mismatches
}

/// The same key the tables print, computed from untyped JSON so the recompute does not
/// depend on the type it is checking. The two spellings of a row are handled separately
/// and completely: a raw pass row serialises the model, so its chain id lives inside
/// `pool` and its selection lives under `selected`; a committed table row flattens both,
/// and the events table carries the chain position as its own fields. Every part of the
/// key has to be named by the row that supplies it — a missing part is a failure, never
/// a default, because a key with a hole in it quietly fuses two findings into one.
fn row_key_of(row: &Value, target: u64) -> String {
    fn position(block: &Value, tx: &Value, log: &Value) -> String {
        format!(
            "{}/{}/{}",
            block.as_u64().expect("a sync block number in every row"),
            tx.as_u64().expect("a sync tx index in every row"),
            log.as_u64().expect("a sync log index in every row")
        )
    }
    let (chain, pool, tail) = match &row["pool"] {
        Value::Object(pool_id) => (
            pool_id["chain_id"]
                .as_u64()
                .expect("a chain id in every row"),
            pool_id["address"]
                .as_str()
                .expect("a pool address in every row")
                .to_string(),
            match row.get("selected").filter(|value| !value.is_null()) {
                Some(sync) => {
                    position(&sync["block_number"], &sync["tx_index"], &sync["log_index"])
                }
                None => "none".to_string(),
            },
        ),
        Value::String(address) => {
            let tail = if row.get("block_number").is_some() {
                position(&row["block_number"], &row["tx_index"], &row["log_index"])
            } else if row["selected_sync_block"].is_null() {
                "none".to_string()
            } else {
                position(
                    &row["selected_sync_block"],
                    &row["selected_sync_tx_index"],
                    &row["selected_sync_log_index"],
                )
            };
            (
                row["chain_id"].as_u64().expect("a chain id in every row"),
                address.to_string(),
                tail,
            )
        }
        other => panic!("a row names its pool as an address or as a pool id, not {other}"),
    };
    // The tables print `pool` in the address's lowercase form and its identity in the
    // checksummed form; both are the same pool, so the comparison is case-folded rather
    // than pretended apart.
    format!("{chain}/{pool}/{target}/{tail}").to_lowercase()
}

// ---------------------------------------------------------------------------
// the gates
// ---------------------------------------------------------------------------

/// Load the raw records, rebuild the pipeline, write (refresh mode) or byte-gate
/// (default mode) every committed table.
#[test]
fn committed_tables_match_a_rebuild_from_the_raw_records() {
    let case = Case::load();
    let tables = case.tables();
    case.commit(&tables);
    for (name, text) in &tables {
        let committed = std::fs::read_to_string(evidence_dir().join(name))
            .unwrap_or_else(|err| panic!("{}: {err}", missing(name)));
        assert_eq!(
            text, &committed,
            "{name} is not what the raw records rebuild to. Re-assemble with \
             M92_EVIDENCE_REFRESH=1 if the raw records legitimately changed.",
        );
    }
}

/// The independent half: no discovery function is called here at all.
#[test]
fn the_independent_recompute_agrees_with_every_committed_number() {
    let pass: Value = read_json_value(PASS_A);
    let reconstruction: Value = read_json_value(RECONSTRUCTION);
    let integration: Value = read_json_value(INTEGRATION);
    let unavailable: Value = read_json_value(UNAVAILABLE_TABLE);
    let summary: Value = read_json_value(SUMMARY);
    let sync_events: Value = read_json_value(SYNC_EVENTS);
    let mismatches = recompute_mismatches(
        &pass,
        &reconstruction,
        &integration,
        &unavailable,
        &summary,
        &sync_events,
    );
    assert!(
        mismatches.is_empty(),
        "the raw records and the committed tables disagree:\n{}",
        mismatches.join("\n")
    );
}

/// The recompute has to be able to fail. Every tampering below changes one number that a
/// reader would otherwise take on trust, and each must produce exactly the complaint the
/// directory deserves — a gate that never turns red on a wrong number is a formatter.
#[test]
fn an_injected_wrong_number_is_caught_by_the_recompute() {
    let pass = read_json_value(PASS_A);
    let real = (
        read_json_value(RECONSTRUCTION),
        read_json_value(INTEGRATION),
        read_json_value(UNAVAILABLE_TABLE),
        read_json_value(SUMMARY),
        read_json_value(SYNC_EVENTS),
    );
    // 1. The summary claims one more valid pool than the rows support.
    let mut tampered = real.3.clone();
    let claimed = tampered["state_coverage"]["target_state_valid"]
        .as_u64()
        .expect("a count")
        + 1;
    tampered["state_coverage"]["target_state_valid"] = json!(claimed);
    let found = recompute_mismatches(&pass, &real.0, &real.1, &real.2, &tampered, &real.4);
    assert!(
        found
            .iter()
            .any(|line| line.contains("target_state_valid (summary)")),
        "a summary that overstates valid state by one passed the recompute: {found:?}"
    );

    // 2. The graph table claims one more pool in the graph than its own accounting can
    //    produce (§22). This tampers the graph count rather than deleting a skip row: a run
    //    that put every pool in the graph has no skip row to delete, and a control that only
    //    exists when the data happens to cooperate controls nothing.
    let mut tampered = real.1.clone();
    let graph = tampered["graph_pool_count"]
        .as_u64()
        .expect("a graph pool count");
    tampered["graph_pool_count"] = json!(graph + 1);
    let found = recompute_mismatches(&pass, &real.0, &tampered, &real.2, &real.3, &real.4);
    assert!(
        found
            .iter()
            .any(|line| line.contains("§22 does not add up")),
        "a graph table that claims {graph}+1 pools passed the recompute: {found:?}"
    );

    // 3. A row's Scan reaches only half way to the target and the table still calls it valid.
    let mut tampered_pass = pass.clone();
    let rows = tampered_pass["rows"]
        .as_array()
        .expect("a row array")
        .clone();
    assert!(
        rows.iter().any(|row| !row["selected"].is_null()),
        "this control needs at least one pool with a selected Sync in the run"
    );
    let target = tampered_pass["target"].as_u64().expect("a target");
    let mut tampered_rows = rows.clone();
    for row in &mut tampered_rows {
        if !row["selected"].is_null() {
            row["selected"]["block_number"] = json!(target + 1);
            break;
        }
    }
    tampered_pass["rows"] = json!(tampered_rows);
    let found = recompute_mismatches(&tampered_pass, &real.0, &real.1, &real.2, &real.3, &real.4);
    assert!(
        found
            .iter()
            .any(|line| line.contains("is after the target")),
        "a Sync after the target passed as state at the target: {found:?}"
    );

    // 4. An edge from after the target sits in the graph.
    let mut tampered = real.1.clone();
    let mut edges = tampered["edges"].as_array().expect("an edge array").clone();
    assert!(
        !edges.is_empty(),
        "this control needs at least one edge in the committed graph"
    );
    let edge = edges[0].clone();
    let block = edge["state_position"]["block_number"]
        .as_u64()
        .expect("a position");
    let mut bad_edge = edge;
    bad_edge["state_position"]["block_number"] = json!(target + 1);
    edges[0] = bad_edge;
    tampered["edges"] = json!(edges);
    let found = recompute_mismatches(&pass, &real.0, &tampered, &real.2, &real.3, &real.4);
    assert!(
        found.iter().any(|line| line.contains("priced from block")),
        "an edge from block {} in the graph of block {target} passed: {found:?}",
        block + 1,
    );
    assert!(
        block <= target,
        "the control's own premise is wrong: the real edge is already past the target"
    );

    // 5. The staleness median read from the row order instead of the sorted spans. This
    //    reproduces the defect the sort exists to prevent rather than inventing a wrong
    //    number: the middle of a pool-id ordering is some pool's span, and it still prints as
    //    a percentile.
    let mut tampered = real.4.clone();
    let ordered: Vec<u64> = real.4["rows"]
        .as_array()
        .expect("a row array")
        .iter()
        .map(|row| {
            row["blocks_between_state_and_target"]
                .as_u64()
                .expect("a span")
        })
        .collect();
    let mut sorted = ordered.clone();
    sorted.sort_unstable();
    let by_row_order = ordered[ordered.len() / 2];
    let by_value = sorted[sorted.len() / 2];
    assert_ne!(
        by_row_order, by_value,
        "this control only bites when the two middles differ; on this data they are both \
         {by_value}, so it would prove nothing"
    );
    tampered["blocks_behind_target"]["median"] = json!(by_row_order);
    let found = recompute_mismatches(&pass, &real.0, &real.1, &real.2, &real.3, &tampered);
    assert!(
        found.iter().any(|line| line.contains("median span")),
        "a median of {by_row_order} passed while the sorted middle of these spans is {by_value}: \
         {found:?}"
    );
}

fn read_json_value(relative: &str) -> Value {
    serde_json::from_str(&read_text(relative))
        .unwrap_or_else(|err| panic!("{} does not parse: {err}", relative))
}

/// §28: nothing in this directory was broadcast, and nothing was signed. The trace is the
/// witness: a method outside the read-only set is a failure of this test, not a note.
///
/// The second half of §28's promise — that no historical read was answered by `latest` —
/// cannot come from these lines, because the sink stamps a block only for methods that take
/// a block *tag*, and a log query is not one of them (`eth_getLogs` lines carry
/// `block: null`). It comes from the ranges the rows record and from the type that reaches
/// the wire, which is what the two asserts at the end of this test check.
/// §12's arms were measured at one chunk width and the target run may scan at another, so
/// the width that bought the cheaper pass has to be shown not to have bought a different
/// answer. This compares the two documents' selections by the same semantic key §12's
/// arm-to-arm comparison uses — across chunk widths instead of across filter shapes — and
/// then the two reserves each selection published, because a chain position is the same
/// state only if the numbers read off it are the same too.
#[test]
fn the_target_run_selects_the_same_sync_as_the_experiment_arm() {
    let case = Case::load();
    let keys = |rows: &[PoolSyncAtTarget]| -> Vec<String> {
        let mut keys: Vec<String> = rows.iter().map(row_key).collect();
        keys.sort();
        keys
    };
    let reserves = |rows: &[PoolSyncAtTarget]| -> BTreeMap<String, String> {
        rows.iter()
            .map(|row| {
                let published = row
                    .selected
                    .as_ref()
                    .map(|sync| format!("{},{}", sync.reserve0, sync.reserve1))
                    .unwrap_or_else(|| "none".to_string());
                (row_key(row), published)
            })
            .collect()
    };
    assert_eq!(
        keys(&case.a.reconstruction.rows),
        keys(&case.pool_arm.rows),
        "the target run scanned at {} blocks per chunk and the §12 per-pool arm at {}, and the \
         two disagree about which `Sync` prices a pool: the wider scan changed the answer, not \
         just the cost, so it cannot be used as a cheaper way to buy the same reconstruction \
         (§12 gives correctness priority over the request count)",
        case.a.raw.chunk_blocks,
        case.pool_arm.chunk_blocks,
    );
    assert_eq!(
        reserves(&case.a.reconstruction.rows),
        reserves(&case.pool_arm.rows),
        "the two documents selected the same chain positions but different reserves, which is \
         a decoded-record difference between two reads of the same log"
    );
}

#[test]
fn every_recorded_call_is_a_read_and_the_run_asked_for_nothing_else() {
    let calls = traced_calls();
    assert!(
        !calls.is_empty(),
        "not one trace file in this directory could be read"
    );
    for (relative, line) in &calls {
        let method = line["method"].as_str().expect("a method");
        assert!(
            READ_ONLY_METHODS.contains(&method),
            "{relative}: {method} is not a read this milestone is allowed to make"
        );
        if let Some(block) = line["block"].as_str() {
            assert!(
                block.chars().all(|c| c.is_ascii_digit()),
                "{relative}: a block argument written as {block} is a tag, and a tag is not a \
                 pinned height (§10)"
            );
        }
    }
    // The same list the tables quote, so a published count cannot describe a different set
    // of files from the one this test just walked.
    let case = Case::load();
    assert_eq!(
        case.calls_read + case.calls_not_read,
        calls.len(),
        "the tables count {} traced calls and this test found {}",
        case.calls_read + case.calls_not_read,
        calls.len()
    );
    assert_eq!(
        case.calls_not_read, 0,
        "{} of the traced calls are outside the read-only set",
        case.calls_not_read
    );

    // §10, first half, from the rows: every block the run says it read is numbered, at or
    // below the target it reconstructed, and not above the block the pool was seen in.
    for raw in [&case.a.raw, &case.b.raw] {
        let sum: u64 = raw
            .rows
            .iter()
            .map(|row| row.search_to.0 - row.search_from.0 + 1)
            .sum();
        assert_eq!(
            sum, raw.blocks_scanned,
            "pass {}: the rows cover {sum} blocks and the run claims {}",
            raw.pass, raw.blocks_scanned
        );
        for row in &raw.rows {
            assert_eq!(
                row.search_to, row.target,
                "pass {}: a scan that stopped short of the target cannot prove the target's \
                 state is unchanged",
                raw.pass
            );
        }
        // §8's bound, in the shape the strategy actually has. A per-pool walk is told each
        // pool's own discovery block and may not start below it; a chain-wide pass is told
        // one shared range whose floor is the earliest discovery block in the asked set, so
        // its rows legitimately start above or below an individual pool's own bound but never
        // below the set's. Both are numbers, never a tag, and neither may begin at genesis.
        assert!(
            matches!(raw.strategy.as_str(), "pool" | "census"),
            "pass {}: `{}` is not one of the two strategies §12 compares",
            raw.pass,
            raw.strategy
        );
        let earliest_in_the_asked_set = raw
            .rows
            .iter()
            .map(|row| row.discovery_block.0)
            .min()
            .expect("a pass that asked about no pool has no rows to check");
        assert_eq!(
            raw.earliest_discovery_block, earliest_in_the_asked_set,
            "pass {}: the run says its lowest pool bound is {} and the rows it wrote start at \
             {} — one of the two is not the input this scan was given",
            raw.pass, raw.earliest_discovery_block, earliest_in_the_asked_set
        );
        if raw.strategy == "pool" {
            for row in &raw.rows {
                assert!(
                    row.search_from >= row.discovery_block,
                    "pass {}: a per-pool scan of {} started at {}, below that pool's own \
                     discovery block {} — it re-reads history the pool does not own (§8)",
                    raw.pass,
                    row.pool.address,
                    row.search_from.0,
                    row.discovery_block.0
                );
            }
        } else {
            for row in &raw.rows {
                assert_eq!(
                    row.search_from.0, raw.earliest_discovery_block,
                    "pass {}: a chain-wide pass reads one shared range, so every row must name \
                     the same `search_from` ({}) and this row names {}",
                    raw.pass, raw.earliest_discovery_block, row.search_from.0
                );
            }
        }
    }
    // §10, second half, from the code that writes the request. A check on the only two
    // places a block argument is turned into JSON: a numeric type in, a hex number out.
    let rpc = source_text("crates/chain/src/rpc.rs");
    assert!(
        rpc.contains("fn block_param(number: BlockNumber) -> String {")
            && rpc.contains("format!(\"{:#x}\", number.0)"),
        "crates/chain/src/rpc.rs no longer formats a block argument as a number out of a \
         `BlockNumber`; if a tag can reach the wire from here, this gate has to be rewritten \
         to read the ranges from the trace instead"
    );
    assert!(
        source_text("crates/chain/src/types.rs").contains("pub struct LogFilter {"),
        "LogFilter is where a log query's range is typed"
    );
}

/// §28 at the call layer, for every run this directory commits evidence for: the two §20
/// passes and whichever §12 arms have documents each account for their own calls against
/// their own request counts, and the two passes account for the same number of them.
/// `call_accounting` is where the arithmetic is required; this test is where the runs are
/// named together, so a run added to the directory without its accounting cannot slip past
/// the tables.
#[test]
fn every_run_accounts_for_every_call_it_put_on_the_wire() {
    let case = Case::load();
    let runs = case.call_accounts();
    // The two §20 passes and the per-pool §12 arm are always three runs; a measured
    // chain-wide arm is a fourth. The directory is allowed to carry three when the second arm
    // produced no document, which is the state `strategy-comparison.json` publishes as
    // `decision.cost_assert_active: false` rather than hiding.
    let expected = 3 + usize::from(case.census_arm.is_some());
    assert_eq!(
        runs.len(),
        expected,
        "the two §20 passes, the per-pool §12 arm{} are {} runs, and this directory accounts \
         for {} — a run published without its accounting, or an accounting for a run that \
         committed no trace, makes the tables describe a different set of runs than they walk",
        if case.census_arm.is_some() {
            ", and the chain-wide arm"
        } else {
            ""
        },
        expected,
        runs.len()
    );
    for accounting in &runs {
        let label = accounting["run"].as_str().expect("a run label");
        let cap = accounting["cap_per_sink"].as_u64().expect("a cap");
        let lines = accounting["lines_in_the_trace_file"]
            .as_u64()
            .expect("a count");
        let dropped = accounting["events_dropped_beyond_the_cap"]
            .as_u64()
            .expect("a dropped count");
        assert!(
            lines <= cap,
            "{label}: {lines} trace lines against a per-sink cap of {cap} — a file longer \
             than the sink can hold would mean a different writer than the one audited here"
        );
        assert_eq!(
            accounting["eth_chainId_lines"].as_u64(),
            Some(1),
            "{label}: a run reads the chain id once, because the adapter caches it \
             (`crates/chain/src/adapter.rs:93`); {} chain-id lines mean either a reconnect \
             or a request this gate's arithmetic does not model",
            accounting["eth_chainId_lines"]
        );
        assert_eq!(
            lines + dropped,
            accounting["calls_the_scan_accounts_for"]
                .as_u64()
                .expect("a call count"),
            "{label}: the sums this run reported do not add up"
        );
    }
    // §27 again, one layer down: the two passes walked the same history, so the windows they
    // were built from are the same count. The figure on the wire is not required to be equal,
    // because the harness re-asks a pool the node refused mid-run and a refused attempt is a
    // cost the pass discloses rather than a different reconstruction.
    assert_eq!(
        runs[0]["eth_getlogs_requests_the_rows_were_built_from"],
        runs[1]["eth_getlogs_requests_the_rows_were_built_from"],
        "the two reconstruction passes scanned a different number of windows, so they did not \
         reconstruct the same history (§27). The refusals behind that difference: pass a {}, \
         pass b {}",
        runs[0]["pool_call_refusals"],
        runs[1]["pool_call_refusals"],
    );
    // The §20 run and the §12 arm that chose its strategy are reconciled through the
    // arithmetic rather than through a raw equality of request counts, because they are
    // allowed to run at different chunk widths — a wider window is precisely the saving §11
    // measured. What has to hold at either width is that each row paid for exactly the windows
    // its own answer implies and named the bounds that count produces, over the same input
    // floors. That is a stronger statement than the equality it replaces, and it stays true
    // when the widths differ. The requirement that makes a wider window a cost saving rather
    // than an unverified shortcut is a separate one:
    // [`the_target_run_selects_the_same_sync_as_the_experiment_arm`].
    let want = format!("arm {}", case.strategy);
    assert!(
        runs.iter()
            .any(|row| row["run"].as_str() == Some(want.as_str())),
        "no §12 arm was run with the strategy the target run used (`{}`), so this directory's \
         cost comparison has no measured counterpart",
        case.strategy
    );
    if case.strategy == "pool" {
        let arm = &case.pool_arm;
        assert_eq!(
            arm.strategy, "pool",
            "`{}` names the per-pool arm's document, which describes a `{}` run",
            STRATEGY_POOL, arm.strategy
        );
        for (label, rows, width, asked) in [
            (
                "§12 arm A",
                &arm.rows,
                arm.chunk_blocks,
                arm.eth_getlogs_requests as u64,
            ),
            (
                "pass a",
                &case.a.raw.rows,
                case.a.raw.chunk_blocks,
                case.a.raw.eth_getlogs_requests as u64,
            ),
            (
                "pass b",
                &case.b.raw.rows,
                case.b.raw.chunk_blocks,
                case.b.raw.eth_getlogs_requests as u64,
            ),
        ] {
            assert!(width > 0, "{label}: a chunk width of 0 scans nothing");
            // The cost law the scan implements (`crates/discovery/src/reconstruct.rs:447`):
            // windows are walked backwards from the target and the walk stops at the first
            // window that answers. So a pool's request count is the windows between the target
            // and the `Sync` it selected — or, when nothing answered, the windows down to that
            // pool's own §8 floor — and its lowest scanned block is what that count implies,
            // clipped at the floor. Both are read off the row's own fields, so a row that
            // stopped above its own answer, or paid for windows it did not need, fails here.
            for row in rows {
                let floor = match row.selected {
                    Some(sync) => sync.block_number,
                    None => row.discovery_block,
                };
                let span = row.target.0 - floor.0 + 1;
                assert_eq!(
                    row.chunks_scanned as u64,
                    span.div_ceil(width),
                    "{label}: {}'s answer is at block {}, {} blocks at or below the target \
                     {}, which at a width of {width} is {} windows, and its row claims {}",
                    row.pool.address,
                    floor.0,
                    span,
                    row.target.0,
                    span.div_ceil(width),
                    row.chunks_scanned,
                );
                let low = row
                    .discovery_block
                    .0
                    .max(row.target.0 - row.chunks_scanned as u64 * width + 1);
                assert_eq!(
                    row.search_from.0,
                    low,
                    "{label}: {} claims {} windows of {width} from {}, so the lowest block it \
                     could have scanned is {} (clipped at its floor {}) and the row names {}",
                    row.pool.address,
                    row.chunks_scanned,
                    row.target.0,
                    low,
                    row.discovery_block.0,
                    row.search_from.0,
                );
                assert_eq!(
                    row.search_to, row.target,
                    "{label}: {}'s scan does not reach the target it answers for",
                    row.pool.address
                );
            }
            let sum: u64 = rows.iter().map(|row| row.chunks_scanned as u64).sum();
            assert_eq!(
                sum, asked,
                "{label}: the rows add up to {sum} requests and the run reports {asked}"
            );
        }
        // What the two documents must agree on is their input: the same pools, each with the
        // same §8 floor. Their `search_from` values are NOT required to agree — a wider window
        // leaves a shorter tail under the answer, so the lowest block actually scanned is a
        // function of the width, and that is exactly how one strategy's cost halves.
        let pool_key =
            |row: &PoolSyncAtTarget| format!("{}/{}", row.pool.chain_id.0, row.pool.address);
        let floors: BTreeMap<String, u64> = arm
            .rows
            .iter()
            .map(|row| (pool_key(row), row.discovery_block.0))
            .collect();
        assert_eq!(
            floors.len(),
            arm.rows.len(),
            "the §12 arm's document lists a pool twice, so it has no single floor to compare \
             against"
        );
        for row in &case.a.raw.rows {
            assert_eq!(
                floors.get(&pool_key(row)),
                Some(&row.discovery_block.0),
                "pass a asked about {} from its floor {} and the §12 arm that measured this \
                 strategy did not use that floor for it, so the two runs' costs are not \
                 comparable",
                row.pool.address,
                row.discovery_block.0,
            );
        }
        assert_eq!(
            case.a.raw.rows.len(),
            arm.rows.len(),
            "the §20 pass reconstructed {} pools and the §12 arm measured {}",
            case.a.raw.rows.len(),
            arm.rows.len()
        );
    }
}

/// The endpoint is named as a digest and nowhere else. Written as a scan over the
/// directory rather than as a field check, because the thing being proved is the absence
/// of a string.
#[test]
fn no_committed_file_contains_the_endpoint_url() {
    let census: Value = read_json("data/evidence/m7/census-pair-created.json");
    let url = census["_provenance"]["rpc_endpoint"]
        .as_str()
        .expect("M7's census names its endpoint")
        .to_string();
    for name in TABLES {
        let text = committed_table(name);
        assert!(
            !text.contains(&url),
            "{name} carries the endpoint URL; this directory names it only as its digest"
        );
        assert!(
            !text.contains("https://") && !text.contains("http://"),
            "{name} carries a URL scheme"
        );
    }
    // The raw records are committed too, so they get the same rule as the tables.
    let manifest: Value = read_json_value(MANIFEST);
    for row in manifest["raw_files"]
        .as_array()
        .expect("the manifest lists its raw files")
    {
        let name = row["file"].as_str().expect("a raw file name").to_string();
        let text = read_text(&name);
        assert!(
            !text.contains(&url) && !text.contains("https://") && !text.contains("http://"),
            "{name} carries an endpoint URL; the raw records name it only as its digest"
        );
    }
}

/// Every file in this directory cites other files by their path *inside the repository*, the
/// way M9.1's directory does (it carries no absolute path anywhere). A machine-local prefix is
/// a citation a reviewer on another machine cannot open, and the live writer used to produce
/// one: `sync_reconstruction_live::m91_verified_pools` once recorded the workspace root in
/// front of the input file's name, so the scan covers the raw run records as well as the tables
/// — the tables were already clean, which is exactly why a tables-only check would have missed
/// the defect.
#[test]
fn no_committed_file_cites_an_absolute_machine_path() {
    let absolute = format!("{}/", workspace_root().display());
    let mut offenders = Vec::new();
    for name in TABLES {
        if committed_table(name).contains(&absolute) {
            offenders.push(name.to_string());
        }
    }
    let manifest: Value = read_json_value(MANIFEST);
    for row in manifest["raw_files"]
        .as_array()
        .expect("the manifest lists its raw files")
    {
        let name = row["file"].as_str().expect("a raw file name").to_string();
        if read_text(&name).contains(&absolute) {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "{} cite{} a path rooted at this machine's workspace ({absolute}); every reference \
         in the evidence directory has to be repo-relative so it resolves wherever the repo \
         is checked out",
        offenders.join(", "),
        if offenders.len() == 1 { "s" } else { "" }
    );
}

/// Every table says what its rows are keyed by, and the keys are unique. A directory
/// whose rows can only be told apart by their line number cannot be compared across
/// runs, which is the failure mode §27 names explicitly.
#[test]
fn every_row_identity_is_unique_and_named() {
    for name in [RECONSTRUCTION, UNAVAILABLE_TABLE, SYNC_EVENTS] {
        let table: Value = read_json_value(name);
        assert!(
            table["row_identity"].is_string(),
            "{name} does not say what its rows are keyed by"
        );
        let rows = table["rows"].as_array().expect("a row array");
        let keys: BTreeSet<String> = rows
            .iter()
            .map(|row| {
                row["identity"]
                    .as_str()
                    .expect("a printed identity")
                    .to_string()
            })
            .collect();
        assert_eq!(
            keys.len(),
            rows.len(),
            "{name}: {} rows share an identity, so the table cannot be diffed across runs",
            rows.len()
        );
    }
}

/// §36's four statuses stay four. This reads the committed bytes and refuses a directory
/// that lets a reader collapse them: the same count must not appear as both "verified"
/// and "in the graph" unless the run genuinely put every pool in the graph.
#[test]
fn verified_state_valid_and_graph_eligible_stay_different_claims() {
    let summary: Value = read_json_value(SUMMARY);
    let integration: Value = read_json_value(INTEGRATION);
    let verified = summary["state_coverage"]["verified"]
        .as_u64()
        .expect("a count");
    let valid = summary["state_coverage"]["target_state_valid"]
        .as_u64()
        .expect("a count");
    let graph = integration["graph_pool_count"].as_u64().expect("a count");
    assert_eq!(
        valid
            + summary["state_coverage"]["state_unavailable"]
                .as_u64()
                .expect("a count")
            + summary["state_coverage"]["state_invalid"]
                .as_u64()
                .expect("a count"),
        verified,
        "§21's classes do not cover the verified set"
    );
    assert!(
        graph <= valid,
        "the graph holds {graph} pools but only {valid} have state proven at the target — \
         a pool cannot be more eligible than it is provable (§15)"
    );
    let notes = summary["not_claimed"].as_array().expect("a list");
    assert!(
        notes.len() >= 4,
        "the summary must keep saying what it does not claim, in as many sentences as there \
         are statuses a reader could collapse"
    );
}

/// §19/§31: each control names the tests that carry it, and this checks those tests exist —
/// a control whose test was renamed or deleted is a claim, not a control. This is one of the
/// two gates in the file that need no evidence to run.
#[test]
fn every_negative_control_names_a_test_that_runs() {
    for (id, asks, tests) in NEGATIVE_CONTROLS {
        assert!(!asks.is_empty(), "{id} has no question");
        assert_tests_exist(id, tests);
    }
    let ids: Vec<&str> = NEGATIVE_CONTROLS.iter().map(|(id, _, _)| *id).collect();
    assert_eq!(ids, vec!["NC1", "NC2", "NC3", "NC4", "NC5", "NC6", "NC7"]);
}

/// §31: all thirteen topics the task book asks a focused test for, each naming the tests that
/// answer it, and each name checked against the source files.
#[test]
fn every_test_matrix_topic_names_a_test_that_runs() {
    assert_eq!(
        TEST_MATRIX.len(),
        13,
        "§31 lists thirteen topics; this table has to have exactly one row per topic"
    );
    for (topic, tests) in TEST_MATRIX {
        assert!(!tests.is_empty(), "{topic} names no test");
        assert_tests_exist(topic, tests);
    }
}

/// The committed summary carries the same §31 matrix this file compiles with, for the same
/// reason: a table assembled before a topic moved describes a different suite.
#[test]
fn the_committed_summary_carries_the_test_matrix() {
    let summary: Value = read_json_value(SUMMARY);
    let matrix = summary["test_matrix"]
        .as_array()
        .expect("the summary records the §31 topics");
    assert_eq!(
        matrix.len(),
        TEST_MATRIX.len(),
        "{SUMMARY} carries {} topics and this file names {}",
        matrix.len(),
        TEST_MATRIX.len()
    );
    for (topic, tests) in TEST_MATRIX {
        let row = matrix
            .iter()
            .find(|row| row["topic"].as_str() == Some(topic))
            .unwrap_or_else(|| panic!("§31's {topic} is not in {SUMMARY}"));
        assert_eq!(
            row["tests"].as_str(),
            Some(tests),
            "{topic}: the tests the summary names are not the ones this file names"
        );
    }
}

/// Read every `module::test` name out of the source file it claims to live in.
fn assert_tests_exist(label: &str, tests: &str) {
    for path in tests
        .split(',')
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
    {
        let (module, test) = path
            .rsplit_once("::")
            .unwrap_or_else(|| panic!("{label}: {path} is not a module::test path"));
        let file = test_source_file(module);
        let text = std::fs::read_to_string(workspace_root().join(file))
            .unwrap_or_else(|err| panic!("{label} points at {file}: {err}"));
        assert!(
            text.contains(&format!("fn {test}(")),
            "{label} names {path}, and no function by that name is in {file}"
        );
    }
}

/// The committed summary carries the same control table this file compiles with; a summary
/// assembled before a control changed describes different controls than the ones on test.
#[test]
fn the_committed_summary_carries_the_control_table() {
    let summary: Value = read_json_value(SUMMARY);
    let rows = summary["negative_controls"]
        .as_array()
        .expect("the summary records the controls");
    assert_eq!(
        rows.len(),
        NEGATIVE_CONTROLS.len(),
        "{SUMMARY} carries {} controls and this file names {}",
        rows.len(),
        NEGATIVE_CONTROLS.len()
    );
    for (id, asks, tests) in NEGATIVE_CONTROLS {
        let row = rows
            .iter()
            .find(|row| row["control"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("{id} is not in {SUMMARY}"));
        assert_eq!(
            row["asks"].as_str(),
            Some(asks),
            "{id}: the question the summary asks is not the one this file records"
        );
        assert_eq!(
            row["tests"].as_str(),
            Some(tests),
            "{id}: the tests the summary names are not the ones this file names"
        );
    }
}

/// The refresh target writes; in default mode this is the same gate as the byte test
/// above, so a reader who runs the suite gets the comparison whether or not the
/// environment variable was set.
#[test]
fn refresh_writes_every_table() {
    let case = Case::load();
    let tables = case.tables();
    case.commit(&tables);
    assert_eq!(
        tables.len(),
        TABLES.len(),
        "the directory is supposed to carry exactly the §23 tables plus the strategy and \
         capacity evidence this milestone measured"
    );
    for name in TABLES {
        assert!(
            tables.contains_key(name),
            "{name} is in the table list but nothing builds it"
        );
    }
}

/// §29 ("M9.2 must work using canonical historical RPC. Do not integrate Flashblocks") and
/// §30 ("Do NOT implement … PathFinder is the next milestone") are boundaries of this
/// milestone, and a boundary nobody checks is only a sentence. Both are dependency-shaped
/// facts here, which is what this test reads: the flashblock reader lives in
/// `crates/live/src/flashblocks.rs` and the cycle/path search lives in
/// `crates/opportunity/src/path.rs`, so a crate that cannot see either of them cannot be
/// using either. The control is the same scan run against a crate that is allowed to use
/// both — if the vocabulary matched nothing anywhere, a zero in discovery would be vacuous.
#[test]
fn the_milestone_reads_canonical_history_and_stops_at_the_graph() {
    let manifest = source_text("crates/discovery/Cargo.toml");
    for forbidden in [
        "evm-live",
        "evm-opportunity",
        "evm-simulation",
        "evm-execution",
        "evm-pipeline",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "crates/discovery/Cargo.toml lists `{forbidden}`: §29 keeps this milestone on \
             canonical historical RPC and §30 keeps it short of any opportunity search, so \
             the dependency that would give discovery either one has to be removed, not \
             explained"
        );
    }

    // The manifest is what a build resolves; this is what the code actually names. The walk
    // is `src` only, because this very file has to spell the forbidden tokens in order to
    // check for them — a scan that reads its own word list would fail on itself. A `use` in a
    // test could not compile without a dependency line, and the manifest read above covers
    // `[dev-dependencies]` as well.
    let tree = workspace_root().join("crates/discovery/src");
    let mut checked = 0usize;
    for file in rust_files(&tree) {
        let text = std::fs::read_to_string(&file).expect("a discovery source file");
        checked += 1;
        for forbidden in [
            "evm_live::",
            "evm_opportunity::",
            "evm_simulation::",
            "evm_execution::",
            "evm_pipeline::",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} names `{forbidden}`; discovery's surface ends at a GraphSnapshot built \
                 from a pinned historical read (§30)",
                file.display()
            );
        }
    }
    assert!(
        checked > 0,
        "the scan walked no files under crates/discovery/src, so it proved nothing"
    );

    // Control: the same tokens do exist elsewhere in the workspace, in the crates whose job
    // they are. This is what makes the zeros above a boundary rather than a typo.
    let live = source_text("crates/live/src/flashblocks.rs");
    assert!(
        live.contains("Flashblock"),
        "crates/live/src/flashblocks.rs is the reader §29 excludes; it no longer names a \
         flashblock, so this control has to be re-pointed at whatever replaced it"
    );
    let pipeline = source_text("crates/pipeline/src/engine.rs");
    assert!(
        pipeline.contains("evm_live::"),
        "the exclusion vocabulary was checked against `evm_live::` in the pipeline engine, and \
         it is gone — a scan whose tokens match nothing anywhere would pass on any crate"
    );
    let arbitrage = source_text("crates/pipeline/src/arbitrage.rs");
    assert!(
        arbitrage.contains("evm_opportunity::"),
        "the search vocabulary was checked against `evm_opportunity::` in the pipeline \
         arbitrage module, and it is gone"
    );
}
