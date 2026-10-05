//! M9.2 §11/§12/§20/§27 — `Sync` reconstruction against a real GIWA node.
//!
//! The offline half (`sync_reconstruction.rs`, `state_at_target.rs`) proves the
//! *decision*: which `Sync` a scan selects, what that scan proves, what it may not
//! claim. Three questions are not answerable offline, because they are properties of a
//! node and of this chain's history rather than of the code:
//!
//! - **What does the node allow?** (§11) M9.1 measured a range cap while scanning
//!   `PairCreated`. This milestone asks a different question of the same node — a
//!   chain-wide `Sync` census returns far more logs per block — so the width is
//!   re-measured here rather than inherited by assumption.
//! - **Which query strategy is cheaper, and do both mean the same thing?** (§12)
//! - **What is the state of M9.1's 80 verified pools at the block M9.1 priced its graph
//!   at?** (§20)
//!
//! All three are `#[ignore]`: `cargo test --workspace` must never spend a request by
//! accident. Each reads the endpoint from the environment only:
//!
//! ```text
//! GIWA_RPC_URL=<endpoint> cargo test -p evm-discovery --test sync_reconstruction_live \
//!     -- --ignored --nocapture --test-threads=1 <target-name>
//! ```
//!
//! ## Why they are three targets and not one
//!
//! [`probe_node_capacity_and_sync_density`] costs about twenty requests and answers the
//! range-cap and per-chunk-volume questions. It exists so the runs below pick a chunk
//! width from a measurement instead of discovering a refusal six thousand requests in: a
//! run that dies on the node's range cap at request 6,000 is not cheap, it is wasted.
//!
//! [`query_strategies_select_the_same_sync_on_real_history`] is §12 — strategy A (per
//! pool, walking backwards from the target, stopping where it finds a `Sync`) against
//! strategy B (one chain-wide `Sync` pass), over the same pools at the same target. The
//! selections must be *identical*; only after that does the cost comparison become a
//! choice rather than a race.
//!
//! [`target_block_reconstruction_is_reproducible`] is §20/§27: the chosen strategy run
//! twice at the target, the two reconstructions and the two graphs compared, and the raw
//! material for `data/evidence/m9/m9.2/` written out.
//!
//! ## Where the pools come from
//!
//! `data/evidence/m9/m9.1/raw/pass-a.json`, deserialized into the same `VerifiedPool`
//! values M9.1 produced. §20 asks what happens to *those* 80 pools, so the input is
//! literally that milestone's committed output rather than a fresh census that might find
//! a different set — and the graph this run builds is therefore directly comparable with
//! the 24-pool graph M9.1 published.
//!
//! ## What is committed
//!
//! Per pool: the row the scan produced (its searched range, how many `Sync` logs it saw,
//! which one it selected with block, tx index, log index and both reserves, and the
//! outcome it licenses) — that is `sync-events.jsonl`, and it is the whole basis of the
//! selection, because a row's `selected` is by construction the highest-position `Sync`
//! its scan read inside `search_from..=search_to`. Plus, for a census, every chunk it
//! asked for with its returned-log count, so the pass's volume is auditable without
//! committing every other pool's history. What is *not* committed is the intermediate
//! `Sync` logs a per-pool scan passed over on its way down, and §24 does not ask for
//! them: a count of them is in the row beside the one that was chosen.
//!
//! ## What it may ask
//!
//! Only `eth_getLogs`, plus the one `eth_chainId` the connection itself makes, each with
//! a decimal block number. There is no contract read on this path at all: §5 forbids
//! `getReserves()` from standing in for a `Sync`, so it is not merely unused here but
//! absent, and the adapter's own trace is checked for exactly that (§28).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use evm_chain::{
    ChainAdapter, ChainLog, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource,
    MAX_EVENTS_PER_SINK, RPC_TRACE_SCHEMA,
};
use evm_core::{BlockNumber, ChainId, PoolId};
use evm_discovery::{
    integrate_at_target, DiscoveredState, DuplicateClaim, HistoricalSyncSource, PoolStateAtTarget,
    PoolSyncAtTarget, Reconstruction, ScanWindow, SyncCensus, VerifiedPool,
};
use evm_graph::{GraphEdge, SkippedPool};
use evm_protocol::{PoolAttestation, Registry};
use evm_state::UpdatePosition;

/// The block M9.1 priced its graph at, so the two milestones describe the same moment
/// and §20's "explain all 80" is a comparison rather than a coincidence.
const DEFAULT_TARGET: u64 = 37_224_031;

const RAW_DIR: &str = "data/evidence/m9/m9.2/raw";

/// M9.1's own live output, read as values rather than as a table.
const M91_RAW_PASS: &str = "data/evidence/m9/m9.1/raw/pass-a.json";

/// `eth_chainId` is the connection's own first call, not a reconstruction read; it is
/// in the trace and named here so the audit below has nothing to explain away.
const READ_ONLY_METHODS: [&str; 2] = ["eth_chainId", "eth_getLogs"];

const CHAIN: ChainId = ChainId(91342);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn rpc_url() -> String {
    std::env::var("GIWA_RPC_URL").expect(
        "GIWA_RPC_URL is required: this target reads a live node and no endpoint is \
         hardcoded (the project's no-default-endpoint rule, carried from M9.1)",
    )
}

fn raw_dir() -> PathBuf {
    match std::env::var("M92_EVIDENCE_DIR") {
        Ok(override_dir) if !override_dir.is_empty() => PathBuf::from(override_dir),
        _ => workspace_root().join(RAW_DIR),
    }
}

fn target_block() -> BlockNumber {
    match std::env::var("M92_TARGET") {
        Ok(spec) if !spec.is_empty() => BlockNumber(spec.parse().expect("M92_TARGET is a height")),
        _ => BlockNumber(DEFAULT_TARGET),
    }
}

/// Chunk width for the run. The default is the width M9.1 measured for a log scan;
/// [`probe_node_capacity_and_sync_density`] establishes whether it is also right for
/// this one, and a re-run at a measured width is what would change what production uses
/// (§11: not a universal blockchain rule, a property of this node).
fn chunk_blocks() -> u64 {
    match std::env::var("M92_CHUNK") {
        Ok(spec) if !spec.is_empty() => spec.parse().expect("M92_CHUNK is a block count"),
        _ => evm_discovery::CHUNK_BLOCKS,
    }
}

/// Which strategy §20 runs. There is no default: the strategy is §12's answer, and a run
/// that could not say which strategy it used would be evidence of nothing.
fn strategy() -> String {
    match std::env::var("M92_STRATEGY") {
        Ok(spec) if spec == "pool" || spec == "census" => spec,
        Ok(other) => panic!("M92_STRATEGY must be `pool` or `census`, not {other}"),
        _ => panic!(
            "M92_STRATEGY is required: §12 chooses the strategy, and this run is the chosen \
             one executed twice"
        ),
    }
}

/// Which arms §12 runs against the live node.
///
/// `both` — the default — is the experiment as §12 describes it. `b` re-measures only the
/// census arm and reads the per-pool arm's committed document back from disk, because arm A
/// is a two-hour read and an arm that dies fifteen minutes into the second half must not
/// cost another two hours of the first. Reuse is not trust: [`committed_arm_a`] re-derives
/// every count in the saved document from its own rows and re-reads its trace file, so a
/// `b`-only run is required to establish the same facts about arm A that a `both` run
/// established before it wrote them.
///
/// There is no `a`: one arm of a comparison cannot report §12, and a run that executed only
/// arm A would overwrite the document the reuse path reads.
fn arm() -> String {
    match std::env::var("M92_ARM") {
        Ok(spec) if spec == "both" || spec == "b" => spec,
        Ok(other) => panic!(
            "M92_ARM must be `both` (run both arms live) or `b` (reuse the committed arm-A \
             document and run the census arm), not {other}"
        ),
        _ => "both".to_string(),
    }
}

async fn connect() -> (HttpChainAdapter, RpcTraceSink) {
    let sink = RpcTraceSink::new(
        Instant::now(),
        "m9.2-sync-reconstruction",
        RpcTraceSource::Live,
        None,
    );
    let chain = HttpChainAdapter::connect_with_trace(&rpc_url(), Some(sink.clone()))
        .await
        .expect("connect to the GIWA endpoint");
    (chain, sink)
}

fn write_json(path: &Path, value: &impl Serialize) {
    let text = serde_json::to_string_pretty(value).expect("serialize");
    std::fs::write(path, text).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

fn write_jsonl(path: &Path, rows: &[impl Serialize]) {
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(row).expect("serialize one row"));
        text.push('\n');
    }
    std::fs::write(path, text).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

