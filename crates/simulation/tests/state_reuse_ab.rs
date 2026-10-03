//! M8.3.1 §7 and §14's Test B: the reuse boundary counted at the wire, not derived from
//! a trace.
//!
//! §7 forbids the cheapest version of this proof — reading the numbers out of M8.2's
//! duplicate tally and declaring the saving. So the same pinned simulation is run twice
//! against a JSON-RPC endpoint of this test's own, once with reuse off and once with it
//! on, and three independent accounts of every call are compared: what arrived at the
//! endpoint, what the adapter's sink recorded, and what the reuse boundary says it
//! looked up. If reuse avoided a read, all three move together; if it silently changed
//! an answer, the result fingerprint moves and the test says so.
//!
//! The endpoint is `support::stub` — the same one M8.3.3's concurrency fixture reads
//! through, so the two milestones provably run against the same recorded snapshot. This
//! suite asks a reuse question, so it takes the shape that answers one request at a time
//! on one thread ([`Stub::spawn`]): an arrival list whose order *is* the run's order, so a
//! cached arm's calls can be checked to be the baseline's with some removed and none
//! reordered. The two properties that make the endpoint an instrument rather than a
//! convenience — it holds only the pinned block, and it answers only what the dump
//! recorded — are stated there.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use serde_json::{json, Value};

use evm_chain::{BlockContext, ChainAdapter, HttpChainAdapter, RpcTraceSink, RpcTraceSource};
use evm_core::BlockNumber;
use evm_simulation::{
    engine::run, BlockPin, PricedRoute, RpcStateProvider, SimulationResult, StateProvider,
    StateReadStats,
};

mod support;
use support::stub::{ServedState, Stub};
use support::{request, BLOCK, CHAIN};

/// The four methods §3 allows this milestone to reuse, in §7's reporting order. The
/// header read is listed apart because §1 keeps its behaviour untouched.
const STATE_METHODS: [&str; 4] = [
    "eth_getCode",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getStorageAt",
];

/// One arm of the A/B: the same simulation, the same endpoint shape, one switch apart.
struct Arm {
    reuse: bool,
    /// Methods as the endpoint saw them, connect excluded.
    sequence: Vec<String>,
    /// The same calls as the adapter's sink saw them, with the §12 key each carries.
    keys: Vec<(String, Option<String>)>,
    method_counts: BTreeMap<String, usize>,
    /// `eth_getCode` + `eth_getBalance` + `eth_getTransactionCount` +
    /// `eth_getStorageAt`, counted off the endpoint's own arrivals.
    state_reads: usize,
    header_reads: usize,
    stats: StateReadStats,
    /// Sum of `duration_ns` over the sink's events: the wall time this arm spent inside
    /// provider calls, measured on one monotonic clock.
    rpc_wall: Duration,
    run_wall: Duration,
    result: SimulationResult,
}

impl Arm {
    fn method_count(&self, method: &str) -> usize {
        self.method_counts.get(method).copied().unwrap_or(0)
    }

    /// §5's comparison list, read off this arm's result. Each of these is a field a wrong
    /// cache key could move — a stale bytecode changes return data, a stale balance
    /// changes the gas ceiling, a stale slot changes the quote — and the identity check
    /// below runs over all of them before it falls back on the fingerprint.
    fn identity(&self) -> Value {
        json!({
            "status": format!("{:?}", self.result.status),
            "reverted": self.result.revert().is_some(),
            "revert": format!("{:?}", self.result.revert()),
            "gas_used": self.result.gas_used(),
            "outcome": format!("{:?}", self.result.outcome),
            "measurements": self
                .result
                .measurements
                .iter()
                .map(|m| format!("{}={}", m.binding, m.value))
                .collect::<Vec<_>>(),
            "step_statuses": self
                .result
                .steps
                .iter()
                .map(|step| format!("{}:{:?}", step.index, step.status))
                .collect::<Vec<_>>(),
            "logs": self.result.logs().len(),
            "gross_profit": self.result.gross_profit.map(|amount| amount.to_string()),
            "net_profit_word": match &self.result.net_profit {
                evm_simulation::result::NetProfit::Gain { .. } => "gain",
                evm_simulation::result::NetProfit::BreakEven { .. } => "break-even",
                evm_simulation::result::NetProfit::Loss { .. } => "loss",
                evm_simulation::result::NetProfit::Shortfall { .. } => "shortfall",
                evm_simulation::result::NetProfit::NotComputable { .. } => "not-computable",
            },
            "net_profit": format!("{:?}", self.result.net_profit),
            "fingerprint": self.result.fingerprint(),
        })
    }

