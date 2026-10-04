//! M9.1 §18/§26 — the historical census against a real GIWA node.
//!
//! This is the one target in the crate that talks to a chain, and it is `#[ignore]`
//! so `cargo test --workspace` never spends a request by accident. Run it as:
//!
//! ```text
//! GIWA_RPC_URL=<endpoint> cargo test -p evm-discovery --test historical_live -- \
//!     --ignored --nocapture --test-threads=1
//! ```
//!
//! Optional overrides, both so a re-run can be scoped without editing code:
//!
//! ```text
//! M91_WINDOWS="from-to,from-to,…"   replaces the window list below
//! M91_EVIDENCE_DIR=<path>           replaces data/evidence/m9/m9.1/raw
//! ```
//!
//! ## What it does
//!
//! It runs the *same* functions the offline tests exercise — `scan`,
//! `collect_candidate_reads`, `verify`, `integrate` — over a set of historical
//! windows, twice (pass A and pass B), and compares the two runs. §18 asks for one
//! discovery implementation used by replay and live alike; the way to prove there is
//! only one is to run it twice against the same history and require the same answer.
//!
//! ## What it writes
//!
//! Raw, per-pass output under the evidence directory: the scan reports (including
//! every raw `PairCreated` log, so the candidate set can be re-derived offline with
//! no node), the collected records per candidate, the verified and rejected rows,
//! the registry/state/graph result, and the node's own call trace as jsonl. The
//! committed tables are assembled from these files by `tests/evidence_gate.rs`,
//! which never touches the network — so "reproducible" here means a reviewer can
//! recompute every number from the raw material, not re-run the census.
//!
//! ## What it costs
//!
//! Read-only, and only these methods: `eth_chainId`, `eth_getLogs`, `eth_call`,
//! `eth_getCode` — asserted from the adapter's trace below, not from intent (§25:
//! zero signatures, zero broadcasts, zero executions). Per window it issues
//! `ceil(10_000 / 4_999) = 3` log requests, then 4 pinned reads + 1 `Sync` search per
//! candidate. The window list was picked from `data/evidence/m7/census-pair-created.json`
//! (1,030 `PairCreated` logs across the whole chain), so the candidate count per
//! window is small by construction: this census is tens of requests, not thousands.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use evm_chain::{ChainAdapter, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource};
use evm_core::BlockNumber;
use evm_discovery::{
    collect_candidate_reads, integrate, verify, CandidateReads, DiscoveredState, DuplicateClaim,
    GraphOutcome, HistoricalPairCreatedSource, RejectedPool, ScanReport, StoreRejection,
    Verification, VerifiedPool,
};
use evm_graph::{GraphEdge, SkippedPool};
use evm_protocol::{PoolAttestation, Registry, V2Adapter};
use evm_state::UpdatePosition;

/// The five windows this census looks at, chosen from where the M7 census actually
/// reported `PairCreated` activity (the chain idles for millions of blocks between
/// clusters, and a list of only productive windows would overstate the coverage).
const WINDOWS: [(u64, u64); 5] = [
    (4_050_000, 4_059_999),
    (28_540_000, 28_549_999),
    (31_710_000, 31_719_999),
    (37_180_000, 37_189_999),
    (37_220_000, 37_229_999),
];

const RAW_DIR: &str = "data/evidence/m9/m9.1/raw";

/// The only methods a discovery census may put on the wire. Anything else in the
/// adapter's own trace means discovery grew an execution path, and §25 forbids it.
const READ_ONLY_METHODS: [&str; 4] = ["eth_chainId", "eth_getLogs", "eth_call", "eth_getCode"];

/// One pass, as a document. Everything in it is either something the node returned or
/// something discovery decided from it — no field is a restatement of a count that a
/// reader could not recompute from the rows beside it (§19).
#[derive(Serialize)]
struct Pass {
    pass: &'static str,
    chain_id: u64,
    windows: Vec<ScanReport>,
    candidates: Vec<CandidateReads>,
    verified: Vec<VerifiedPool>,
    rejected: Vec<RejectedPool>,
    state: StateAndGraph,
    committed_registry: Vec<RegistryCrossCheck>,
    rpc: RpcAccounting,
}

/// The integrated result, flattened to serializable rows. `DiscoveredState` itself is
/// values (so two passes can be compared with `==`); this is what a reviewer reads.
#[derive(Serialize)]
struct StateAndGraph {
    attested: Vec<PoolAttestation>,
    duplicates: Vec<DuplicateClaim>,
    store_rejections: Vec<StoreRejection>,
    snapshot_position: Option<UpdatePosition>,
    registry_pool_count: usize,
    edges: Vec<GraphEdge>,
    skipped: Vec<SkippedPool>,
}