fn prepare_directory() -> PathBuf {
    let directory = raw_dir();
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|err| panic!("create {}: {err}", directory.display()));
    directory
}

/// Every call the sink saw must be a read of a pinned height (§28), and the calls it could
/// not see must be exactly the ones its own cap names.
///
/// `scans` comes from the scan's arithmetic — `Reconstruction::chunks_scanned` or
/// `SyncCensus::rpc_calls` — never from the trace, so the sum below compares two
/// independent records of the same run. One sink holds [`MAX_EVENTS_PER_SINK`] events and
/// counts the rest rather than dropping them silently; a per-pool scan of this milestone's
/// length is longer than that cap, so the trace is a sample, and what this proves is that
/// the sample plus the counted remainder is every call there was. A signature, a
/// broadcast, or any read the scan does not explain would make the left side larger than
/// the right and stop the run here.
fn audit_trace(events: &[RpcCallEvent], sink: &RpcTraceSink, scans: u64) {
    let mut stamps = 0u64;
    for event in events {
        assert!(
            READ_ONLY_METHODS.contains(&event.method.as_str()),
            "the run asked {}, which reconstruction may not",
            event.method
        );
        if event.method == "eth_chainId" {
            stamps += 1;
        }
        if let Some(block) = &event.block {
            assert!(
                block.chars().all(|c| c.is_ascii_digit()),
                "the run asked for block {block} by tag instead of by number"
            );
        }
    }
    let held = events.len() as u64;
    let dropped = sink.dropped_events();
    assert_eq!(
        held + dropped,
        scans + stamps,
        "the sink held {held} events and counted {dropped} beyond its \
         {MAX_EVENTS_PER_SINK}-event cap, {} calls against the {} the scan accounts for \
         ({scans} scans plus {stamps} chain-id reads) — either the sink lost events it did \
         not count, or the run made calls the scan does not explain",
        held + dropped,
        scans + stamps,
    );

    // A refusal is only expected when it is the cap naming the event it counted. The
    // marker is built from the same constant the sink interpolates, so a cap change
    // moves both; a refusal for any other reason (a write after the list closed, a sink
    // asked to record nothing) is the instrumentation declining to do its job, and that
    // stops the run rather than being averaged into a count.
    let cap_marker = format!("one sink holds at most {MAX_EVENTS_PER_SINK} calls");
    let (beyond_cap, other): (Vec<String>, Vec<String>) = sink
        .refusals()
        .into_iter()
        .partition(|note| note.contains(&cap_marker));
    assert!(
        other.is_empty(),
        "the sink refused to record {other:?} for a reason other than its \
         {MAX_EVENTS_PER_SINK}-event cap"
    );
    assert_eq!(
        beyond_cap.len() as u64,
        dropped,
        "the sink counted {dropped} dropped events but refused to record {} — the two \
         counters name different runs",
        beyond_cap.len(),
    );
}

// ── the pool set: M9.1's own verified pools, read as values ───────────────────

#[derive(Deserialize)]
struct M91Pass {
    chain_id: u64,
    verified: Vec<VerifiedPool>,
}

/// The 80 pools M9.1 verified, straight out of its committed raw pass, plus the path
/// they came from (which every document written below names as its input).
fn m91_verified_pools() -> (Vec<VerifiedPool>, String) {
    let path = workspace_root().join(M91_RAW_PASS);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let pass: M91Pass = serde_json::from_str(&text).expect("M9.1 raw pass");
    assert_eq!(pass.chain_id, CHAIN.0, "M9.1's pass names another chain");
    // The repo-relative name, not `path`: what this run read goes into a committed record,
    // and an absolute path from one machine is not a citation another machine can open.
    (pass.verified, M91_RAW_PASS.to_string())
}

/// Each pool's own lower bound: the earliest block any claim about it names (§8).
///
/// A pool claimed twice keeps the *earlier* block, because a scan may not claim to have
/// covered the interval between two claims it never looked at.
fn lower_bounds(verified: &[VerifiedPool]) -> BTreeMap<PoolId, BlockNumber> {
    let mut bounds: BTreeMap<PoolId, BlockNumber> = BTreeMap::new();
    for pool in verified {
        let claimed_at = pool.candidate.discovery_block();
        bounds
            .entry(pool.candidate.pool)
            .and_modify(|existing| *existing = (*existing).min(claimed_at))
            .or_insert(claimed_at);
    }
    bounds
}

fn source() -> HistoricalSyncSource {
    HistoricalSyncSource::with_chunk_blocks(HistoricalSyncSource::new(), chunk_blocks())
        .expect("the chunk width is a positive number of blocks")
}

/// One strategy, one target, the whole pool set — a `Reconstruction` either way, so §12
/// can compare them and §20 can run whichever won. The census returns its pass too,
/// because the pass has volume the per-pool rows cannot show.
async fn run_strategy(
    chain: &HttpChainAdapter,
    bounds: &BTreeMap<PoolId, BlockNumber>,
    target: BlockNumber,
    strategy: &str,
) -> (Reconstruction, Option<SyncCensus>) {
    let source = source();
    match strategy {
        "pool" => (
            source
                .reconstruct(chain, bounds, target)
                .await
                .expect("strategy A completed"),
            None,
        ),
        "census" => {
            let earliest = bounds
                .values()
                .map(|block| block.0)
                .min()
                .expect("the pool set is not empty");
            let census = source
                .census(chain, BlockNumber(earliest), target)
                .await
                .expect("strategy B completed");
            let reconstruction = census.reconstruction(bounds);
            (reconstruction, Some(census))
        }
        other => panic!("no strategy named {other}"),
    }
}

/// How many times §20's harness asks one pool's range before it gives up.
///
/// This is not a second retry policy for a request: the production client keeps its own
/// single retry per request, and a refusal here is the whole *pool range* being asked for
/// again. The number is published per pass (`pool_call_refusals`), and the requests a
/// refused pass cost are counted onto the wire rather than folded into the reconstruction's
/// own `chunks_scanned`.
const POOL_CALL_ATTEMPTS: usize = 3;

/// §20's pass, one pool at a time, retried at the pool boundary.
///
/// `HistoricalSyncSource::reconstruct` is a loop over pools that adds each pool's counters
/// into one `Reconstruction`, so asking it for one pool at a time and summing the same
/// counters builds the same value. That is checkable rather than assumed: §12's per-pool arm
/// committed the same eighty pools at a different chunk width, and the evidence gate's
/// `the_target_run_selects_the_same_sync_as_the_experiment_arm` compares the two by the
/// semantic key of their own rows.
///
/// The reason for the pieces is measured: four consecutive whole-pass runs ended on one
/// transport failure the node returned (1,250 s, 1,598 s, 1,794 s and 6,724 s into the read),
/// and a pass that cannot survive one such answer costs its whole window each time.
///
/// Returns the reconstruction, how many pool passes were thrown away, and how many
/// `eth_getLogs` requests the run put on the wire — which is more than the reconstruction
/// accounts for, exactly by the work those refused passes had already done.
async fn run_pool_pass(
    chain: &HttpChainAdapter,
    sink: &RpcTraceSink,
    pass: &'static str,
    bounds: &BTreeMap<PoolId, BlockNumber>,
    target: BlockNumber,
) -> (Reconstruction, usize, u64) {
    let source = source();
    // Read the chain id before the first measurement, so a request delta names only
    // `eth_getLogs` and never the chain-id stamp.
    let chain_id = chain.chain_id();
    let mut rows = Vec::with_capacity(bounds.len());
    let mut chunks_scanned = 0usize;
    let mut logs_returned = 0usize;
    let mut refusals = 0usize;
    let mut wasted_scans = 0u64;
    // The sink counts every call it was given, including the ones whose caller then threw
    // the answer away, so what a refused pass cost is read off the sink around it.
    let calls_so_far = |sink: &RpcTraceSink| sink.recorded_events() as u64 + sink.dropped_events();
    for (index, (pool, discovery_block)) in bounds.iter().enumerate() {
        let one = BTreeMap::from([(*pool, *discovery_block)]);
        let mut asked = 1usize;
        loop {
            let before = calls_so_far(sink);
            match source.reconstruct(chain, &one, target).await {
                Ok(one_pool) => {
                    assert_eq!(
                        one_pool.rows.len(),
                        1,
                        "pass {pass}: a scan asked for one pool and returned {} rows",
                        one_pool.rows.len()
                    );
                    assert_eq!(
                        one_pool.target, target,
                        "pass {pass}: the pool at index {index} was reconstructed at block {} \
                         rather than the target {}",
                        one_pool.target.0, target.0
                    );
                    rows.extend(one_pool.rows);
                    chunks_scanned += one_pool.chunks_scanned;
                    logs_returned += one_pool.logs_returned;
                    break;
                }
                Err(err) => {
                    let after = calls_so_far(sink);
                    wasted_scans += after.saturating_sub(before);
                    refusals += 1;
                    if asked == POOL_CALL_ATTEMPTS {
                        panic!(
                            "pass {pass}: pool {} refused the scan {POOL_CALL_ATTEMPTS} \
                             times, the last as {err}",
                            pool.address
                        );
                    }
                    asked += 1;
                }
            }
        }
        println!(
            "  pass {pass}: pool {nth}/{pools} reconstructed at {height}, {chunks} \
             `eth_getLogs` requests accounted for, {refusals} refused pass{plural} so far",
            nth = index + 1,
            pools = bounds.len(),
            height = target.0,
            chunks = chunks_scanned,
            plural = if refusals == 1 { "" } else { "es" },
        );
    }
    let wire_scans = chunks_scanned as u64 + wasted_scans;
    let reconstruction = Reconstruction {
        chain_id,
        target,
        rows,
        chunks_scanned,
        logs_returned,
    };
    (reconstruction, refusals, wire_scans)
}