    fn row(&self) -> Value {
        let mut per_method = serde_json::Map::new();
        for method in STATE_METHODS {
            per_method.insert(
                method.to_string(),
                json!({"requests": self.method_count(method)}),
            );
        }
        per_method.insert(
            "eth_getBlockByNumber".to_string(),
            json!({"requests": self.header_reads}),
        );
        json!({
            "reuse": self.reuse,
            "simulation_id": format!(
                "m8.3.1-ab-{}-{BLOCK}",
                if self.reuse { "cached" } else { "baseline" }
            ),
            "rpc_count": self.sequence.len(),
            "state_read_count": self.state_reads,
            "method_counts": per_method,
            "method_sequence": self.sequence,
            "cache_hits": self.stats.total_hits(),
            "cache_misses": self.stats.total_misses(),
            "per_kind": self.stats.to_json(),
            // With the switch off, an account triple never passed the boundary to be
            // looked up, so this arm's tally counts only the reads HEAD already looked up
            // (storage and the direct bytecode path). It is reported as a partial account
            // rather than as a zero on the kinds that were not measured.
            "tally_scope": if self.reuse {
                "every state read of the four kinds was looked up here"
            } else {
                "only the reads M8.2's HEAD already looked up: eth_getStorageAt and the \
                 direct bytecode path; an account triple went straight to the node"
            },
            "rpc_wall_ns": self.rpc_wall.as_nanos(),
            "run_wall_ns": self.run_wall.as_nanos(),
            "result": self.identity(),
        })
    }
}

/// Run the pinned M4 route once, with reuse either on or off, against a fresh endpoint
/// holding the recorded state.
///
/// Nothing varies between the two calls below but `reuse`: same route, same header, same
/// ask, same pin, same adapter code, same read order (§6).
async fn run_arm(header: BlockContext, route: PricedRoute, reuse: bool) -> Arm {
    let state = Arc::new(ServedState::load());
    let stub = Stub::spawn(Arc::clone(&state));
    let base = HttpChainAdapter::connect(stub.url())
        .await
        .expect("the stub answers eth_chainId, which is all a connect does");
    assert_eq!(
        base.chain_id(),
        CHAIN,
        "the stub is the chain the route is on"
    );

    // The sink is attached to both arms. M8.2 §18 proved a traced path issues no call the
    // untraced path did not, so the same instrument on both sides costs the comparison
    // nothing, and it is the only way this test can name an RPC wall duration per arm.
    let sink = RpcTraceSink::new(
        Instant::now(),
        format!(
            "m8.3.1-ab-{}-{BLOCK}",
            if reuse { "cached" } else { "baseline" }
        ),
        RpcTraceSource::Fixture,
        Some(CHAIN.0),
    );
    let adapter: Arc<dyn ChainAdapter> = ChainAdapter::with_rpc_trace(&base, sink.clone())
        .expect("an http source has calls to record");

    let pin = BlockPin::new(BlockNumber(BLOCK), state.block_hash);
    let provider = Arc::new(RpcStateProvider::with_state_read_reuse(adapter, pin, reuse));
    let shared: Arc<dyn StateProvider> = provider.clone();
    let started = Instant::now();
    let result = run(
        shared,
        &request(route, header, U256::ONE, provider.source()),
    )
    .await
    .expect("the recorded state replays this route");
    let run_wall = started.elapsed();

    let calls = sink.events();
    let arrivals = stub.methods();
    // Three accounts of the same calls, before any arithmetic: the endpoint's arrivals,
    // the sink's events, and the reuse boundary's counters are reconciled below only if
    // the first two already agree on which calls happened in which order.
    assert_eq!(
        arrivals.first().map(String::as_str),
        Some("eth_chainId"),
        "the first thing any arm does is open a connection"
    );
    let wire = arrivals[1..].to_vec();
    assert_eq!(
        calls
            .iter()
            .map(|event| event.method.as_str())
            .collect::<Vec<_>>(),
        wire.iter().map(String::as_str).collect::<Vec<_>>(),
        "the sink and the endpoint disagree about which calls this arm made"
    );
    assert!(
        calls.iter().all(|event| event.success),
        "a call this arm made did not succeed: {:?}",
        calls
            .iter()
            .filter(|event| !event.success)
            .map(|event| (&event.method, &event.error_detail))
            .collect::<Vec<_>>()
    );

    let mut method_counts: BTreeMap<String, usize> = BTreeMap::new();
    for method in &wire {
        *method_counts.entry(method.clone()).or_default() += 1;
    }
    let state_reads = STATE_METHODS
        .iter()
        .map(|method| method_counts.get(*method).copied().unwrap_or(0))
        .sum();
    let header_reads = method_counts
        .get("eth_getBlockByNumber")
        .copied()
        .unwrap_or(0);
    // Saturating, not a plain sum: an evidence line that says "the calls of this arm took
    // at least this long" is worth more than a panic over an arithmetic edge that a run
    // of 117 calls cannot reach anyway.
    let rpc_wall = Duration::from_nanos(
        calls
            .iter()
            .fold(0u64, |total, event| total.saturating_add(event.duration_ns)),
    );
    Arm {
        reuse,
        sequence: wire,
        keys: calls
            .iter()
            .map(|event| (event.method.clone(), event.dedup_key.clone()))
            .collect(),
        method_counts,
        state_reads,
        header_reads,
        stats: provider.state_read_stats(),
        rpc_wall,
        run_wall,
        result,
    }
}

