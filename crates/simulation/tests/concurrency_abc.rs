//! M8.3.3 §17's fixed-block correctness fixture — and, in the same three runs, §12's
//! proof that the concurrency this milestone measures actually happened.
//!
//! ```text
//! the variable          one number: how many of this simulation's independent state
//!                       reads may be outstanding at the node at once (1, 2, 4)
//! what must not move    the simulation's answer, field by field, and the calls it made
//!                       to get there
//! the block             37 191 169, the M4 pin, through the endpoint in
//!                       `support::stub` — the same recorded answers M8.3.1's reuse A/B
//!                       ran against, so a difference between the two milestones can only
//!                       be the bound
//! ```
//!
//! §17 forbids comparing profit alone, and §34 makes a difference here fatal rather than
//! interesting. So each arm publishes a named list of the fields a wrong cache key or a
//! reordered read could move, the three arms are compared on all of them, and *then* the
//! whole result struct and its fingerprint are compared — the stricter claim, which is
//! what makes the named list a readable failure rather than the whole of the proof.
//!
//! §12 asks for the concurrency to be proven at the wire and §16 forbids an instrument
//! that manufactures its own measurement, so each arm carries three independent accounts
//! of the same instants and none of them can inflate the others:
//!
//! ```text
//! the scheduler          its own gauge, bracketing each dispatch it was given
//! the RPC trace          the intervals of the calls that reached the wire, swept by
//!                        `evm_pipeline::diagnosis::timeline` — the same function that
//!                        produced M8.3.2's committed tables (§14: no second recorder)
//! the endpoint           how many connections it was holding open at once, counted on
//!                        the server side of the wire after the request body was read
//! ```
//!
//! A configured bound is never treated as a result (§15): `configured_concurrency` and
//! `observed_max_concurrency` are separate fields in every row, and the arm that asked
//! for 4 and came back serial would be reported as serial.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use serde_json::{json, Value};

use evm_chain::{
    BlockContext, ChainAdapter, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource,
};
use evm_core::BlockNumber;
use evm_pipeline::diagnosis::{timeline, RpcTimeline, SimulationWindow};
use evm_simulation::{
    engine::run, BlockPin, ConcurrencyReport, PricedRoute, RpcStateProvider, SimulationResult,
    StateProvider, StateReadStats,
};

mod support;
use support::stub::{Arrival, ServedState, Stub};
use support::{request, workspace_root, BLOCK, CHAIN};

/// The four methods whose reads this milestone may put outstanding together, in the
/// reporting order M8.3.1 used.
const STATE_METHODS: [&str; 4] = [
    "eth_getCode",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getStorageAt",
];

/// §2's arms, in order. C8 is deliberately absent: §13 lets it exist only after these
/// three pass, and it is a fourth live arm, not a fourth row of this fixture.
const ARMS: [usize; 3] = [1, 2, 4];

/// M8.3.1's controlled stub arm, read from the evidence it wrote rather than from a
/// number typed here. §10's rule is that a bound of 1 reproduces the baseline; on this
/// endpoint the baseline is that arm, and the two must agree call for call.
const M831_CONTROLLED: &str = "data/evidence/m8/optimization/raw/controlled/m8.3.1-ab.json";

/// The address shape a negative test uses: nothing in the recorded dump, so this endpoint
/// has no answer for it and says so rather than inventing one.
const UNKNOWN_ACCOUNT: Address = Address::new([0xaa; 20]);

/// A second one, so a batch has a member after the failure and the fold can be seen to
/// pick the first error rather than the last.
const UNKNOWN_OTHER: Address = Address::new([0xbb; 20]);

/// §17's field list, named in one place. [`Arm::identity`] builds the row with `json!`, so the
/// names it writes are checked against this list, and the positive control below probes this
/// list rather than a shorter one it invented. A fixture that stopped comparing a field would
/// otherwise keep printing `true` for the fields it still has.
const IDENTITY_FIELDS: [&str; 21] = [
    "chain_id",
    "block_number",
    "block_hash",
    "state_source",
    "status",
    "reverted",
    "revert",
    "gas_used",
    "gas_charge",
    "outcome",
    "compared",
    "measurements",
    "steps",
    "log_count",
    "gross_profit",
    "gross_loss",
    "net_profit",
    "state_changes",
    "slippage",
    "plan_summary",
    "fingerprint",
];

/// The comparison this fixture writes and §41 keeps committed. The positive control reads its
/// field list off this file, so the control runs against the evidence the milestone actually
/// published instead against a list typed twice.
const COMMITTED_COMPARISON: &str = "data/evidence/m8/concurrency/correctness-comparison.json";

/// One §17 field across the arms, as the comparison publishes it: whether every arm answered
/// the same, and the serial arm's value beside the verdict so a reader can see what was equal.
fn field_verdict(values: &[Value]) -> Value {
    json!({
        "identical_across_bounds": values.windows(2).all(|pair| pair[0] == pair[1]),
        "value_c1": values.first().cloned().unwrap_or(Value::Null),
    })
}