// ── §11: the node's own capacity, measured on the filter this milestone uses ────

/// One range the probe asked about, and what came back.
#[derive(Serialize)]
struct ProbeRow {
    /// `pool` or `chain-wide`, i.e. which of the two filters §12 compares.
    filter: &'static str,
    from_block: u64,
    to_block: u64,
    blocks: u64,
    /// The address a `pool` filter named, so a reader can re-ask the same question.
    address: Option<String>,
    accepted: bool,
    logs_returned: usize,
    /// Wall time of the request. §12 measures cost in requests and logs; time is what a
    /// run actually waits, so both are kept.
    duration_ms: u64,
    error: Option<String>,
}

async fn probe_once(
    chain: &HttpChainAdapter,
    source: &HistoricalSyncSource,
    filter: &'static str,
    address: Option<PoolId>,
    from: u64,
    to: u64,
) -> ProbeRow {
    let (from_block, to_block) = (BlockNumber(from), BlockNumber(to));
    let log_filter = match address {
        Some(pool) => source.pool_filter(&pool, from_block, to_block),
        None => source.census_filter(from_block, to_block),
    };
    let started = Instant::now();
    let outcome = chain.get_logs(log_filter).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let (accepted, logs_returned, error) = match outcome {
        Ok(logs) => (true, logs.len(), None),
        Err(err) => (false, 0, Some(err.to_string())),
    };
    ProbeRow {
        filter,
        from_block: from,
        to_block: to,
        blocks: to - from + 1,
        address: address.map(|pool| format!("{:?}", pool.address)),
        accepted,
        logs_returned,
        duration_ms,
        error,
    }
}

/// Measure the two things a chunk width has to respect on this node, for the filter shape
/// this milestone actually sends.
///
/// A range cap and a per-request result ceiling are different limits, and which one binds
/// decides the run's cost by an order of magnitude: the cap says how wide a chunk may be,
/// the ceiling says how wide a chunk may be *before its answer is truncated*, and a
/// truncated `Sync` census is worse than a refused one — it turns "a later `Sync` exists
/// and was not read" into "none was found". `refuse_truncated_window` exists so that
/// never becomes a state claim, but the run would still stop, which is why the volume is
/// measured before the run rather than discovered inside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live GIWA node; M9.2 §11 runs it explicitly"]
async fn probe_node_capacity_and_sync_density() {
    let (chain, sink) = connect().await;
    let source = HistoricalSyncSource::new();
    let target = target_block().0;
    let bounds = lower_bounds(&m91_verified_pools().0);
    let sample_pool = *bounds
        .keys()
        .next()
        .expect("M9.1's committed set is not empty");

    let mut rows: Vec<ProbeRow> = Vec::new();
    // Width ladder on the chain-wide filter: the span M9.1 measured, the cap it recorded,
    // and past both. A refusal is a finding here, not a failure, so it is recorded.
    for (from, to) in [
        (target - 4_999, target),
        (target - 9_999, target),
        (target - 10_000, target),
        (target - 19_999, target),
        (target - 49_999, target),
    ] {
        rows.push(probe_once(&chain, &source, "chain-wide", None, from, to).await);
    }
    // The same ladder on one pool's filter, which is the other §12 arm.
    for (from, to) in [
        (target - 4_999, target),
        (target - 10_000, target),
        (target - 19_999, target),
    ] {
        rows.push(probe_once(&chain, &source, "pool", Some(sample_pool), from, to).await);
    }
    // Volume: the chain-wide answer per 5,000-block chunk down the band this chain
    // actually traded in, then in the idle history between clusters, so the ceiling is
    // compared against a measured busiest chunk rather than a guess.
    for window in 0..6u64 {
        let to = target - window * 5_000;
        rows.push(probe_once(&chain, &source, "chain-wide", None, to - 4_999, to).await);
    }
    for window in [1_000u64, 3_000u64, 6_000u64] {
        let to = target - window * 5_000;
        rows.push(probe_once(&chain, &source, "chain-wide", None, to - 4_999, to).await);
    }

    let accepted = rows.iter().filter(|row| row.accepted).count();
    let widest = |filter: &'static str| {
        rows.iter()
            .filter(|row| row.accepted && row.filter == filter)
            .map(|row| row.blocks)
            .max()
            .unwrap_or(0)
    };
    let chain_wide_5000 = rows
        .iter()
        .filter(|row| row.filter == "chain-wide" && row.blocks == 5_000)
        .map(|row| (row.logs_returned, row.from_block, row.to_block))
        .max()
        .unwrap_or((0, 0, 0));
    let total_logs: usize = rows.iter().map(|row| row.logs_returned).sum();
    let ceiling = evm_discovery::NODE_LOG_LIMIT;

    let directory = prepare_directory();
    write_json(&directory.join("probe-node-capacity.json"), &rows);
    let trace = sink.events();
    write_jsonl(&directory.join("probe-node-capacity-rpc.jsonl"), &trace);
    audit_trace(&trace, &sink, rows.len() as u64);

    println!(
        "probe: {} rows, {} accepted; widest accepted chain-wide range {} blocks, widest \
         accepted pool range {} blocks; busiest 5,000-block chain-wide chunk = {} logs at \
         {}..={}; {} logs across the whole probe",
        rows.len(),
        accepted,
        widest("chain-wide"),
        widest("pool"),
        chain_wide_5000.0,
        chain_wide_5000.1,
        chain_wide_5000.2,
        total_logs,
    );
    for row in &rows {
        println!(
            "  {:<10} {:>10}..{:<10} {:>6} blocks {:>6} logs {:>6} ms {}",
            row.filter,
            row.from_block,
            row.to_block,
            row.blocks,
            row.logs_returned,
            row.duration_ms,
            if row.accepted {
                "ok".to_string()
            } else {
                format!("REFUSED: {}", row.error.clone().unwrap_or_default())
            }
        );
    }
    // The number the next two targets are chosen against: a chunk of the run's width must
    // not be able to reach the node's result ceiling.
    println!(
        "the per-request ceiling is {} logs; the busiest 5,000-block chunk above used {:.2}% \
         of it, so a {}-block chunk would need {:.1}x the headroom",
        ceiling,
        chain_wide_5000.0 as f64 * 100.0 / ceiling as f64,
        widest("chain-wide"),
        widest("chain-wide") as f64 / 5_000.0,
    );
}

// ── §12: the two strategies, on real history, against the same target ──────────

/// What one sink recorded of a run that may be longer than one sink can hold.
///
/// `eth_getlogs_scans` is the run's own arithmetic — `Reconstruction::chunks_scanned` or
/// `SyncCensus::rpc_calls` — while the two event counts come from the sink, so the
/// document carries both halves of the sum [`audit_trace`] requires. `events_dropped` is
/// not a failure: past [`MAX_EVENTS_PER_SINK`] the sink counts instead of storing, and a
/// per-pool scan of this milestone's length is past it. What the document must not do is
/// let a reader mistake a capped list for the whole call list.
#[derive(Serialize)]
struct TraceAccounting {
    events_recorded: usize,
    events_dropped: u64,
    max_events_per_sink: usize,
    eth_getlogs_scans: u64,
}