/// §7, §8, §12 and §15 together on one historical simulation: the request counter says
/// reuse asked the node for less, the reuse boundary's own tally says by exactly how
/// much, and the run is the same run.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reuse_answers_the_second_read_and_leaves_the_run_unchanged() {
    let fixture = support::Fixture::load().await;
    let baseline = run_arm(fixture.header.clone(), fixture.route.clone(), false).await;
    let cached = run_arm(fixture.header.clone(), fixture.route.clone(), true).await;

    // ---- §5: the thing an optimization must not touch, compared in full ----------------
    // Field by field first, so a failure names which fact moved; then the whole struct,
    // which is the stricter claim and the one this type can make without a new canonical
    // form of my own (§5 prefers an existing stricter representation, and `fingerprint`
    // is that: it covers every field of the result, down to a single log byte).
    assert_eq!(
        cached.identity(),
        baseline.identity(),
        "status / revert / gas / return data / profit did not survive reuse identically"
    );
    assert_eq!(
        cached.result, baseline.result,
        "reuse changed the simulation's answer, not just how it got there"
    );
    assert_eq!(
        cached.result.fingerprint(),
        baseline.result.fingerprint(),
        "the fingerprint covers every field of the result, so a difference anywhere — \
         revert status, gas used, return data, profit — shows here first"
    );

    // ---- §7: the request counter, read off the endpoint --------------------------------
    println!(
        "arm A (reuse off): {} state reads, {} total, fingerprint {}",
        baseline.state_reads,
        baseline.sequence.len(),
        baseline.result.fingerprint()
    );
    println!(
        "arm B (reuse on):  {} state reads, {} total, {} answered from an earlier read, \
         fingerprint {}",
        cached.state_reads,
        cached.sequence.len(),
        cached.stats.total_hits(),
        cached.result.fingerprint()
    );
    println!(
        "  reads removed from the wire by this milestone: {}",
        baseline.state_reads - cached.state_reads
    );
    for method in STATE_METHODS {
        let tally = match method {
            "eth_getCode" => cached.stats.code,
            "eth_getBalance" => cached.stats.balance,
            "eth_getTransactionCount" => cached.stats.nonce,
            _ => cached.stats.storage,
        };
        println!(
            "  {method:<26} baseline {:>3}  cached {:>3}  hits {:>3}  misses {:>3}",
            baseline.method_count(method),
            cached.method_count(method),
            tally.hits,
            tally.misses,
        );
    }
    println!(
        "  eth_getBlockByNumber       baseline {:>3}  cached {:>3}",
        baseline.header_reads, cached.header_reads
    );
    println!(
        "  rpc wall                   baseline {:>13?}  cached {:>13?}",
        baseline.rpc_wall, cached.rpc_wall
    );
    println!(
        "  run wall                   baseline {:>13?}  cached {:>13?}",
        baseline.run_wall, cached.run_wall
    );

    assert!(
        cached.state_reads < baseline.state_reads,
        "reuse asked for no fewer state reads: baseline {} vs cached {}",
        baseline.state_reads,
        cached.state_reads
    );
    // §1: only the three account methods change. Storage already had a full key at HEAD,
    // and the header read keeps its behaviour, so both arms must pay the same for them.
    assert_eq!(
        cached.method_count("eth_getStorageAt"),
        baseline.method_count("eth_getStorageAt"),
        "storage reuse was not this milestone's variable, so its count moved"
    );
    assert_eq!(
        cached.header_reads, baseline.header_reads,
        "§1 leaves eth_getBlockByNumber alone"
    );
    for method in ["eth_getCode", "eth_getBalance", "eth_getTransactionCount"] {
        assert!(
            cached.method_count(method) < baseline.method_count(method),
            "{method}: cached {} vs baseline {}",
            cached.method_count(method),
            baseline.method_count(method)
        );
    }

    // ---- §7: the method sequence, compared rather than counted -------------------------
    // The cached arm's calls are the baseline's in the same order with some removed: no
    // reordering, no batching, no prefetch (§13). A removed call is a *whole* call, so
    // what is left is a subsequence of the arrivals, not a rewritten list.
    assert!(
        is_subsequence(&cached.sequence, &baseline.sequence),
        "the cached arm made a call the baseline arm did not make, or made one out of order"
    );
    // And each key the cached arm did ask for, it asked for once: a second arrival of the
    // same §3 identity inside one simulation is the duplicate this milestone exists to end.
    let mut seen: BTreeMap<&str, Vec<Option<String>>> = BTreeMap::new();
    for (method, key) in &cached.keys {
        if STATE_METHODS.contains(&method.as_str()) {
            let listed = seen.entry(method).or_default();
            assert!(
                !listed.contains(key),
                "{method} reached the wire twice for the same key {key:?} in one simulation"
            );
            listed.push(key.clone());
        }
    }

    // ---- §12: the tally reconciles with the counter, not with itself ------------------
    assert!(cached.stats.reuse, "the cached arm reports the switch off");
    assert!(
        !baseline.stats.reuse,
        "the baseline arm reports the switch on, so its own tally is not the one §12 asks for"
    );
    // In the cached arm every read that reached the wire is one the boundary looked up and
    // did not have: `misses` per kind equals that method's request count, which is the
    // alignment §12 asks for — checked against a counter of arrivals, not against itself.
    for (kind, tally, method) in [
        ("code", cached.stats.code, "eth_getCode"),
        ("balance", cached.stats.balance, "eth_getBalance"),
        ("nonce", cached.stats.nonce, "eth_getTransactionCount"),
        ("storage", cached.stats.storage, "eth_getStorageAt"),
    ] {
        assert_eq!(
            tally.misses,
            cached.method_count(method),
            "{kind}: the tally says {tally:?} while the endpoint took {} {method} calls",
            cached.method_count(method)
        );
    }
    // The two arms were asked the same questions in the same order — §6's single-variable
    // comparison — so a read either reached the endpoint or was answered here, and that
    // total is the same on both sides. What moves between arms is only which side of the
    // line the answer came from.
    for (kind, method, baseline_kind, cached_kind) in [
        (
            "code",
            "eth_getCode",
            baseline.stats.code,
            cached.stats.code,
        ),
        (
            "balance",
            "eth_getBalance",
            baseline.stats.balance,
            cached.stats.balance,
        ),
        (
            "nonce",
            "eth_getTransactionCount",
            baseline.stats.nonce,
            cached.stats.nonce,
        ),
        (
            "storage",
            "eth_getStorageAt",
            baseline.stats.storage,
            cached.stats.storage,
        ),
    ] {
        assert_eq!(
            baseline.method_count(method) + baseline_kind.hits,
            cached.method_count(method) + cached_kind.hits,
            "{kind}: the arms were asked a different number of {method} questions \
             ({} vs {}), which is a second variable, not the one this milestone owns",
            baseline.method_count(method) + baseline_kind.hits,
            cached.method_count(method) + cached_kind.hits,
        );
    }
    // The duplicates M8.2 counted are the calls this arm no longer makes — with one
    // exception that is not this milestone's: `eth_getStorageAt` already had a full
    // four-field key at HEAD, so both arms pay for it identically and its 21 calls are
    // the residue §1 leaves in place.
    let removed = baseline.state_reads - cached.state_reads;
    let answers_reused = cached.stats.total_hits() - baseline.stats.total_hits();
    assert_eq!(
        removed, answers_reused,
        "{removed} calls disappeared from the wire while {answers_reused} reads were \
         answered from this simulation's own earlier read — the two accounts of the \
         saving disagree"
    );
    assert_eq!(
        baseline.stats.balance.hits + baseline.stats.nonce.hits,
        0,
        "with reuse off, an account's balance and nonce never reached the boundary at all; \
         a non-zero tally here means arm A is not M8.2's HEAD"
    );
    assert_eq!(
        baseline.stats.storage, cached.stats.storage,
        "storage reuse is not this milestone's variable, and its tally moved"
    );

    // ---- §10 at the wire: nothing was asked for a height this endpoint does not hold ---
    // The stub would have answered any other height with an error and the run would have
    // refused; this assertion names the same fact in the record rather than in a failure.
    for (method, key) in &cached.keys {
        if let Some(key) = key {
            let height = key
                .split('|')
                .nth(2)
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert_eq!(
                height,
                BLOCK.to_string(),
                "{method} was asked for a block other than the pin: {key}"
            );
        }
    }

    if let Some(dir) = std::env::var_os("M831_AB_EVIDENCE") {
        write_evidence(&std::path::PathBuf::from(dir), &baseline, &cached);
    }
}