/// What happens when a discovered attestation meets the hand-maintained registry.
///
/// `merge` refuses at the first conflict, so each pool is checked against its own
/// clone — the alternative would report one conflict per run and hide the rest.
#[derive(Serialize)]
struct RegistryCrossCheck {
    pool: String,
    outcome: &'static str,
    message: Option<String>,
}

#[derive(Serialize)]
struct RpcAccounting {
    total: usize,
    by_method: BTreeMap<String, usize>,
    /// The block argument every read carried, tallied — decimal heights are pinned
    /// history, `latest`/`pending`/`earliest` would not be (§22).
    by_block_argument: BTreeMap<String, usize>,
    dropped_events: u64,
    refusals: Vec<String>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rpc_url() -> String {
    std::env::var("GIWA_RPC_URL").expect(
        "GIWA_RPC_URL is required: this target reads a live node and no endpoint is \
         hardcoded (M9.1 §44 / the project's no-default-endpoint rule)",
    )
}

fn raw_dir() -> PathBuf {
    match std::env::var("M91_EVIDENCE_DIR") {
        Ok(override_dir) if !override_dir.is_empty() => PathBuf::from(override_dir),
        _ => workspace_root().join(RAW_DIR),
    }
}

fn windows() -> Vec<(BlockNumber, BlockNumber)> {
    match std::env::var("M91_WINDOWS") {
        Ok(spec) if !spec.is_empty() => spec
            .split(',')
            .map(|pair| {
                let (from, to) = pair
                    .split_once('-')
                    .unwrap_or_else(|| panic!("M91_WINDOWS wants from-to, got {pair}"));
                (
                    BlockNumber(from.trim().parse().expect("window start")),
                    BlockNumber(to.trim().parse().expect("window end")),
                )
            })
            .collect(),
        _ => WINDOWS
            .iter()
            .map(|(from, to)| (BlockNumber(*from), BlockNumber(*to)))
            .collect(),
    }
}

/// Scan, collect, verify, integrate — the whole pipeline for one pass, with no
/// knowledge that a second pass exists.
///
/// Returns the pass document and *its own* call events: the sink accumulates for the
/// whole run, so `events_before` is where this pass started in that accumulation. The
/// connect call's `eth_chainId` precedes pass a and is counted in pass a, which is
/// honest (it is a read the run performed) and is why the two passes' totals are never
/// compared to each other — only their decisions are.
async fn run_pass(
    pass: &'static str,
    chain: &HttpChainAdapter,
    sink: &RpcTraceSink,
    events_before: usize,
) -> (Pass, Vec<RpcCallEvent>) {
    let source = HistoricalPairCreatedSource::new();
    let adapter = V2Adapter::new(Registry::default());
    let mut reports = Vec::new();
    let mut candidates = Vec::new();

    for (from, to) in windows() {
        let report = source
            .scan(chain, &adapter, from, to)
            .await
            .unwrap_or_else(|err| {
                panic!("pass {pass} could not scan {}..={}: {err}", from.0, to.0)
            });
        // The state search runs from each candidate's own discovery block to the end
        // of the window it was found in. It cannot move the verification: the four
        // contract reads stay pinned at the discovery block whatever this is (§22),
        // so a wider search only adds the chance of finding a published `Sync`.
        let search_to = report.to_block;
        for candidate in &report.candidates {
            let reads = collect_candidate_reads(chain, candidate, search_to)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "pass {pass} could not collect reads for {}: {err}",
                        candidate.pool.address
                    )
                });
            candidates.push(reads);
        }
        reports.push(report);
    }

    let mut verified = Vec::new();
    let mut rejected = Vec::new();
    for reads in &candidates {
        match verify(reads) {
            Verification::Verified(pool) => verified.push(pool),
            Verification::Rejected(rejection) => rejected.push(rejection),
        }
    }

    let chain_id = chain.chain_id();
    let state = state_rows(
        &integrate(chain_id, &Registry::default(), &verified)
            .unwrap_or_else(|err| panic!("pass {pass} could not integrate: {err}")),
    );
    let committed_registry = cross_check_committed(&state.attested);

    let events = sink.events();
    let own = events[events_before..].to_vec();
    let rpc = account(&own, sink);
    (
        Pass {
            pass,
            chain_id: chain_id.0,
            windows: reports,
            candidates,
            verified,
            rejected,
            state,
            committed_registry,
            rpc,
        },
        own,
    )
}