fn accounting(sink: &RpcTraceSink, events_recorded: usize, scans: usize) -> TraceAccounting {
    TraceAccounting {
        events_recorded,
        events_dropped: sink.dropped_events(),
        max_events_per_sink: MAX_EVENTS_PER_SINK,
        eth_getlogs_scans: scans as u64,
    }
}

/// One strategy's run, as a document: what it cost, and which `Sync` it selected for
/// every pool.
#[derive(Serialize)]
struct StrategyRun {
    strategy: &'static str,
    chunk_blocks: u64,
    target: u64,
    pools: usize,
    /// `eth_getLogs` requests this run made. For a census this is the pass's own count
    /// rather than a sum over rows — `PoolSyncAtTarget`'s fields say the same.
    eth_getlogs_requests: usize,
    logs_returned: usize,
    /// Blocks scanned summed over pools (work done), and blocks the pass covered (history
    /// read). A census has one of each; a per-pool strategy has only the first.
    blocks_scanned: u64,
    blocks_covered_by_the_pass: Option<u64>,
    /// Logs the census returned about pools it was not asked about. `None` for a per-pool
    /// strategy, which by construction asks about one pool at a time.
    irrelevant_logs: Option<usize>,
    undecodable_logs: Option<usize>,
    duplicate_logs: Option<usize>,
    wall_ms: u64,
    target_state_valid: usize,
    state_unavailable: usize,
    state_invalid: usize,
    scan_incomplete: usize,
    rows: Vec<PoolSyncAtTarget>,
    trace: TraceAccounting,
}

fn strategy_run(
    strategy: &'static str,
    reconstruction: &Reconstruction,
    census: Option<&SyncCensus>,
    bounds: &BTreeMap<PoolId, BlockNumber>,
    wall_ms: u64,
    trace: TraceAccounting,
) -> StrategyRun {
    let counted = |outcome: PoolStateAtTarget| reconstruction.counted(outcome);
    let irrelevant = |census: &SyncCensus| {
        census.irrelevant_logs(&bounds.keys().copied().collect::<BTreeSet<PoolId>>())
    };
    StrategyRun {
        strategy,
        chunk_blocks: chunk_blocks(),
        target: reconstruction.target.0,
        pools: reconstruction.rows.len(),
        eth_getlogs_requests: reconstruction.chunks_scanned,
        logs_returned: reconstruction.logs_returned,
        blocks_scanned: reconstruction.blocks_scanned(),
        blocks_covered_by_the_pass: census.map(SyncCensus::blocks_covered),
        irrelevant_logs: census.map(&irrelevant),
        undecodable_logs: census.map(|census| census.undecodable_logs),
        duplicate_logs: census.map(|census| census.duplicate_logs),
        wall_ms,
        target_state_valid: counted(PoolStateAtTarget::Reconstructed),
        state_unavailable: counted(PoolStateAtTarget::NothingPublished),
        state_invalid: counted(PoolStateAtTarget::EmptyReserves),
        scan_incomplete: counted(PoolStateAtTarget::ScanDoesNotReachTarget),
        rows: reconstruction.rows.clone(),
        trace,
    }
}

/// The `trace` object of a saved run, under the field names [`strategy_run`] writes.
#[derive(Deserialize)]
struct SavedTraceAccounting {
    events_recorded: usize,
    events_dropped: u64,
    max_events_per_sink: usize,
    eth_getlogs_scans: u64,
}

/// One arm's committed document, read back with the field names [`strategy_run`] writes.
///
/// A mirror rather than `#[derive(Deserialize)]` on `StrategyRun`, for the reason
/// [`SavedPass`] gives: every counter has to be named by the thing that reads it, so a
/// reused run cannot be missing a figure the comparison depends on.
#[derive(Deserialize)]
struct SavedStrategyRun {
    strategy: String,
    chunk_blocks: u64,
    target: u64,
    pools: usize,
    eth_getlogs_requests: usize,
    logs_returned: usize,
    blocks_scanned: u64,
    irrelevant_logs: Option<usize>,
    wall_ms: u64,
    trace: SavedTraceAccounting,
    target_state_valid: usize,
    state_unavailable: usize,
    state_invalid: usize,
    scan_incomplete: usize,
    rows: Vec<PoolSyncAtTarget>,
}

/// Read arm A's committed document and the trace file beside it, and hand back the values
/// §12's comparison needs.
///
/// Nothing is taken on trust. The four §21 classes, the scanned-block total and the pool
/// count are re-derived from `rows` with the same `Reconstruction` methods a live arm is
/// measured by; the per-row `eth_getLogs` counts are summed against the document's total,
/// which holds only for a per-pool strategy (a census row repeats the shared pass's own
/// figures, so its sum would be the pass times the pool count); and every row is re-checked
/// against §8's lower bound for its pool. A document that will not re-derive describes a
/// different run than the one the census arm would be compared with, so this stops the test
/// before the second arm spends its time.
///
/// What cannot be re-checked from disk is the sink's refusal list — it lives in the run that
/// filled it. That is why the document carries both event counters: the arithmetic required
/// below is the same equation [`audit_trace`] required of the live sink, applied to the bytes
/// that were written *after* that audit passed, which is what makes the file's existence a
/// record of the check rather than a substitute for it.
fn committed_arm_a(
    directory: &Path,
    bounds: &BTreeMap<PoolId, BlockNumber>,
    target: BlockNumber,
) -> (Reconstruction, StrategyRun) {
    let path = directory.join("strategy-a-pool.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let saved: SavedStrategyRun =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));

    // Provenance: the document has to describe *this* experiment.
    assert_eq!(
        saved.strategy,
        "pool",
        "{} is a {} run, not the per-pool arm",
        path.display(),
        saved.strategy
    );
    assert_eq!(
        saved.chunk_blocks,
        chunk_blocks(),
        "arm A was measured at {} blocks per chunk and arm B is being asked for at {}: a \
         cost comparison between two chunk widths compares the widths, not the strategies",
        saved.chunk_blocks,
        chunk_blocks()
    );
    assert_eq!(
        BlockNumber(saved.target),
        target,
        "arm A priced {} and the census arm is running at {}",
        saved.target,
        target.0
    );
    assert!(
        saved.irrelevant_logs.is_none(),
        "the per-pool arm reports irrelevant-log counts, which only a chain-wide pass can \
         have: this document is not the arm §12 names A"
    );

    // §8 and §7, per row.
    let rows: &Vec<PoolSyncAtTarget> = &saved.rows;
    assert_eq!(
        rows.len(),
        saved.pools,
        "the document's pool count is not the number of rows it carries"
    );
    assert_eq!(
        rows.len(),
        bounds.len(),
        "the reused arm covered {} pools and M9.1's committed set has {}",
        rows.len(),
        bounds.len()
    );
    for row in rows {
        assert_eq!(
            row.pool.chain_id, CHAIN,
            "a reused row belongs to another chain"
        );
        assert_eq!(
            row.target, target,
            "a reused row answers for {:?}, not {target:?}",
            row.target
        );
        assert_eq!(
            row.search_to, target,
            "a reused row's scan stopped at {:?}, below the target it is offered for",
            row.search_to
        );
        let bound = *bounds
            .get(&row.pool)
            .unwrap_or_else(|| panic!("the reused document covers a pool M9.1 did not verify"));
        assert_eq!(
            row.discovery_block, bound,
            "a reused row names a lower bound that is not M9.1's discovery block for it"
        );
        assert!(
            row.search_from >= bound,
            "a reused row scanned {:?} below its pool's discovery block {bound:?}",
            row.search_from
        );
    }

    // The row-level sums against the run-level counters, then the trace arithmetic.
    let row_requests: usize = rows.iter().map(|row| row.chunks_scanned).sum();
    assert_eq!(
        row_requests, saved.eth_getlogs_requests,
        "the rows account for {row_requests} requests and the document says {}",
        saved.eth_getlogs_requests
    );
    let row_logs: usize = rows.iter().map(|row| row.logs_returned).sum();
    assert_eq!(
        row_logs, saved.logs_returned,
        "the rows account for {row_logs} returned logs and the document says {}",
        saved.logs_returned
    );

    let trace_path = directory.join("strategy-a-rpc.jsonl");
    let trace_text = std::fs::read_to_string(&trace_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", trace_path.display()));
    let mut chain_id_reads = 0u64;
    let mut recorded = 0u64;
    for line in trace_text.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("{}: {err}", trace_path.display()));
        assert_eq!(
            value.get("trace_schema").and_then(|v| v.as_u64()),
            Some(RPC_TRACE_SCHEMA),
            "{} is not a schema-{RPC_TRACE_SCHEMA} line",
            trace_path.display()
        );
        let method = value
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("a trace line names no method"));
        assert!(
            READ_ONLY_METHODS.contains(&method),
            "the reused arm asked {method}, which reconstruction may not"
        );
        if method == "eth_chainId" {
            chain_id_reads += 1;
        }
        if let Some(block) = value.get("block").and_then(|v| v.as_str()) {
            assert!(
                block.chars().all(|c| c.is_ascii_digit()),
                "the reused arm asked for block {block} by tag instead of by number"
            );
        }
        recorded += 1;
    }
    assert_eq!(
        recorded,
        saved.trace.events_recorded as u64,
        "{} holds {recorded} lines and the document counted {}",
        trace_path.display(),
        saved.trace.events_recorded
    );
    assert_eq!(
        saved.trace.max_events_per_sink, MAX_EVENTS_PER_SINK,
        "the document names a sink capacity of {} and this build holds {}",
        saved.trace.max_events_per_sink, MAX_EVENTS_PER_SINK
    );
    assert_eq!(
        saved.trace.eth_getlogs_scans, saved.eth_getlogs_requests as u64,
        "the document's trace section and its request counter disagree"
    );
    assert_eq!(
        recorded + saved.trace.events_dropped,
        saved.eth_getlogs_requests as u64 + chain_id_reads,
        "the reused arm's trace is {} recorded plus {} counted past the cap against {} \
         requests plus {chain_id_reads} chain-id reads",
        recorded,
        saved.trace.events_dropped,
        saved.eth_getlogs_requests
    );

    // Re-derive the run from its rows, and require the document to have said the same.
    let reconstruction = Reconstruction {
        chain_id: CHAIN,
        target,
        rows: saved.rows.clone(),
        chunks_scanned: saved.eth_getlogs_requests,
        logs_returned: saved.logs_returned,
    };
    let run = strategy_run(
        "pool",
        &reconstruction,
        None,
        bounds,
        saved.wall_ms,
        TraceAccounting {
            events_recorded: saved.trace.events_recorded,
            events_dropped: saved.trace.events_dropped,
            max_events_per_sink: saved.trace.max_events_per_sink,
            eth_getlogs_scans: saved.trace.eth_getlogs_scans,
        },
    );
    for (label, derived, written) in [
        (
            "target_state_valid",
            run.target_state_valid,
            saved.target_state_valid,
        ),
        (
            "state_unavailable",
            run.state_unavailable,
            saved.state_unavailable,
        ),
        ("state_invalid", run.state_invalid, saved.state_invalid),
        (
            "scan_incomplete",
            run.scan_incomplete,
            saved.scan_incomplete,
        ),
    ] {
        assert_eq!(
            derived, written,
            "the document's {label} is {written} and its rows re-classify as {derived}"
        );
    }
    assert_eq!(
        run.blocks_scanned, saved.blocks_scanned,
        "the document's rows sum to {} blocks of scanning and it says {}",
        run.blocks_scanned, saved.blocks_scanned
    );
    (reconstruction, run)
}