/// §11 and §2 at the wire: two simulations over one endpoint, both with reuse on.
///
/// The cache lives in the provider and a provider is one simulation, so the second
/// simulation pays for its own state exactly as the first did. A process-wide map would
/// show up here as a cheaper second run — and, worse, as a run that reads a block the
/// world may no longer be at.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_second_simulation_over_the_same_endpoint_pays_for_its_own_reads() {
    let fixture = support::Fixture::load().await;
    let first = run_arm(fixture.header.clone(), fixture.route.clone(), true).await;
    let second = run_arm(fixture.header.clone(), fixture.route.clone(), true).await;

    assert_eq!(
        first.state_reads, second.state_reads,
        "the second simulation asked the node for {} state reads where the first asked \
         for {} — reuse crossed the simulation boundary",
        second.state_reads, first.state_reads
    );
    assert_eq!(
        first.stats, second.stats,
        "the two simulations' tallies differ: {:?} vs {:?}",
        first.stats, second.stats
    );
    assert_eq!(
        first.result.fingerprint(),
        second.result.fingerprint(),
        "the same simulation run twice did not produce the same answer"
    );
}

fn is_subsequence(short: &[String], long: &[String]) -> bool {
    let mut cursor = 0usize;
    for method in short {
        loop {
            if cursor >= long.len() {
                return false;
            }
            let matched = long[cursor] == *method;
            cursor += 1;
            if matched {
                break;
            }
        }
    }
    true
}