/// Every field of the first row, walked and judged. Keys are taken from row one: a row that
/// lost a field reads as `Null` against the others and is therefore a difference, not a silent
/// absence.
fn compare_fields(rows: &[Value]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let names = rows
        .first()
        .and_then(Value::as_object)
        .expect("a row of §17's fields")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for name in names {
        let values = rows
            .iter()
            .map(|row| row.get(&name).cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>();
        out.insert(name, field_verdict(&values));
    }
    out
}

fn label(bound: usize) -> String {
    format!("c{bound}")
}

fn simulation_id(bound: usize) -> String {
    format!("m8.3.3-{}-{BLOCK}", label(bound))
}

/// One arm: the pinned simulation at one bound, plus everything three witnesses and the
/// endpoint said about it.
struct Arm {
    bound: usize,
    /// The calls this arm made, connect excluded, in arrival order at the endpoint.
    sequence: Vec<String>,
    method_counts: BTreeMap<String, usize>,
    /// The four state methods, counted off the endpoint's arrivals.
    state_reads: usize,
    header_reads: usize,
    events: Vec<RpcCallEvent>,
    /// What the endpoint itself recorded, including the height each call sent.
    arrivals: Vec<Arrival>,
    /// The sweep of the wire intervals, by the pipeline's own analyzer.
    timeline: RpcTimeline,
    concurrency: ConcurrencyReport,
    stub_peak: usize,
    stats: StateReadStats,
    run_wall: Duration,
    result: SimulationResult,
}

impl Arm {
    fn method_count(&self, method: &str) -> usize {
        self.method_counts.get(method).copied().unwrap_or(0)
    }

    /// §17's field list, read off the result. Every name here is one the task book
    /// prints, and each is a fact a stale bytecode, a stale balance or a slot read out of
    /// order could change.
    ///
    /// §17 also names return data. This build has no raw-return-data field to compare:
    /// a step keeps what the EVM answered as `measured` (the decoded value of the binding
    /// that step produced) and as `logs`, and the bytes behind them are covered by
    /// [`Arm::whole_result`], which is the whole struct including every step.
    fn identity(&self) -> Value {
        json!({
            "chain_id": self.result.chain_id.0,
            "block_number": self.result.block.number.0,
            "block_hash": format!("{:?}", self.result.block.hash),
            "state_source": self.result.state_source.clone(),
            "status": format!("{:?}", self.result.status),
            "reverted": self.result.revert().is_some(),
            "revert": format!("{:?}", self.result.revert()),
            "gas_used": self.result.gas_used(),
            "gas_charge": format!("{:?}", self.result.gas_charge),
            "outcome": format!("{:?}", self.result.outcome),
            "compared": format!("{:?}", self.result.compared),
            "measurements": self
                .result
                .measurements
                .iter()
                .map(|m| format!("{}={}", m.binding, m.value))
                .collect::<Vec<_>>(),
            "steps": self
                .result
                .steps
                .iter()
                .map(|step| {
                    json!({
                        "index": step.index,
                        "status": format!("{:?}", step.status),
                        "signature": step.signature.clone(),
                        "selector": step.selector.clone(),
                        "to": format!("{}", step.to),
                        "value": step.value.to_string(),
                        "calldata": hex::encode(&step.calldata),
                        "gas_used": step.gas_used,
                        "measured": step
                            .measured
                            .as_ref()
                            .map(|m| format!("{}={}", m.binding, m.value)),
                        "logs": step
                            .logs
                            .iter()
                            .map(|log| format!("{:?}", log))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>(),
            "log_count": self.result.logs().len(),
            "gross_profit": self.result.gross_profit.map(|amount| amount.to_string()),
            "gross_loss": self.result.gross_loss.map(|amount| amount.to_string()),
            "net_profit": format!("{:?}", self.result.net_profit),
            "state_changes": format!("{:?}", self.result.state_changes),
            "slippage": format!("{:?}", self.result.slippage),
            "plan_summary": format!("{:?}", self.result.plan_summary),
            "fingerprint": self.result.fingerprint(),
        })
    }

    /// The stricter claim: every field of the result, including the ones §17 did not
    /// think to name. Serialized by the same code `fingerprint()` hashes, so this and the
    /// fingerprint cannot disagree.
    fn whole_result(&self) -> Value {
        serde_json::to_value(&self.result)
            .unwrap_or_else(|error| panic!("the result of a finished run must serialize: {error}"))
    }

    /// The arm's three accounts of how much it ran at once, side by side, plus the
    /// sentence saying which one a reader should trust for what.
    fn witnesses(&self) -> Value {
        json!({
            "configured_concurrency": self.concurrency.configured,
            "observed_max_concurrency": self.concurrency.observed_peak,
            "wire_max_concurrency": self.timeline.max_concurrency,
            "endpoint_held_connections_peak": self.stub_peak,
            "wire_overlap_duration_ns": self.timeline.overlap_duration_ns,
            "wire_union_duration_ns": self.timeline.union_duration_ns,
            "wire_sum_duration_ns": self.timeline.sum_duration_ns,
            "wire_serial_or_overlap": self.timeline.serial_or_overlap(),
            "note": "configured is what the run was told and proves nothing on its own \
                     (§15); observed_max_concurrency is this simulation's scheduler \
                     bracketing the reads it was handed, so a read answered from the \
                     reuse boundary is not in it; wire_max_concurrency and the overlap \
                     beside it are swept from the RPC trace's own intervals and count \
                     requests that reached the endpoint; held_connections_peak is counted \
                     server-side and can be lowered by a slow client but not raised by \
                     either instrument.",
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
            // §30's provenance, on the same terms the live runs carry it: which arm of
            // which milestone produced this row, and which commit the binary that ran it
            // was built from. `git_revision` is baked at build time by design, so a row
            // names the code that made it rather than whatever the checkout says today.
            "run_id": format!("fixed-block-{}", label(self.bound)),
            "commit": evm_pipeline::latency::git_revision(),
            "arm": label(self.bound),
            "simulation_id": simulation_id(self.bound),
            "state_read_reuse": self.stats.reuse,
            "rpc_count": self.sequence.len(),
            "state_read_count": self.state_reads,
            "method_counts": per_method,
            "method_sequence": self.sequence,
            // Which clock the list above is ordered by, so a reader comparing two arms'
            // sequences knows whether a difference is a finding or the fixture's design:
            // bound 1 has one order for the whole run, a concurrent arm has an accept order
            // here and a completion order in `calls[]`, and those two differ on purpose.
            "method_sequence_order": if self.bound == 1 {
                "accept order, which at this bound is also completion and record order"
            } else {
                "the endpoint's accept order; this arm's completion order is calls[].rpc_id"
            },
            "cache_hits": self.stats.total_hits(),
            "cache_misses": self.stats.total_misses(),
            "per_kind": self.stats.to_json(),
            "concurrency": serde_json::to_value(&self.concurrency)
                .expect("a report of integers and strings serializes"),
            "witnesses": self.witnesses(),
            "rpc_timeline": self.timeline.to_json(),
            "endpoint": {
                "responses_held_ns": support::stub::RESPONSE_DELAY.as_nanos() as u64,
                "arrivals": self.arrivals.len(),
                "held_ns_min": self
                    .arrivals
                    .iter()
                    .map(|arrival| arrival.held_ns)
                    .min()
                    .unwrap_or(0),
                "held_ns_max": self
                    .arrivals
                    .iter()
                    .map(|arrival| arrival.held_ns)
                    .max()
                    .unwrap_or(0),
            },
            "run_wall_ns": self.run_wall.as_nanos(),
            "calls": self
                .events
                .iter()
                .map(|event| {
                    json!({
                        "rpc_id": event.rpc_id,
                        "method": event.method.clone(),
                        "block": event.block.clone(),
                        "target": event.target.clone(),
                        "started_ns": event.started_ns,
                        "finished_ns": event.finished_ns,
                        "success": event.success,
                        "attempts": event.attempts.len(),
                    })
                })
                .collect::<Vec<_>>(),
            "result": self.identity(),
        })
    }
}

/// Run the pinned M4 route once at one bound, against a fresh endpoint holding the
/// recorded state.
///
/// The only input that differs between the three calls in the test below is `bound`.
async fn run_arm(header: BlockContext, route: PricedRoute, bound: usize) -> Arm {
    let state = Arc::new(ServedState::load());
    let stub = Stub::spawn_concurrent(Arc::clone(&state));
    let origin = Instant::now();
    let sink = RpcTraceSink::new(
        origin,
        simulation_id(bound),
        RpcTraceSource::Fixture,
        Some(CHAIN.0),
    );
    let base = HttpChainAdapter::connect(stub.url())
        .await
        .expect("the stub answers eth_chainId, which is all a connect does");
    assert_eq!(
        base.chain_id(),
        CHAIN,
        "the stub is the chain the route is on"
    );
    let adapter: Arc<dyn ChainAdapter> = ChainAdapter::with_rpc_trace(&base, sink.clone())
        .expect("an http source has calls to record");

    let pin = BlockPin::new(BlockNumber(BLOCK), state.block_hash);
    let provider = Arc::new(RpcStateProvider::with_state_read_concurrency(
        adapter, pin, true, bound,
    ));
    let shared: Arc<dyn StateProvider> = provider.clone();
    let started = Instant::now();
    let result = run(
        shared,
        &request(route, header, U256::ONE, provider.source()),
    )
    .await
    .expect("the recorded state replays this route");
    let run_wall = started.elapsed();

    let events = sink.events();
    let arrivals = stub.calls();
    // The window is the arm's whole span on the sink's own clock, from before the
    // connection was opened to after the run finished, so no call is clipped out of it and
    // `calls_clipped == 0` below is the check that the claim held.
    let window = SimulationWindow {
        simulation_id: simulation_id(bound),
        source: RpcTraceSource::Fixture,
        chain_id: Some(CHAIN.0),
        block_number: Some(BLOCK),
        state_source: Some(provider.source()),
        started_ns: 0,
        finished_ns: origin.elapsed().as_nanos() as u64,
    };
    let timeline = timeline(&window, &events);

    let sequence: Vec<String> = arrivals
        .iter()
        .skip(1)
        .map(|arrival| arrival.method.clone())
        .collect();
    let mut method_counts: BTreeMap<String, usize> = BTreeMap::new();
    for method in &sequence {
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
    assert_eq!(
        arrivals.first().map(|arrival| arrival.method.as_str()),
        Some("eth_chainId"),
        "the first thing any arm does is open a connection"
    );
    // The endpoint's arrivals and the trace's events are two accounts of the same calls;
    // §16's rule that an instrument must not add a call is checked at this boundary, per
    // arm, before any concurrency figure is read from either.
    //
    // How strong that check can be depends on the bound, and stating which is which is the
    // honest version of it. At bound 1 the client sends one read and waits for it, so the
    // order the endpoint accepted them in and the order the sink recorded them in are one
    // sequence — and that sequence is the thing §10 requires to equal M8.3.1's, so it is
    // compared position by position. At bounds 2 and 4 the arm has no single order: the
    // endpoint notes a request when it accepts one, the sink notes one when it finishes, and
    // the responses here are deliberately held open so that overlap can be measured at all.
    // Two correct concurrent arms therefore need not agree on a sequence, and asserting that
    // they do would test thread scheduling rather than the experiment. What is asserted at
    // every bound is the weaker-sounding but load-bearing claim: the two accounts name the
    // same *calls* — same count, and per state read the same (method, address), so an arm
    // that asked about a different contract would not pass.
    let endpoint_account = |arrival: &Arrival| {
        (
            arrival.method.clone(),
            if STATE_METHODS.contains(&arrival.method.as_str()) {
                arrival
                    .params
                    .get(0)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            } else {
                String::new()
            },
        )
    };
    let sink_account = |event: &RpcCallEvent| {
        (
            event.method.clone(),
            if STATE_METHODS.contains(&event.method.as_str()) {
                event.target.clone().unwrap_or_default()
            } else {
                String::new()
            },
        )
    };
    let mut from_endpoint: Vec<(String, String)> =
        arrivals.iter().skip(1).map(endpoint_account).collect();
    let mut from_sink: Vec<(String, String)> = events.iter().map(sink_account).collect();
    assert_eq!(
        from_sink.len(),
        from_endpoint.len(),
        "§16: bound {bound}'s sink recorded {} calls while the wire carried {}",
        from_sink.len(),
        from_endpoint.len(),
    );
    from_endpoint.sort();
    from_sink.sort();
    assert_eq!(
        from_sink, from_endpoint,
        "the sink and the endpoint disagree about which calls bound {bound} made"
    );
    if bound == 1 {
        assert_eq!(
            events
                .iter()
                .map(|event| event.method.as_str())
                .collect::<Vec<_>>(),
            sequence.iter().map(String::as_str).collect::<Vec<_>>(),
            "§10: bound 1's calls must arrive, finish, and get recorded in one and the same \
             order — it is the serial arm, and this ordering is the only one its evidence \
             claims"
        );
    }
    assert!(
        events.iter().all(|event| event.success),
        "a call this arm made did not succeed: {:?}",
        events
            .iter()
            .filter(|event| !event.success)
            .map(|event| (&event.method, &event.error_detail))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        timeline.calls_clipped, 0,
        "a call fell outside the window this arm was measured over"
    );
    assert_eq!(
        timeline.total_attempts, timeline.total_calls,
        "§25: an arm that retried a call would make `attempts` and `calls` differ, and this \
         fixture's comparison is about reads, not retries"
    );
    assert_eq!(
        provider.state_read_concurrency().configured,
        bound,
        "the provider does not report the bound it was given"
    );

    Arm {
        bound,
        sequence,
        method_counts,
        state_reads,
        header_reads,
        events,
        arrivals,
        timeline,
        concurrency: provider.state_read_concurrency(),
        stub_peak: stub.concurrent_peak(),
        stats: provider.state_read_stats(),
        run_wall,
        result,
    }
}

/// §17 and §34: three bounds, one block, one route — the answer identical, the calls
/// identical, and the concurrency real.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_bound_answers_the_pinned_block_identically() {
    let fixture = support::Fixture::load().await;
    let mut arms = Vec::new();
    for bound in ARMS {
        arms.push(run_arm(fixture.header.clone(), fixture.route.clone(), bound).await);
    }
    let baseline = &arms[0];
    for arm in &arms[1..] {
        // Named first, so a failure says which fact moved; then the whole struct and the
        // fingerprint, which are the stricter claims.
        assert_eq!(
            arm.identity(),
            baseline.identity(),
            "bound {} did not reproduce bound {} on §17's field list",
            arm.bound,
            baseline.bound
        );
        assert_eq!(
            arm.whole_result(),
            baseline.whole_result(),
            "bound {} changed the result struct itself",
            arm.bound
        );
        assert_eq!(
            arm.result, baseline.result,
            "bound {} changed the simulation's answer",
            arm.bound
        );
        assert_eq!(
            arm.result.fingerprint(),
            baseline.result.fingerprint(),
            "bound {} changed the fingerprint, which covers every field of the result",
            arm.bound
        );
    }

    // ---- §23 and §16: the bound must not change which calls the run makes --------------
    // Order-free between arms, for the reason run_arm gives: a concurrent arm's sequence is
    // a property of when its held-open replies landed. The set of calls is not, and that is
    // what §23 says must stay at 39.
    let baseline_calls = {
        let mut calls = baseline.sequence.clone();
        calls.sort();
        calls
    };
    for arm in &arms {
        let mut calls = arm.sequence.clone();
        calls.sort();
        assert_eq!(
            calls, baseline_calls,
            "bound {} made a different set of calls than bound {}",
            arm.bound, baseline.bound
        );
        assert_eq!(
            arm.state_reads, baseline.state_reads,
            "bound {} asked the node for {} state reads where bound {} asked for {}",
            arm.bound, arm.state_reads, baseline.bound, baseline.state_reads
        );
        for method in STATE_METHODS {
            assert_eq!(
                arm.method_count(method),
                baseline.method_count(method),
                "{method}: bound {} vs bound {}",
                arm.bound,
                baseline.bound
            );
        }
        assert_eq!(
            arm.stats, baseline.stats,
            "bound {}'s reuse boundary tallied differently from bound {}'s",
            arm.bound, baseline.bound
        );
    }
    // The same pin M8.3.1 measured on this endpoint, call for call: a bound of 1 is that
    // arm, so the scheduler and the batched bytecode pre-read cannot have added, removed,
    // or reordered a request.
    let previous: Value = {
        let path = workspace_root().join(M831_CONTROLLED);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&raw).expect("M8.3.1's controlled A/B is JSON")
    };
    let cached = &previous["cached"];
    let expected: Vec<String> = cached["method_sequence"]
        .as_array()
        .expect("the controlled arm lists its calls")
        .iter()
        .map(|value| value.as_str().expect("a method name").to_string())
        .collect();
    assert_eq!(
        baseline.sequence, expected,
        "bound 1 does not reproduce M8.3.1's controlled reuse arm call for call"
    );
    assert_eq!(
        baseline.state_reads,
        cached["state_read_count"].as_u64().expect("a count") as usize,
        "bound 1 asks a different number of state reads than the {} M8.3.1 measured",
        cached["state_read_count"]
    );

    // ---- §18: every read names this block, and none of them asks for a moving one ------
    for arm in &arms {
        for arrival in &arm.arrivals {
            let height = match arrival.method.as_str() {
                "eth_getCode" | "eth_getBalance" | "eth_getTransactionCount" => {
                    arrival.params.get(1).and_then(Value::as_str)
                }
                // Storage puts its block argument third: address, slot, then tag. Reading
                // index 0 here would compare the address against a height and fail for a
                // reason that has nothing to do with which block was asked for.
                "eth_getStorageAt" => arrival.params.get(2).and_then(Value::as_str),
                "eth_getBlockByNumber" => arrival.params.get(0).and_then(Value::as_str),
                _ => None,
            };
            if let Some(sent) = height {
                assert_eq!(
                    sent,
                    format!("{BLOCK:#x}"),
                    "{} was sent the block argument `{sent}` on bound {}",
                    arrival.method,
                    arm.bound
                );
            }
            for param in arrival
                .params
                .as_array()
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                if let Some(text) = param.as_str() {
                    assert!(
                        !matches!(
                            text,
                            "latest" | "pending" | "safe" | "finalized" | "earliest"
                        ),
                        "bound {} sent the block tag `{text}` to {}",
                        arm.bound,
                        arrival.method
                    );
                }
            }
        }
        // The same fact as the trace records it, so an evidence reader does not have to
        // take the endpoint's word for it: the normalized block argument of every state
        // read is the pin's decimal height.
        for event in &arm.events {
            if STATE_METHODS.contains(&event.method.as_str()) {
                assert_eq!(
                    event.block.as_deref(),
                    Some(BLOCK.to_string().as_str()),
                    "{} #{} carries a block argument other than the pin: {:?}",
                    event.method,
                    event.rpc_id,
                    event.block
                );
                assert!(
                    event.dedup_key.is_some(),
                    "a state read this build cannot key is a read §18 cannot check: {:?}",
                    event.method
                );
            }
        }
        for event in &arm.events {
            let key = event.dedup_key.as_deref().unwrap_or_default();
            if let Some(height) = key.split('|').nth(2) {
                assert_eq!(
                    height,
                    BLOCK.to_string(),
                    "a keyed call asked for height {height}: {key}"
                );
            }
        }
    }

    // ---- §10 and §12: bound 1 is serial, and bounds 2 and 4 are not --------------------
    // §44: an arm that shows no overlap is a failed experiment, not evidence that
    // concurrency does not help. These assertions are the experiment's own acceptance
    // that the thing it measures happened.
    assert_eq!(
        baseline.concurrency.observed_peak, 1,
        "§10: bound 1 must be serial"
    );
    assert_eq!(
        baseline.timeline.max_concurrency, 1,
        "the wire saw more than one of bound 1's calls at once"
    );
    assert_eq!(
        baseline.timeline.overlap_duration_ns,
        Some(0),
        "bound 1 overlapped"
    );
    assert_eq!(
        baseline.stub_peak, 1,
        "the endpoint held more than one of bound 1's connections"
    );
    assert_eq!(
        baseline.timeline.serial_or_overlap(),
        Some("serial"),
        "bound 1's own sweep does not call it serial"
    );
    for arm in &arms[1..] {
        assert!(
            arm.concurrency.observed_peak <= arm.bound,
            "bound {} put {} reads outstanding, above its own bound",
            arm.bound,
            arm.concurrency.observed_peak
        );
        assert!(
            arm.timeline.max_concurrency <= arm.bound,
            "bound {} let {} calls reach the wire at once, above its own bound",
            arm.bound,
            arm.timeline.max_concurrency
        );
        assert!(
            arm.stub_peak <= arm.bound,
            "bound {} had the endpoint holding {} requests, above its own bound",
            arm.bound,
            arm.stub_peak
        );
        assert!(
            arm.concurrency.observed_peak >= 2,
            "experiment_failed (§12): bound {} never ran two reads at once — its scheduler \
             saw {}",
            arm.bound,
            arm.concurrency.observed_peak
        );
        assert!(
            arm.stub_peak >= 2,
            "experiment_failed (§12): bound {}'s reads never shared a connection slot — the \
             endpoint held {}",
            arm.bound,
            arm.stub_peak
        );
        // §36: real concurrency is an overlap *and* a union shorter than the sum.
        let overlap = arm.timeline.overlap_duration_ns.unwrap_or(0);
        assert!(
            overlap > 0,
            "experiment_failed (§12): bound {} overlapped for 0 ns",
            arm.bound
        );
        let union = arm.timeline.union_duration_ns.unwrap_or(0);
        assert!(
            arm.timeline.sum_duration_ns > union,
            "§36: bound {} overlaps for {overlap} ns while its union is not shorter than its \
             sum (union {union} of sum {}) — an overlap that saves no wall time is the two \
             accounts of this arm disagreeing, not a measurement",
            arm.bound,
            arm.timeline.sum_duration_ns,
        );
    }

    println!(
        "bound 1: {} reads, peak {} (wire {}, endpoint {}), overlap {:?} ns, run wall {:?}",
        baseline.state_reads,
        baseline.concurrency.observed_peak,
        baseline.timeline.max_concurrency,
        baseline.stub_peak,
        baseline.timeline.overlap_duration_ns,
        baseline.run_wall,
    );
    for arm in &arms[1..] {
        println!(
            "bound {}: {} reads, peak {} (wire {}, endpoint {}), overlap {:>12} ns, \
             union {:>12} ns, run wall {:>12?}  ({}‰ of bound 1's run wall)",
            arm.bound,
            arm.state_reads,
            arm.concurrency.observed_peak,
            arm.timeline.max_concurrency,
            arm.stub_peak,
            arm.timeline.overlap_duration_ns.unwrap_or(0),
            arm.timeline.union_duration_ns.unwrap_or(0),
            arm.run_wall,
            (arm.run_wall.as_nanos() * 1000 / baseline.run_wall.as_nanos()) as u64,
        );
    }

    if let Some(dir) = std::env::var_os("M833_ABC_EVIDENCE") {
        // A test binary runs with its own *package* directory as the working directory,
        // while the evidence tree lives at the workspace root beside the other milestones.
        // Resolving a relative `M833_ABC_EVIDENCE` against `workspace_root` gives the path a
        // maintainer types at a shell one meaning in both places; an absolute path still
        // means itself.
        let dir = std::path::PathBuf::from(dir);
        let dir = if dir.is_absolute() {
            dir
        } else {
            workspace_root().join(dir)
        };
        write_evidence(&dir, &arms, &previous, &expected);
        println!("fixed-block evidence written under {}", dir.display());
    }
}

/// §24 and §10 at their sharpest edge: a read that failed is not an answer, and a batch
/// that fails can ask the node for reads the serial path never reached.
///
/// The negative control is a synthetic account — `0xaaaa…` and `0xbbbb…`, neither of them
/// in the recorded dump — so this endpoint has no answer to give and returns a JSON-RPC
/// error rather than the zero nobody read. Three claims come out of the one run:
///
/// ```text
/// no caching of a failure   the same address asked twice costs two calls and never
///                           reports a hit, and the two errors are the same text
/// no default served         the error names the account, so nothing downstream can read
///                           it as an empty one
/// the extra reads are real  at bound 1 the batch stops at its first member; at bound 2
///                           and 4 it also sent the members that shared the failing
///                           chunk, which is `min(bound, 3)` calls and not a rounding
/// ```
///
/// The error the caller receives is the *first* member's at every bound, which is the
/// sequential path's `?` reproduced rather than approximated: a bound changes how many
/// requests left, never which failure the run reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_read_is_not_an_answer_and_a_failing_batch_reports_its_extra_reads() {
    // The second member is a real account from the recorded dump, so the failing chunk
    // holds a success as well: that is the shape which would let a stale or invented value
    // through if the fold read anything but the members it was given.
    let batch = [UNKNOWN_ACCOUNT, support::TOKEN_WETH, UNKNOWN_OTHER];
    let unknown = format!("{UNKNOWN_ACCOUNT:?}").to_ascii_lowercase();

    for bound in ARMS {
        let state = Arc::new(ServedState::load());
        let stub = Stub::spawn_concurrent(Arc::clone(&state));
        let base = HttpChainAdapter::connect(stub.url())
            .await
            .expect("the stub answers eth_chainId, which is all a connect does");
        let sink = RpcTraceSink::new(
            Instant::now(),
            format!("m8.3.3-negative-{}-{BLOCK}", label(bound)),
            RpcTraceSource::Fixture,
            Some(CHAIN.0),
        );
        let adapter: Arc<dyn ChainAdapter> =
            ChainAdapter::with_rpc_trace(&base, sink).expect("an http source has calls to record");
        let pin = BlockPin::new(BlockNumber(BLOCK), state.block_hash);
        let provider = RpcStateProvider::with_state_read_concurrency(adapter, pin, true, bound);

        let first = refusal(provider.code(UNKNOWN_ACCOUNT).await, UNKNOWN_ACCOUNT);
        let second = refusal(provider.code(UNKNOWN_ACCOUNT).await, UNKNOWN_ACCOUNT);
        assert_eq!(
            first, second,
            "the same failed read answered differently the second time, so the second one was \
             served from somewhere"
        );
        assert!(
            first.contains(&unknown),
            "the refusal does not name the account this endpoint has no answer for: {first}"
        );

        let batch_error = refusal(provider.codes(&batch).await, UNKNOWN_ACCOUNT);
        assert_eq!(
            batch_error, first,
            "bound {} reported a failure other than the first member's in input order, which \
             is the sequential path's `?` and nothing else",
            bound
        );

        let stats = provider.state_read_stats();
        assert_eq!(
            stats.code.hits, 0,
            "a read that failed was cached at bound {}: {:?}",
            bound, stats.code
        );
        // Two direct reads of the missing account, plus the members of the chunks that ran
        // before the batch stopped: a chunk is awaited in full and no later chunk starts, so
        // bound 1 sends one call, bound 2 two, bound 4 all three. §10 — those extra reads are
        // a fact about a failing batch that gets reported, not undone by dropping a future
        // whose request already left (§16).
        let expected_calls = 2 + bound.min(batch.len());
        let actual = stub
            .methods()
            .iter()
            .filter(|method| *method == "eth_getCode")
            .count();
        assert_eq!(
            actual, expected_calls,
            "bound {} sent {actual} `eth_getCode` calls where the chunking predicts \
             {expected_calls}",
            bound
        );
        // What the reuse boundary tallies afterwards, read against §24's definitions rather
        // than against a guess: `hits` increments on a cache lookup that returned a value and
        // `misses` on a successful read that got stored, so a failed read moves neither. That
        // makes `hits == 0` the statement that a failure was never served from cache and
        // `misses` the statement of how many members of this batch actually *succeeded* —
        // only TOKEN_WETH can, and it is batch member index 1, so it is dispatched only where
        // the first chunk is wider than one. Expecting `misses` to equal the call count here
        // would be expecting a failed read to count as a stored one, which is the exact thing
        // §24 forbids.
        let expected_stores = usize::from(bound.min(batch.len()) > 1);
        assert_eq!(
            stats.code.misses, expected_stores,
            "bound {}'s batch stored {expected_stores} code answers ({} found), while the \
             boundary reports misses {} and hits {}",
            bound, expected_stores, stats.code.misses, stats.code.hits,
        );
        assert!(
            provider.state_read_concurrency().observed_peak <= bound,
            "the negative path at bound {} ran {} reads at once",
            bound,
            provider.state_read_concurrency().observed_peak
        );
        println!(
            "negative control, bound {bound}: {actual} eth_getCode calls reached the wire, \
             {expected_stores} code answer was stored, 0 were served from cache, and the \
             batch reported the first member's error in input order"
        );
    }
}

/// The refusal a read ended with, as the text a later comparison can use.
///
/// An answer here is the failure of the test: this endpoint has no recorded answer for the
/// account it was given, so anything that comes back as `Ok` is a value somebody invented.
/// The fields a set of verdicts says differ, in the order the row holds them.
fn flagged(verdicts: serde_json::Map<String, Value>) -> Vec<String> {
    verdicts
        .iter()
        .filter(|(_, verdict)| verdict["identical_across_bounds"] != json!(true))
        .map(|(name, _)| name.clone())
        .collect()
}

/// §37's Evidence row 「fixed-block comparison catches differences」. A comparison that can only
/// print `true` is not a gate, so this is the paired non-zero probe: the same [`compare_fields`]
/// the fixture calls, run over §17's own field list read off the committed evidence, with one
/// arm's value changed at a time. Each probe has to flag exactly the field it moved.
///
/// The last probe is the one §17 is really warning about. A change buried inside one step's
/// calldata is invisible to a comparison of top-level scalars and to a comparison of profit, and
/// it has to come out as a difference anyway — which it does, because the row it lives in is one
/// of the fields being compared.
#[test]
fn the_fixed_block_comparison_names_a_field_that_moved() {
    let path = workspace_root().join(COMMITTED_COMPARISON);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error} — the fixture writes it under M833_ABC_EVIDENCE, and the committed \
             copy is what this probe reads",
            path.display()
        )
    });
    let document: Value = serde_json::from_str(&raw).expect("the committed comparison is JSON");
    let walked = document["fields"]
        .as_object()
        .expect("a field table")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut named = IDENTITY_FIELDS
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(
        walked, named,
        "the committed comparison does not walk the fields this file names, so the probe below \
         would be testing a shorter list than §17 asks for"
    );

    // One value per field, straight out of the serial arm's own row: a probe changes an answer,
    // it does not delete one.
    let base = document["fields"]
        .as_object()
        .expect("a field table")
        .iter()
        .map(|(name, row)| (name.clone(), row["value_c1"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let base = Value::Object(base);
    let verdicts = compare_fields(&[base.clone(), base.clone(), base.clone()]);
    assert_eq!(verdicts.len(), IDENTITY_FIELDS.len());
    assert!(
        verdicts
            .values()
            .all(|verdict| verdict["identical_across_bounds"] == json!(true)),
        "three identical rows did not compare as identical, which means the verdict is not a \
         function of the values beside it"
    );

    for (field, probe) in [
        ("block_number", json!(BLOCK + 1)),
        ("gas_used", json!(0u64)),
        (
            "fingerprint",
            json!("0x0000000000000000000000000000000000000000000000000000000000000000"),
        ),
        ("net_profit", json!("None")),
    ] {
        assert_ne!(
            base[field], probe,
            "the probe for {field} is the value it already holds, which would flag nothing and \
             prove nothing"
        );
        let mut moved = base.clone();
        moved[field] = probe;
        assert_eq!(
            flagged(compare_fields(&[base.clone(), moved, base.clone()])),
            vec![field.to_string()],
            "changing {field} in one arm has to be reported as that field and nothing else"
        );
    }

    let mut moved = base.clone();
    moved["steps"][0]["calldata"] = json!("0xdead0001");
    assert_ne!(
        base["steps"], moved["steps"],
        "the row this fixture wrote has no step to move, so this probe would be vacuous"
    );
    assert_eq!(
        flagged(compare_fields(&[base.clone(), moved, base.clone()])),
        vec!["steps".to_string()],
        "a step's calldata is inside `steps`, and §17 forbids reading a run as equal because \
         its profit matched"
    );
}

fn refusal<T>(outcome: evm_simulation::ProviderResult<T>, asked: Address) -> String {
    match outcome {
        Ok(_) => panic!("the endpoint was asked for {asked} and answered with a value"),
        Err(error) => error.to_string(),
    }
}

/// §17's comparison, as a file: the named fields one row each with the verdict beside
/// them, so a reader sees which field was checked and not merely that something passed.
///
/// Every verdict field is computed here from the rows beside it — nothing in this file is a
/// `true` typed by hand, because a gate a reader cannot recompute is only a claim.
fn write_evidence(dir: &std::path::Path, arms: &[Arm], committed: &Value, expected: &[String]) {
    // §29's layout: the three per-arm rows under `fixed-block/`, and the comparison beside
    // the other assembled tables at the directory root.
    let fixed = dir.join("fixed-block");
    std::fs::create_dir_all(&fixed).unwrap_or_else(|error| panic!("{}: {error}", fixed.display()));
    for arm in arms {
        let path = fixed.join(format!("{}.json", label(arm.bound)));
        write_json(&path, &arm.row());
    }

    let baseline = &arms[0];
    let rows: Vec<Value> = arms.iter().map(Arm::identity).collect();
    let mut named = IDENTITY_FIELDS
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    named.sort();
    let walked = rows[0]
        .as_object()
        .expect("a row of §17's fields")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        walked, named,
        "the row §17 compares is not the field list this file names: {walked:?} vs {named:?}"
    );
    let per_field = compare_fields(&rows);
    let document = json!({
        "schema": "m8.3.3-fixed-block/v1",
        "generated_by": "M833_ABC_EVIDENCE=<dir> cargo test -p evm-simulation --test concurrency_abc",
        "commit": evm_pipeline::latency::git_revision(),
        // §41 asks which evidence regenerates byte for byte and which does not. The claims
        // in this file that decide anything — `fields`, `whole_result`, `call_counts`, the
        // `gates` — are functions of the recorded dump and hold across runs. The nanosecond
        // figures beside them are one run's wall clock and move every time, which is why the
        // milestone's performance numbers come from the live runs and never from here.
        "stable_across_reruns": [
            "fields",
            "whole_result",
            "call_counts",
            "gates",
            "concurrency[].witnesses: configured_concurrency, observed_max_concurrency, \
             wire_max_concurrency, endpoint_held_connections_peak, wire_serial_or_overlap",
        ],
        "run_specific": [
            "concurrency[].witnesses.wire_sum_duration_ns",
            "concurrency[].witnesses.wire_union_duration_ns",
            "concurrency[].witnesses.wire_overlap_duration_ns",
            "fixed-block/<arm>.json: run_wall_ns, rpc_timeline, endpoint, calls[].*_ns",
        ],
        "milestone": "M8.3.3 §17",
        "variable": "state read concurrency bound",
        "arms": arms.iter().map(|arm| label(arm.bound)).collect::<Vec<_>>(),
        "chain_id": CHAIN.0,
        "block": BLOCK,
        "block_hash": format!("{:?}", baseline.result.block.hash),
        "ask": "1 wei (the smallest output that is still a trade)",
        "state_source": format!(
            "local JSON-RPC stub serving {}, each response held open for {} ms so an \
             overlap is measurable rather than assumed",
            support::DUMP,
            support::stub::RESPONSE_DELAY.as_millis(),
        ),
        "fields": per_field,
        "whole_result": {
            "identical": arms
                .iter()
                .all(|arm| arm.whole_result() == baseline.whole_result()),
            "fingerprint_identical": arms
                .iter()
                .all(|arm| arm.result.fingerprint() == baseline.result.fingerprint()),
            "fingerprint": baseline.result.fingerprint().to_string(),
        },
        "call_counts": {
            "state_reads_per_bound": arms
                .iter()
                .map(|arm| json!({"bound": arm.bound, "state_reads": arm.state_reads}))
                .collect::<Vec<_>>(),
            // Two fields because they are two different facts, and only the second is a gate.
            // A concurrent arm's sequence is the order the held-open replies were accepted in,
            // and thread timing can reorder that legitimately, so requiring it to match would
            // test the scheduler's luck rather than the experiment; §23's requirement is the
            // order-free set of calls. This fixture's three arms did agree on order as well, so
            // the field below is published as an observation — a `false` here would mean the
            // arms received their replies in a different order, not that they were different runs.
            "ordered_sequence_identical_to_bound_1": arms
                .iter()
                .all(|arm| arm.sequence == baseline.sequence),
            "call_multiset_identical_to_bound_1": arms.iter().all(|arm| {
                let mut actual = arm.sequence.clone();
                let mut baseline_calls = baseline.sequence.clone();
                actual.sort();
                baseline_calls.sort();
                actual == baseline_calls
            }),
            "m8_3_1_controlled_arm_comparison": {
                "file": M831_CONTROLLED,
                "committed_state_read_count": committed["cached"]["state_read_count"].clone(),
                "bound_1_state_reads": baseline.state_reads,
                "state_read_count_identical":
                    committed["cached"]["state_read_count"] == json!(baseline.state_reads),
                "bound_1_sequence_equals_committed": baseline.sequence == expected,
            },
        },
        "concurrency": arms
            .iter()
            .map(|arm| json!({"bound": arm.bound, "witnesses": arm.witnesses()}))
            .collect::<Vec<_>>(),
        "gates": {
            "every_bound_serial_at_1": baseline.timeline.max_concurrency == 1
                && baseline.concurrency.observed_peak == 1
                && baseline.stub_peak == 1,
            "overlap_positive_above_1": arms[1..].iter().all(|arm| {
                arm.timeline.overlap_duration_ns.unwrap_or(0) > 0
                    && arm.concurrency.observed_peak >= 2
                    && arm.stub_peak >= 2
            }),
            "no_bound_exceeded": arms.iter().all(|arm| {
                arm.concurrency.observed_peak <= arm.bound
                    && arm.timeline.max_concurrency <= arm.bound
                    && arm.stub_peak <= arm.bound
            }),
            "every_state_read_named_the_pin": arms.iter().all(|arm| {
                arm.events
                    .iter()
                    .filter(|event| STATE_METHODS.contains(&event.method.as_str()))
                    .all(|event| event.block.as_deref() == Some(BLOCK.to_string().as_str()))
                    && arm.arrivals.iter().all(|arrival| {
                        !arrival
                            .params
                            .as_array()
                            .map(|params| params.iter().any(|param| {
                                param
                                    .as_str()
                                    .is_some_and(|text| text == "latest" || text == "pending")
                            }))
                            .unwrap_or(false)
                    })
            }),
            "no_retry_in_this_fixture": arms
                .iter()
                .all(|arm| arm.timeline.total_attempts == arm.timeline.total_calls),
        },
    });
    write_json(&dir.join("correctness-comparison.json"), &document);
}

fn write_json(path: &std::path::Path, value: &Value) {
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(value).expect("serializable")
        ),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    println!("wrote {}", path.display());
}