/// §12: run both query strategies over the same pools at the same target, require the
/// same `Sync` selections, and record what each cost.
///
/// The assertion that matters first. If the two strategies disagree about which `Sync`
/// prices a pool, one of them is wrong, and no cost figure should ever be compared
/// against that run — which is why the cost halves are printed only after the correctness
/// halves have been required.
///
/// Each arm writes its own document the moment it finishes, before the other arm starts.
/// An hour-long run whose second half hits a node limit must not lose the first half's
/// measurement with it.
///
/// Each arm also gets its own connection and its own sink, because the event cap
/// [`MAX_EVENTS_PER_SINK`] is per sink: one arm holding a shared sink can fill it and
/// leave the other arm's trace file a length that has nothing to do with how many calls
/// that arm made. Each arm's §28 audit runs before its own bytes are committed, so an arm
/// that cannot account for every call it made leaves no evidence behind for a later table
/// to quote.
///
/// `M92_ARM=b` runs the census arm against the per-pool arm's committed document instead of
/// its live run, which is what makes a second attempt at a failed arm cost the arm that
/// failed rather than the whole experiment.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live GIWA node; M9.2 §12 runs it explicitly"]
async fn query_strategies_select_the_same_sync_on_real_history() {
    let target = target_block();
    let bounds = lower_bounds(&m91_verified_pools().0);
    let directory = prepare_directory();

    // Arm A: measured against the node now, or read back from the document a previous
    // attempt committed — see [`arm`] for why both are offered, and [`committed_arm_a`] for
    // what the reused arm is still required to prove. Its wall time is its own: a `b`-only
    // run measures arm B in a different window than the one arm A was measured in, so the
    // cross-arm ratio below is a comparison between two separate observations of one node.
    let (per_pool, rows_a) = if arm() == "both" {
        let (chain_a, sink_a) = connect().await;
        let started = Instant::now();
        let (per_pool, _) = run_strategy(&chain_a, &bounds, target, "pool").await;
        let wall_a = started.elapsed().as_millis() as u64;
        let trace_a = sink_a.events();
        let rows_a = strategy_run(
            "pool",
            &per_pool,
            None,
            &bounds,
            wall_a,
            accounting(&sink_a, trace_a.len(), per_pool.chunks_scanned),
        );
        // §28, per arm, against the scan count that arm's own document records — before the
        // arm's bytes are committed, so an arm that lost a call leaves no evidence behind.
        audit_trace(&trace_a, &sink_a, rows_a.eth_getlogs_requests as u64);
        write_json(&directory.join("strategy-a-pool.json"), &rows_a);
        write_jsonl(&directory.join("strategy-a-rpc.jsonl"), &trace_a);
        println!(
            "A per-pool measured now: {} requests, {} logs, {} ms",
            rows_a.eth_getlogs_requests, rows_a.logs_returned, rows_a.wall_ms
        );
        (per_pool, rows_a)
    } else {
        let (per_pool, rows_a) = committed_arm_a(&directory, &bounds, target);
        println!(
            "A per-pool reused from {}: {} requests, {} logs, {} ms measured in its own \
             window ({} trace lines re-read, {} counted past the cap)",
            directory.join("strategy-a-pool.json").display(),
            rows_a.eth_getlogs_requests,
            rows_a.logs_returned,
            rows_a.wall_ms,
            rows_a.trace.events_recorded,
            rows_a.trace.events_dropped,
        );
        (per_pool, rows_a)
    };

    let (chain_b, sink_b) = connect().await;
    let started = Instant::now();
    let (wide, census) = run_strategy(&chain_b, &bounds, target, "census").await;
    let wall_b = started.elapsed().as_millis() as u64;
    let census = census.expect("the census strategy returns its own pass");
    let trace_b = sink_b.events();
    let rows_b = strategy_run(
        "census",
        &wide,
        Some(&census),
        &bounds,
        wall_b,
        accounting(&sink_b, trace_b.len(), wide.chunks_scanned),
    );
    audit_trace(&trace_b, &sink_b, rows_b.eth_getlogs_requests as u64);
    write_json(&directory.join("strategy-b-census.json"), &rows_b);
    write_jsonl(&directory.join("strategy-b-rpc.jsonl"), &trace_b);
    write_json(
        &directory.join("strategy-b-census-chunks.json"),
        &census_chunks(&census),
    );
    println!(
        "B census done: {} requests, {} logs, {} ms",
        rows_b.eth_getlogs_requests, rows_b.logs_returned, rows_b.wall_ms
    );

    // ── correctness: the same market state, whichever way it was asked for.
    assert_eq!(
        per_pool.target, wide.target,
        "the two runs were not at one target"
    );
    assert_eq!(
        per_pool.selections(),
        wide.selections(),
        "the two strategies disagree about which Sync prices a pool — the cheaper run is \
         not a strategy, it is a bug"
    );
    // Coverage is compared where the two strategies cannot differ, and not where they
    // differ by construction: both must reach this target, for the same pools. The lower
    // bounds are not comparable — a per-pool walk stops at the block that answered, while
    // a chain-wide pass has one shared start for every pool it read — and the part of
    // coverage that licenses a price, each row's own `proves_valid_at`, is already inside
    // the `selections` map compared above.
    let reached = |reconstruction: &Reconstruction| -> BTreeMap<PoolId, BlockNumber> {
        reconstruction
            .coverage()
            .into_iter()
            .map(|(pool, coverage)| (pool, coverage.through))
            .collect()
    };
    assert_eq!(
        reached(&per_pool),
        reached(&wide),
        "the two strategies reached different heights, or covered different pools"
    );
    assert!(
        reached(&per_pool)
            .values()
            .all(|through| *through == target),
        "a coverage row does not reach the target it is offered for"
    );
    // §21: the two strategies must also leave the pools classified the same way.
    for (label, run) in [("A", &rows_a), ("B", &rows_b)] {
        assert_eq!(
            run.target_state_valid
                + run.state_unavailable
                + run.state_invalid
                + run.scan_incomplete,
            run.pools,
            "strategy {label} left a pool in none of the four classes"
        );
        assert_eq!(
            run.scan_incomplete, 0,
            "strategy {label} produced a scan that does not reach its own target"
        );
    }
    assert_eq!(rows_a.pools, rows_b.pools);
    assert_eq!(rows_a.target_state_valid, rows_b.target_state_valid);
    assert_eq!(rows_a.state_unavailable, rows_b.state_unavailable);
    assert_eq!(rows_a.state_invalid, rows_b.state_invalid);

    println!(
        "A per-pool: {} requests, {} logs, {} blocks of scanning, {} ms — {} valid / {} \
         unavailable / {} invalid",
        rows_a.eth_getlogs_requests,
        rows_a.logs_returned,
        rows_a.blocks_scanned,
        rows_a.wall_ms,
        rows_a.target_state_valid,
        rows_a.state_unavailable,
        rows_a.state_invalid,
    );
    println!(
        "B census:   {} requests, {} logs ({} about pools not asked about, {} undecodable, \
         {} duplicated), {} blocks covered, {} ms — {} valid / {} unavailable / {} invalid",
        rows_b.eth_getlogs_requests,
        rows_b.logs_returned,
        rows_b.irrelevant_logs.unwrap_or_default(),
        rows_b.undecodable_logs.unwrap_or_default(),
        rows_b.duplicate_logs.unwrap_or_default(),
        rows_b.blocks_covered_by_the_pass.unwrap_or_default(),
        rows_b.wall_ms,
        rows_b.target_state_valid,
        rows_b.state_unavailable,
        rows_b.state_invalid,
    );
    println!(
        "selections identical across {} pools at target {}",
        rows_a.pools, rows_a.target
    );
    // §12 asks for all four measures — calls, returned logs, irrelevant logs, wall time —
    // names none of them as the decider, and puts correctness first. The decider this
    // milestone uses is the request count, because it is the one cost figure that belongs to
    // the strategy rather than to the window: the same arm, run twice at this target over
    // these pools, asked for 21,439 requests both times and took 6,505,859 ms once and
    // 7,216,588 ms the other — a reproducible count and a 10.9% duration. A ratio between two
    // arms' durations, measured in different windows (`M92_ARM=b` guarantees that, and so did
    // two separate live attempts), would describe one node's two moods rather than two
    // strategies. The evidence gate reads the count; both durations stay published, here and
    // in `strategy-comparison.json`, as diagnostics.
    let cheaper = if rows_b.eth_getlogs_requests < rows_a.eth_getlogs_requests {
        "census"
    } else {
        "pool"
    };
    println!(
        "cheaper arm by request count: {cheaper} — {} requests against {}; run §20 with \
         M92_STRATEGY={cheaper}",
        rows_a.eth_getlogs_requests, rows_b.eth_getlogs_requests,
    );
    println!(
        "diagnostic only, one duration per measurement window: {} ms for the per-pool arm \
         against {} ms for the census arm",
        rows_a.wall_ms, rows_b.wall_ms,
    );
    if arm() != "both" {
        println!(
            "arm A was read back from its committed document, so its window closed before this \
             run began; the request counts and the identical selections are what this run \
             establishes"
        );
    }
}