fn state_rows(state: &DiscoveredState) -> StateAndGraph {
    let edges: Vec<GraphEdge> = match &state.graph {
        GraphOutcome::Built(build) => build.graph.edges().copied().collect(),
        GraphOutcome::NoStateApplied => Vec::new(),
    };
    let skipped: Vec<SkippedPool> = match &state.graph {
        GraphOutcome::Built(build) => build.skipped.clone(),
        GraphOutcome::NoStateApplied => Vec::new(),
    };
    StateAndGraph {
        attested: state.attested.clone(),
        duplicates: state.duplicates.clone(),
        store_rejections: state.store_rejections.clone(),
        snapshot_position: state.snapshot.position,
        registry_pool_count: state.registry.pools.len(),
        edges,
        skipped,
    }
}

/// Every attested pool against `data/protocols/*.json`, one clone at a time, so a
/// single early conflict cannot hide the others.
fn cross_check_committed(attested: &[PoolAttestation]) -> Vec<RegistryCrossCheck> {
    let base = Registry::load_dir(&workspace_root().join("data/protocols"))
        .expect("the committed registry loads");
    let mut rows = Vec::with_capacity(attested.len());
    for attestation in attested {
        let pool = attestation.pool;
        let mut clone = base.clone();
        let mut one = Registry::default();
        one.attest(attestation.clone());
        let row = match clone.merge(one) {
            Ok(()) => RegistryCrossCheck {
                pool: format!("{pool:?}"),
                outcome: "no_entry_in_committed_registry",
                message: None,
            },
            Err(err) => RegistryCrossCheck {
                pool: format!("{pool:?}"),
                outcome: if base.is_pool(pool) {
                    "conflicts_with_committed_entry"
                } else {
                    "refused"
                },
                message: Some(err.to_string()),
            },
        };
        rows.push(row);
    }
    rows.sort_by(|left, right| left.pool.cmp(&right.pool));
    rows
}

/// Tally one pass's own call events. `dropped_events` and `refusals` are read off the
/// sink and therefore describe the whole run, not just this pass — the sink accumulates
/// until the process ends. Both must be zero for the run to mean anything, and the
/// assertions below say so in those terms.
fn account(events: &[RpcCallEvent], sink: &RpcTraceSink) -> RpcAccounting {
    let mut by_method: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_block_argument: BTreeMap<String, usize> = BTreeMap::new();
    for event in events {
        *by_method.entry(event.method.clone()).or_default() += 1;
        if let Some(block) = &event.block {
            *by_block_argument.entry(block.clone()).or_default() += 1;
        }
    }
    RpcAccounting {
        total: events.len(),
        by_method,
        by_block_argument,
        dropped_events: sink.dropped_events(),
        refusals: sink.refusals(),
    }
}

fn write_json(path: &Path, value: &impl Serialize) {
    let text = serde_json::to_string_pretty(value).expect("serialize");
    std::fs::write(path, text).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

fn write_jsonl(path: &Path, rows: &[impl Serialize]) {
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(row).expect("serialize one call"));
        text.push('\n');
    }
    std::fs::write(path, text).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live GIWA node and writes evidence; M9.1 §26 runs it explicitly"]