/// The A/B as a file, written from the numbers these two arms just produced (§19: no
/// hand-typed counts). Only the test's own run writes this, and only when told where.
fn write_evidence(dir: &std::path::Path, baseline: &Arm, cached: &Arm) {
    std::fs::create_dir_all(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    let document = json!({
        "schema": "m8.3.1-ab-stub/v1",
        "generated_by": "cargo test -p evm-simulation --test state_reuse_ab",
        "state_source": format!(
            "local JSON-RPC stub serving {}",
            support::DUMP
        ),
        "chain_id": CHAIN.0,
        "block": BLOCK,
        "block_hash": format!("{:?}", cached.result.block.hash),
        "ask": "1 wei (the smallest output that is still a trade)",
        "variable": "state read reuse",
        "baseline": baseline.row(),
        "cached": cached.row(),
        "comparison": {
            "result_equal": cached.result == baseline.result,
            "fields_equal": cached.identity() == baseline.identity(),
            "fingerprint_equal":
                cached.result.fingerprint() == baseline.result.fingerprint(),
            "baseline_request_count": baseline.state_reads,
            "cached_request_count": cached.state_reads,
            "state_reads_removed": baseline.state_reads - cached.state_reads,
            // The saving as the boundary itself reports it: arm B answered this many more
            // reads from an earlier read of the same simulation than arm A did. Arm A's
            // own tally already includes the reuse HEAD had (storage and the direct
            // bytecode path), so the two accounts only line up after that subtraction.
            "answers_reused_added":
                cached.stats.total_hits() - baseline.stats.total_hits(),
            "per_method_removed": {
                "eth_getCode": baseline.method_count("eth_getCode")
                    - cached.method_count("eth_getCode"),
                "eth_getBalance": baseline.method_count("eth_getBalance")
                    - cached.method_count("eth_getBalance"),
                "eth_getTransactionCount": baseline.method_count("eth_getTransactionCount")
                    - cached.method_count("eth_getTransactionCount"),
                "eth_getStorageAt": baseline.method_count("eth_getStorageAt")
                    - cached.method_count("eth_getStorageAt"),
                "eth_getBlockByNumber": baseline.header_reads - cached.header_reads,
            },
            "rpc_wall_removed_ns":
                baseline.rpc_wall.as_nanos().saturating_sub(cached.rpc_wall.as_nanos()),
            "run_wall_removed_ns":
                baseline.run_wall.as_nanos().saturating_sub(cached.run_wall.as_nanos()),
        },
    });
    let path = dir.join("m8.3.1-ab.json");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&document).expect("serializable")
        ),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    println!("wrote {}", path.display());
}