// ── §20/§27: the real target block, twice, with the graph it produces ──────────

/// The census's own volume, as one row per chunk of the pass.
#[derive(Serialize)]
struct CensusChunkRow {
    from_block: u64,
    to_block: u64,
    logs_returned: usize,
}

#[derive(Serialize)]
struct CensusTotals {
    chunks: usize,
    logs_returned: usize,
    blocks_covered: u64,
    duplicate_logs: usize,
    undecodable_logs: usize,
    irrelevant_logs: usize,
    asked_pool_logs: usize,
}

/// The state and graph one pass produced, flattened to serializable rows.
#[derive(Serialize)]
struct StateAndGraph {
    attested: Vec<PoolAttestation>,
    duplicates: Vec<DuplicateClaim>,
    store_rejections: Vec<evm_discovery::StoreRejection>,
    /// The block the store applied its last position at, and the block the snapshot is
    /// priced at — two different numbers from M9.2 on, and both kept visible (§13).
    snapshot_position: Option<UpdatePosition>,
    snapshot_target_block: Option<u64>,
    graph_block: Option<u64>,
    graph_pool_count: usize,
    edges: Vec<GraphEdge>,
    skipped: Vec<SkippedPool>,
}

fn state_and_graph(state: &DiscoveredState) -> StateAndGraph {
    let build = state.graph.build();
    StateAndGraph {
        attested: state.attested.clone(),
        duplicates: state.duplicates.clone(),
        store_rejections: state.store_rejections.clone(),
        snapshot_position: state.snapshot.position,
        snapshot_target_block: state.snapshot.target_block().map(|block| block.0),
        graph_block: build.map(|build| build.graph.block_number().0),
        graph_pool_count: build.map_or(0, |build| build.graph.pool_count()),
        edges: build
            .map(|build| build.graph.edges().copied().collect())
            .unwrap_or_default(),
        skipped: build.map(|build| build.skipped.clone()).unwrap_or_default(),
    }
}

/// One pass of the chosen strategy, as a document.
#[derive(Serialize)]
struct Pass {
    pass: &'static str,
    strategy: String,
    chunk_blocks: u64,
    chain_id: u64,
    target: u64,
    /// The input pool set and where it came from, so the run's coverage claim has a
    /// denominator a reader can check against M9.1's own tables.
    pools_input: usize,
    pools_input_source: String,
    earliest_discovery_block: u64,
    eth_getlogs_requests: usize,
    /// Requests the node was actually asked for: the figure above plus the requests of any
    /// pool pass that was refused and asked again. The reconstruction only counts the passes
    /// it kept, so a run that lost work to a transport failure says so here rather than
    /// publishing a cost the wire never paid.
    eth_getlogs_requests_on_the_wire: u64,
    pool_call_refusals: usize,
    logs_returned: usize,
    blocks_scanned: u64,
    wall_ms: u64,
    /// The §21 classes, as counts and as the rows behind them.
    target_state_valid: usize,
    state_unavailable: usize,
    state_invalid: usize,
    rows: Vec<PoolSyncAtTarget>,
    census_chunks: Option<Vec<CensusChunkRow>>,
    census_totals: Option<CensusTotals>,
    state: StateAndGraph,
    trace: TraceAccounting,
}

/// The part of a written pass that has to be re-read to prove the pass is
/// reproducible: the rows and the three run counters a `Reconstruction` needs and a
/// row cannot supply. Field names mirror `Pass`'s JSON keys, so this parses the
/// document as committed rather than as an in-memory value re-stated.
#[derive(Deserialize)]
struct SavedPass {
    chain_id: u64,
    target: u64,
    eth_getlogs_requests: usize,
    logs_returned: usize,
    rows: Vec<PoolSyncAtTarget>,
}

fn census_chunks(census: &SyncCensus) -> Vec<CensusChunkRow> {
    census
        .windows
        .iter()
        .map(|window: &ScanWindow| CensusChunkRow {
            from_block: window.from_block.0,
            to_block: window.to_block.0,
            logs_returned: window.logs_returned,
        })
        .collect()
}

/// `Sync` records this pass read, for the pools it was asked about, in chain order.
///
/// The material `sync-events.json` is assembled from, and the reason a reviewer can
/// re-derive every selection in `rows` with no node: a row's `selected` is the
/// highest-position record here that falls inside its own `search_from..=search_to`.
/// A per-pool scan does not hand its logs back, so this is only available for a census —
/// and `None` here is exactly the limitation §24 asks to be recorded rather than hidden.
fn asked_pool_sync_logs(
    census: &SyncCensus,
    bounds: &BTreeMap<PoolId, BlockNumber>,
) -> Vec<ChainLog> {
    let asked: BTreeSet<PoolId> = bounds.keys().copied().collect();
    census
        .raw_logs
        .iter()
        .filter(|log| asked.contains(&PoolId::new(census.chain_id, log.address)))
        .cloned()
        .collect()
}