async fn historical_census_is_reproducible_and_lands_in_the_graph() {
    let url = rpc_url();
    let sink = RpcTraceSink::new(
        std::time::Instant::now(),
        "m9.1-historical-census",
        RpcTraceSource::Live,
        None,
    );
    let chain = HttpChainAdapter::connect_with_trace(&url, Some(sink.clone()))
        .await
        .expect("connect to the GIWA endpoint");
    let directory = raw_dir();
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|err| panic!("create {}: {err}", directory.display()));

    let (pass_a, trace_a) = run_pass("a", &chain, &sink, 0).await;
    let (pass_b, trace_b) = run_pass("b", &chain, &sink, trace_a.len()).await;

    // ── §21: the same history, twice, must be the same run.
    let identities = |pass: &Pass| -> Vec<(u64, String, u64, u64, u64)> {
        pass.candidates
            .iter()
            .map(|reads| {
                let (chain, address, block, tx_index, log_index) = reads.candidate.identity();
                (chain, address.to_string(), block, tx_index, log_index)
            })
            .collect()
    };
    assert_eq!(
        identities(&pass_a),
        identities(&pass_b),
        "the candidate set moved between passes"
    );
    let verified_pools = |pass: &Pass| -> Vec<(String, u64)> {
        pass.verified
            .iter()
            .map(|pool| {
                (
                    format!("{:?}", pool.candidate.pool.address),
                    pool.pinned_at.0,
                )
            })
            .collect()
    };
    assert_eq!(verified_pools(&pass_a), verified_pools(&pass_b));
    let rejected_rows = |pass: &Pass| -> Vec<(String, u64, u64, &'static str)> {
        pass.rejected
            .iter()
            .map(|row| {
                (
                    format!("{:?}", row.candidate.pool.address),
                    row.candidate.discovery_block().0,
                    row.candidate.discovered_at.log_index.0,
                    row.reason.as_str(),
                )
            })
            .collect()
    };
    assert_eq!(rejected_rows(&pass_a), rejected_rows(&pass_b));
    assert_eq!(
        serde_json::to_string(&pass_a.state.attested).expect("serialize"),
        serde_json::to_string(&pass_b.state.attested).expect("serialize"),
        "the registry rows differ between passes"
    );
    assert_eq!(pass_a.state.edges, pass_b.state.edges, "the graph moved");
    assert_eq!(
        serde_json::to_string(&pass_a.state).expect("serialize"),
        serde_json::to_string(&pass_b.state).expect("serialize"),
        "the state/graph document differs between passes"
    );

    // ── §22 / §25, read off the node's own trace rather than asserted from intent.
    for (label, accounting) in [("a", &pass_a.rpc), ("b", &pass_b.rpc)] {
        for method in accounting.by_method.keys() {
            assert!(
                READ_ONLY_METHODS.contains(&method.as_str()),
                "pass {label} issued {method}, which a discovery census may not ask"
            );
        }
        assert_eq!(
            accounting.dropped_events, 0,
            "pass {label} lost trace events"
        );
        assert!(
            accounting.refusals.is_empty(),
            "pass {label}: the sink refused to record {:?} of its own calls",
            accounting.refusals
        );
    }
    // A pinned read names a number. `latest` in any of these columns would mean a
    // candidate born at block N was judged against a contract that did not exist yet.
    let block_arguments = |pass: &Pass| -> Vec<String> {
        pass.rpc
            .by_block_argument
            .keys()
            .filter(|argument| !argument.chars().all(|c| c.is_ascii_digit()))
            .cloned()
            .collect()
    };
    for (label, pass) in [("a", &pass_a), ("b", &pass_b)] {
        let unpinned = block_arguments(pass);
        assert!(
            unpinned.is_empty(),
            "pass {label} asked for a block by tag, not by number: {unpinned:?}"
        );
    }

    // ── The census found *something*: a window list that produced zero candidates
    // would satisfy every assertion above while proving nothing (§26's real evidence).
    assert!(
        !pass_a.candidates.is_empty(),
        "no PairCreated candidate in {} blocks — the window list is wrong, not the code",
        pass_a
            .windows
            .iter()
            .map(|w| w.blocks_covered())
            .sum::<u64>()
    );
    assert!(
        !pass_a.state.attested.is_empty(),
        "candidates were all rejected: {:?}",
        rejected_rows(&pass_a)
    );
    assert!(
        !pass_a.state.edges.is_empty(),
        "verified pools reached the registry but the graph has no edge"
    );

    // ── Raw evidence, per pass. Nothing here is a summary of a summary: the scan
    // reports carry the node's raw logs, the candidates carry the records verify saw.
    // `rpc-calls-pass-a.jsonl` also carries the one call the connection itself made
    // (`eth_chainId`); pass b's file is its own calls only.
    write_json(&directory.join("pass-a.json"), &pass_a);
    write_json(&directory.join("pass-b.json"), &pass_b);
    write_jsonl(&directory.join("rpc-calls-pass-a.jsonl"), &trace_a);
    write_jsonl(&directory.join("rpc-calls-pass-b.jsonl"), &trace_b);

    println!(
        "census: {} windows, {} candidates, {} verified, {} rejected, {} attested, {} edges; \
         {} call events in pass a (the connection's eth_chainId included), {} in pass b",
        pass_a.windows.len(),
        pass_a.candidates.len(),
        pass_a.verified.len(),
        pass_a.rejected.len(),
        pass_a.state.attested.len(),
        pass_a.state.edges.len(),
        pass_a.rpc.total,
        pass_b.rpc.total,
    );
    println!("raw evidence: {}", directory.display());
}