/// §20/§27: the chosen strategy at the target block, run twice, and the graph the two
/// runs agree on.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live GIWA node; M9.2 §20 runs it explicitly"]
async fn target_block_reconstruction_is_reproducible() {
    let target = target_block();
    let chosen = strategy();
    let (verified, source_path) = m91_verified_pools();
    let bounds = lower_bounds(&verified);
    let directory = prepare_directory();
    let input_source = format!("{source_path} (M9.1 raw pass a)");

    let mut chain_id = None;
    let mut passes: Vec<Pass> = Vec::new();
    let mut reconstructions: Vec<Reconstruction> = Vec::new();
    let mut states: Vec<DiscoveredState> = Vec::new();

    for (index, name) in [(0usize, "a"), (1, "b")] {
        // One connection and one sink per pass. A sink's event cap is its own, so a shared
        // sink would hand the second pass whatever the first left room for — a trace file
        // whose length says more about the observer than about the pass it claims to record.
        let (chain, sink) = connect().await;
        let started = Instant::now();
        let (reconstruction, census, refusals, wire_scans) = if chosen == "pool" {
            let (reconstruction, refusals, wire_scans) =
                run_pool_pass(&chain, &sink, name, &bounds, target).await;
            (reconstruction, None, refusals, wire_scans)
        } else {
            let (reconstruction, census) = run_strategy(&chain, &bounds, target, &chosen).await;
            let wire_scans = reconstruction.chunks_scanned as u64;
            (reconstruction, census, 0, wire_scans)
        };
        let wall_ms = started.elapsed().as_millis() as u64;

        // §28 first: a pass that asked for something the scan does not explain should stop
        // the run before a second pass costs another hour of reading. The number it checks
        // against is the wire total, because a refused pool pass put requests on the wire
        // that the reconstruction it threw away does not account for.
        let trace = sink.events();
        audit_trace(&trace, &sink, wire_scans);

        let state = integrate_at_target(
            chain.chain_id(),
            &Registry::default(),
            &verified,
            &reconstruction,
        )
        .unwrap_or_else(|err| panic!("pass {name} could not reach the graph: {err}"));
        chain_id = Some(chain.chain_id());

        let counted = |outcome: PoolStateAtTarget| reconstruction.counted(outcome);
        let census_chunks = census.as_ref().map(census_chunks);
        let census_totals = census.as_ref().map(|census| CensusTotals {
            chunks: census.rpc_calls(),
            logs_returned: census.logs_returned(),
            blocks_covered: census.blocks_covered(),
            duplicate_logs: census.duplicate_logs,
            undecodable_logs: census.undecodable_logs,
            irrelevant_logs: census.irrelevant_logs(&bounds.keys().copied().collect()),
            asked_pool_logs: asked_pool_sync_logs(census, &bounds).len(),
        });
        let pass = Pass {
            pass: if index == 0 { "a" } else { "b" },
            strategy: chosen.clone(),
            chunk_blocks: chunk_blocks(),
            chain_id: chain.chain_id().0,
            target: target.0,
            pools_input: bounds.len(),
            pools_input_source: input_source.clone(),
            earliest_discovery_block: bounds.values().map(|block| block.0).min().unwrap_or(0),
            eth_getlogs_requests: reconstruction.chunks_scanned,
            eth_getlogs_requests_on_the_wire: wire_scans,
            pool_call_refusals: refusals,
            logs_returned: reconstruction.logs_returned,
            blocks_scanned: reconstruction.blocks_scanned(),
            wall_ms,
            target_state_valid: counted(PoolStateAtTarget::Reconstructed),
            state_unavailable: counted(PoolStateAtTarget::NothingPublished),
            state_invalid: counted(PoolStateAtTarget::EmptyReserves),
            rows: reconstruction.rows.clone(),
            census_chunks,
            census_totals,
            state: state_and_graph(&state),
            trace: accounting(&sink, trace.len(), wire_scans as usize),
        };

        // Written the moment the pass is done, for the same reason §12 writes each arm when
        // it finishes: an hour of reading should not be lost to whatever the next pass hits.
        write_json(
            &directory.join(format!("reconstruction-pass-{name}.json")),
            &pass,
        );
        write_jsonl(
            &directory.join(format!("rpc-calls-pass-{name}.jsonl")),
            &trace,
        );
        if let Some(census) = census.as_ref() {
            let logs = asked_pool_sync_logs(census, &bounds);
            write_jsonl(
                &directory.join(format!("sync-logs-pass-{name}.jsonl")),
                &logs,
            );
        }

        passes.push(pass);
        reconstructions.push(reconstruction);
        states.push(state);
    }
    let chain_id = chain_id.expect("both passes connected, so the chain id was read");

    // ── §27: the same inputs, twice, the same everything.
    assert_eq!(
        reconstructions[0], reconstructions[1],
        "the reconstruction moved between passes"
    );
    assert_eq!(
        reconstructions[0].coverage(),
        reconstructions[1].coverage(),
        "the coverage evidence moved between passes"
    );
    assert_eq!(
        states[0], states[1],
        "the state or the graph moved between passes"
    );
    assert_eq!(
        passes[0].state.edges, passes[1].state.edges,
        "the graph moved"
    );
    assert_eq!(
        passes[0].state.skipped, passes[1].state.skipped,
        "the skip accounting moved"
    );
    let identities = |pass: &Pass| -> Vec<(u64, Address, u64)> {
        pass.rows.iter().map(PoolSyncAtTarget::identity).collect()
    };
    assert_eq!(
        identities(&passes[0]),
        identities(&passes[1]),
        "row identity order differs, so an evidence table keyed on it would not line up"
    );

    // A pool the node refused is asked again, and the requests the refused attempt already
    // spent are disclosed rather than smoothed over: `eth_getlogs_requests` is what the rows
    // were built from, `eth_getlogs_requests_on_the_wire` is what the trace file carries, and
    // the difference is the work this pass paid for and threw away.
    for pass in &passes {
        assert!(
            pass.eth_getlogs_requests_on_the_wire >= pass.eth_getlogs_requests as u64,
            "pass {} put {} requests on the wire and accounts for {}",
            pass.pass,
            pass.eth_getlogs_requests_on_the_wire,
            pass.eth_getlogs_requests,
        );
    }

    // ── §22: every verified pool accounted for exactly once, with one reason.
    let a = &passes[0];
    assert_eq!(a.rows.len(), a.pools_input, "a pool was not reconstructed");
    assert_eq!(
        a.target_state_valid + a.state_unavailable + a.state_invalid,
        a.pools_input,
        "the §21 classes do not add up to the input set"
    );
    assert_eq!(
        a.state.graph_pool_count + a.state.skipped.len(),
        a.state.attested.len(),
        "graph pools + skipped pools must equal the attested set (§22)"
    );
    let skipped_pools: BTreeSet<PoolId> = a.state.skipped.iter().map(|row| row.pool).collect();
    assert_eq!(
        skipped_pools.len(),
        a.state.skipped.len(),
        "a pool was skipped twice, so it has two reasons"
    );
    assert_eq!(
        a.state.attested.len(),
        a.pools_input,
        "the attested set is not the input set — a pool was dropped before the graph"
    );
    assert_eq!(
        a.state.snapshot_target_block,
        Some(target.0),
        "the snapshot is not priced at the target the run asked for"
    );
    assert_eq!(a.state.graph_block, Some(target.0));
    for edge in &a.state.edges {
        assert!(
            edge.state_position.block_number <= target,
            "an edge from block {} priced the target {}",
            edge.state_position.block_number.0,
            target.0,
        );
    }
    for row in &a.state.skipped {
        assert!(
            row.state_position
                .is_none_or(|position| position.block_number <= target),
            "a pool was skipped for a state from after the target"
        );
    }

    // §28's audit and each pass's files were taken inside the loop, against that pass's own
    // sink and its own scan count.

    // §32's reproducibility read off the bytes rather than from memory: pass a's own
    // document is parsed back and asked to produce the same graph. Rows alone are not
    // a `Reconstruction` (the type carries no `Deserialize`, because its counters
    // belong to a run rather than to a pool), so the rebuild names each counter the
    // document stored for it — a silent zero here would fail the equality below.
    let saved: SavedPass = serde_json::from_slice(
        &std::fs::read(directory.join("reconstruction-pass-a.json"))
            .expect("pass a was written before it is read back"),
    )
    .expect("pass a parses back");
    let hydrated = Reconstruction {
        chain_id: ChainId(saved.chain_id),
        target: BlockNumber(saved.target),
        rows: saved.rows,
        chunks_scanned: saved.eth_getlogs_requests,
        logs_returned: saved.logs_returned,
    };
    assert_eq!(
        hydrated, reconstructions[0],
        "pass a's bytes are not pass a"
    );
    let replayed = integrate_at_target(chain_id, &Registry::default(), &verified, &hydrated)
        .expect("the saved rows re-integrate");
    assert_eq!(
        replayed, states[0],
        "the state and graph are not reproducible from the committed rows"
    );

    println!(
        "pass a: {} pools, {} requests, {} ms — graph {} pools / {} skipped; snapshot applied \
         at {:?}, priced at {:?}",
        a.pools_input,
        a.eth_getlogs_requests,
        a.wall_ms,
        a.state.graph_pool_count,
        a.state.skipped.len(),
        a.state.snapshot_position,
        a.state.snapshot_target_block,
    );
    println!(
        "classification: {} target_state_valid, {} state_unavailable, {} state_invalid, of {} \
         verified pools read from {}",
        a.target_state_valid,
        a.state_unavailable,
        a.state_invalid,
        a.pools_input,
        a.pools_input_source,
    );
    println!(
        "pass b: {} requests, {} ms — identical reconstruction, graph and coverage (§27)",
        passes[1].eth_getlogs_requests, passes[1].wall_ms,
    );
    let mut skip_reasons: BTreeMap<String, usize> = BTreeMap::new();
    for row in &a.state.skipped {
        let reason = serde_json::to_string(&row.reason).expect("a skip reason serializes");
        *skip_reasons.entry(reason).or_default() += 1;
    }
    println!(
        "edges {} from {} pools; {} skipped, by reason {:?}",
        a.state.edges.len(),
        a.state.graph_pool_count,
        a.state.skipped.len(),
        skip_reasons,
    );
    println!("raw evidence: {}", directory.display());
}

// ── the audit's own controls, off the network ─────────────────────────────────

/// One pinned `eth_getLogs` line, in the shape the choke point would have written it.
fn pinned_get_logs(rpc_id: u64) -> RpcCallEvent {
    RpcCallEvent {
        trace_schema: RPC_TRACE_SCHEMA,
        rpc_id,
        method: "eth_getLogs".to_string(),
        block: Some(DEFAULT_TARGET.to_string()),
        target: None,
        slot: None,
        started_ns: 0,
        finished_ns: 1,
        duration_ns: 1,
        success: true,
        error_class: None,
        error_detail: None,
        attempts: Vec::new(),
        dedup_key: None,
        key_note: None,
        stage: None,
        caller: None,
        context_note: None,
    }
}

/// Strategy A makes more calls than one sink can hold, so a beyond-cap run is the normal
/// case this audit has to accept — and an unexplained refusal is the case it must not.
///
/// Both directions are proved here rather than discovered mid-run: a rule that only ever
/// met the network is a rule that gets its first real test at the end of an hour of
/// reading, and the two counters (`dropped` and the refusal notes) can disagree in ways
/// no live run would separate from its own arithmetic.
#[test]
fn a_beyond_cap_trace_audits_clean_and_an_unexplained_refusal_does_not() {
    let sink = RpcTraceSink::new(
        Instant::now(),
        "audit-control",
        RpcTraceSource::Fixture,
        Some(CHAIN.0),
    );
    let mut chain_id = pinned_get_logs(0);
    chain_id.method = "eth_chainId".to_string();
    chain_id.block = None;
    chain_id.rpc_id = sink.next_rpc_id();
    sink.record(chain_id);

    let scans = MAX_EVENTS_PER_SINK as u64 + 1;
    for _ in 0..scans {
        let mut event = pinned_get_logs(0);
        event.rpc_id = sink.next_rpc_id();
        sink.record(event);
    }
    let total = scans + 1;
    assert_eq!(
        sink.recorded_events(),
        MAX_EVENTS_PER_SINK,
        "the sink kept a full list"
    );
    assert_eq!(
        sink.dropped_events() + sink.recorded_events() as u64,
        total,
        "{total} calls were offered, and the sink's two counters must account for all of them"
    );

    audit_trace(&sink.events(), &sink, scans);

    sink.refuse("the trace list was closed before this write".to_string());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        audit_trace(&sink.events(), &sink, scans)
    }));
    assert!(
        outcome.is_err(),
        "a refusal that is not the cap naming a dropped event passed the audit"
    );
}

/// A scratch directory a control can write into without touching committed evidence.
fn scratch_directory(name: &str) -> PathBuf {
    let directory = workspace_root().join("target").join(name);
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|err| panic!("create {}: {err}", directory.display()));
    directory
}

/// §12's reuse path, off the network: the committed arm-A document re-derived from its own
/// rows, and three tampered versions of the same bytes refused.
///
/// `M92_ARM=b` trusts this path with the better part of an hour of live reading, so it gets
/// proved before a run depends on it. The positive half is the file that will actually be
/// read back; each negative half moves one figure that the audit claims to check — a
/// classification count, a §8 lower bound, one trace line — and the *honest* pair is written
/// to the same scratch directory first, so a rejection is known to come from the tampered
/// figure rather than from the path.
#[test]
fn a_reused_arm_a_document_has_to_re_derive_from_its_own_rows() {
    let directory = raw_dir();
    let bounds = lower_bounds(&m91_verified_pools().0);
    let target = BlockNumber(DEFAULT_TARGET);
    let (reconstruction, run) = committed_arm_a(&directory, &bounds, target);

    assert_eq!(
        run.pools,
        bounds.len(),
        "the reused arm is not M9.1's pool set"
    );
    assert_eq!(reconstruction.target, target);
    assert_eq!(
        run.target_state_valid + run.state_unavailable + run.state_invalid + run.scan_incomplete,
        run.pools,
        "the reused arm leaves a pool unclassified"
    );
    assert_eq!(
        run.scan_incomplete, 0,
        "a reused row does not reach the target it is offered for, so nothing in it licenses a \
         price"
    );

    let honest_doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.join("strategy-a-pool.json"))
            .expect("arm A's document is the input to this control"),
    )
    .expect("arm A's document parses");
    let honest_trace = std::fs::read_to_string(directory.join("strategy-a-rpc.jsonl"))
        .expect("arm A's trace file is the input to this control");

    let mut classification = honest_doc.clone();
    let counted = classification["target_state_valid"]
        .as_u64()
        .expect("the document counts its classes");
    classification["target_state_valid"] = serde_json::Value::from(counted + 1);

    let mut lower_bound = honest_doc.clone();
    let bound = lower_bound["rows"][0]["discovery_block"]
        .as_u64()
        .expect("a row names its pool's discovery block");
    lower_bound["rows"][0]["discovery_block"] = serde_json::Value::from(bound - 1);

    let truncated_trace = honest_trace
        .lines()
        .take(honest_trace.lines().count() - 1)
        .collect::<Vec<_>>()
        .join("\n");
    let truncated_trace = format!("{truncated_trace}\n");

    let cases: Vec<(String, serde_json::Value, String)> = vec![
        (
            "honest bytes in a scratch directory".to_string(),
            honest_doc.clone(),
            honest_trace.clone(),
        ),
        (
            "one classification figure moved by one".to_string(),
            classification,
            honest_trace.clone(),
        ),
        (
            "one row's discovery block moved down one block".to_string(),
            lower_bound,
            honest_trace.clone(),
        ),
        (
            "the trace file one line shorter than its own count".to_string(),
            honest_doc,
            truncated_trace,
        ),
    ];

    for (index, (label, doc, trace_text)) in cases.iter().enumerate() {
        let scratch = scratch_directory(&format!("m92-arm-a-reuse-control-{index}"));
        write_json(&scratch.join("strategy-a-pool.json"), doc);
        std::fs::write(scratch.join("strategy-a-rpc.jsonl"), trace_text)
            .expect("write the control's trace file");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (reconstruction, run) = committed_arm_a(&scratch, &bounds, target);
            // The honest pair has to return exactly what the committed bytes returned, or the
            // control would show that a scratch copy is accepted and stop short of proving the
            // three rejections are about their tampered figure.
            assert_eq!(run.pools, bounds.len());
            reconstruction
        }));
        if index == 0 {
            assert!(
                outcome.is_ok(),
                "the reuse audit rejected the honest bytes: {label}"
            );
        } else {
            assert!(
                outcome.is_err(),
                "the reuse audit accepted {label}, so it does not check what it claims to"
            );
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
