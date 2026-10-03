//! M8.4.1 §23's Trace and Instrumentation rows, at the layer that has the most to prove:
//! the simulation engine's own reads.
//!
//! §4 asks who issued every state read. The wire record can already name the method, the
//! address, the slot and the height; what it cannot infer is which part of the run asked,
//! because in this build the storage reads are demanded by the EVM one SLOAD at a time
//! from inside a REVM fiber, and the code that opens the socket never sees the plan. So
//! the engine says which phase it is in, at each of the sites that issues a read
//! (`crates/simulation/src/engine.rs`), and the provider puts that name on the calls it
//! issues until the next one. This suite checks the claim at the record rather than at the
//! stamp: it runs the real engine over a traced adapter against the recorded state and
//! reads the labels back off the sink.
//!
//! Three things are checked, and each is a different way this could be wrong:
//!
//! * **Coverage** — every call the provider issued carries a phase name, and the name is
//!   one of the ones the engine actually says (§8: the vocabulary is the code's own).
//! * **Exactness** — no record needed an ambiguity note. A note would mean two calls were
//!   open at one sink at once, or the phase moved while one was open, and then the label
//!   could not be attributed to that call alone.
//! * **Cost** — §12's rule at this layer: the same run through an untraced adapter issues
//!   the identical calls in the identical order and returns the identical result. Stamping
//!   is a label, not a request, and the arrival list at the endpoint is what says so.

use std::sync::Arc;
use std::time::Instant;

use alloy_primitives::U256;

use evm_chain::{
    ChainAdapter, HttpChainAdapter, RpcCallEvent, RpcTraceSink, RpcTraceSource, CONTEXT_NOT_STAMPED,
};
use evm_core::BlockNumber;
use evm_simulation::engine::{INTERPRETER_PHASE_PREFIXES, SLOT_AUDIT_PHASE};
use evm_simulation::{
    engine::run, BlockPin, PricedRoute, RpcStateProvider, SimulationResult, StateProvider,
};

mod support;
use support::stub::{ServedState, Stub};
use support::{request, BLOCK, CHAIN};

/// §4's `stage` for these reads, as `engine.rs` spells it.
const SIMULATION: &str = "simulation";

/// The phases the engine names. `execute:` and its step description are checked as a
/// prefix, because §8 asks for the code's own words and [`evm_simulation::ResolvedStep::describe`]
/// is a function of the plan, not a constant.
///
/// The slots audit leg is not spelled out here: the classifier on the other side of the
/// evidence (`crates/pipeline/src/diagnosis.rs`) decides a storage read's dependency from
/// this exact name, so a rename has to be one edit and one failing test rather than a
/// table that quietly starts calling proven dependencies `unknown`.
const PHASES: [&str; 9] = [
    "header at pin",
    "codes: touched_contracts",
    "code: sender",
    "account: sender",
    "views: token0()",
    "views: token1()",
    "views: getReserves()",
    "state_changes: accounts",
    SLOT_AUDIT_PHASE,
];

fn is_known_phase(caller: &str) -> bool {
    PHASES.contains(&caller)
        || INTERPRETER_PHASE_PREFIXES
            .iter()
            .any(|prefix| caller.starts_with(prefix))
        || caller.starts_with("state_changes: slot ")
}

/// One run of the pinned M4 route, traced or not, with the reuse boundary on or off.
struct Arm {
    /// What the endpoint saw, connect included, in arrival order.
    arrivals: Vec<String>,
    /// What the sink saw. Empty on an untraced arm — which is the point of §12's control.
    events: Vec<RpcCallEvent>,
    result: SimulationResult,
    /// How many bytecodes `StateProvider::codes` was asked for on this run.
    touched: usize,
}

impl Arm {
    /// The calls the provider made: everything after the connect, which no phase owns
    /// because the adapter, not the engine, issues it.
    fn provider_calls(&self) -> &[RpcCallEvent] {
        &self.events[1..]
    }

    fn caller(&self, phase: &str) -> Vec<&RpcCallEvent> {
        self.events
            .iter()
            .filter(|event| event.caller.as_deref() == Some(phase))
            .collect()
    }
}

async fn run_arm(
    state: Arc<ServedState>,
    route: PricedRoute,
    header: evm_chain::BlockContext,
    traced: bool,
    reuse: bool,
) -> Arm {
    let sink = traced.then(|| {
        RpcTraceSink::new(
            Instant::now(),
            format!(
                "m8.4.1-labels-{}-{BLOCK}",
                if reuse { "cached" } else { "serial" }
            ),
            RpcTraceSource::Fixture,
            Some(CHAIN.0),
        )
    });
    let stub = Stub::spawn(Arc::clone(&state));
    // `connect_with_trace` rather than `with_rpc_trace` after the fact, so the connect's
    // own `eth_chainId` is on the same timeline as the reads: it is the one call this run
    // makes that no engine phase asked for, and the record says so rather than being left
    // out of the count. With `traced = false` this is the same call `connect` makes.
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
    let provider = Arc::new(RpcStateProvider::with_state_read_reuse(adapter, pin, reuse));
    let touched = route.touched_contracts().len();
    let shared: Arc<dyn StateProvider> = provider.clone();
    let result = run(
        shared,
        &request(route, header, U256::ONE, provider.source()),
    )
    .await
    .expect("the recorded state replays this route");

    Arm {
        arrivals: stub.methods(),
        events: sink.map(|sink| sink.events()).unwrap_or_default(),
        result,
        touched,
    }
}

/// §4 and §8 together: every state read of a real run carries the phase that asked for
/// it, the phase is one the engine says out loud, and no record needed an ambiguity note.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn every_call_the_provider_makes_carries_the_phase_that_asked_for_it() {
    let fixture = support::Fixture::load().await;
    let arm = run_arm(
        Arc::new(ServedState::load()),
        fixture.route.clone(),
        fixture.header.clone(),
        true,
        false,
    )
    .await;

    // The sink and the endpoint are two accounts of one sequence. §12 wants them to agree
    // before any label claim is read as coverage: a call the sink missed would otherwise
    // look like a call that needed no label.
    assert_eq!(
        arm.events
            .iter()
            .map(|event| event.method.as_str())
            .collect::<Vec<_>>(),
        arm.arrivals.iter().map(String::as_str).collect::<Vec<_>>(),
        "the sink and the endpoint disagree about which calls this run made"
    );

    // The connect is the one call no phase owns, and it says which note covers it.
    let connect = &arm.events[0];
    assert_eq!(connect.method, "eth_chainId");
    assert_eq!(connect.stage, None);
    assert_eq!(connect.caller, None);
    assert_eq!(
        connect.context_note,
        Some(CONTEXT_NOT_STAMPED),
        "the connect should say it was never stamped, not borrow the first phase's name"
    );

    // Everything the provider issued after it is a simulation read with a name on it.
    for event in arm.provider_calls() {
        assert_eq!(
            event.stage.as_deref(),
            Some(SIMULATION),
            "a state read arrived without a stage: {:?}",
            (&event.method, &event.caller)
        );
        let caller = event
            .caller
            .as_deref()
            .unwrap_or_else(|| panic!("a state read arrived without a caller: {event:?}"));
        assert!(
            is_known_phase(caller),
            "{caller:?} is not a phase engine.rs names: {:?}",
            (&event.method, &event.slot)
        );
        // §4's exactness: an attributed label is only this call's label if nothing else
        // was open at the sink and the phase did not move underneath it.
        assert_eq!(
            event.context_note, None,
            "the label on {caller} cannot be attributed to one call: {event:?}"
        );
    }

    // The phases a run of this route must reach, checked as facts about the sequence
    // rather than as counts: a header read, the route's bytecodes as one batch, the
    // sender's own two reads, and the three preflight views.
    assert_eq!(
        arm.caller("header at pin")
            .iter()
            .map(|event| event.method.as_str())
            .collect::<Vec<_>>(),
        ["eth_getBlockByNumber"],
        "the run's first state read is the pinned header"
    );
    assert_eq!(
        arm.caller("codes: touched_contracts").len(),
        arm.touched,
        "the batch's bytecodes are not all attributed to the batch"
    );
    assert!(
        arm.caller("codes: touched_contracts")
            .iter()
            .all(|event| event.method == "eth_getCode"),
        "a read other than a bytecode arrived under the batch's name"
    );
    assert_eq!(
        arm.caller("code: sender")
            .iter()
            .map(|event| event.method.as_str())
            .collect::<Vec<_>>(),
        ["eth_getCode"],
        "§58's sender bytecode check is one read of its own"
    );
    assert_eq!(
        arm.caller("account: sender")
            .iter()
            .map(|event| event.method.as_str())
            .collect::<Vec<_>>(),
        ["eth_getBalance", "eth_getTransactionCount", "eth_getCode"],
        "the sender's triple is three reads, in the order the triple asks for them"
    );
    for view in ["views: token0()", "views: token1()", "views: getReserves()"] {
        let rows = arm.caller(view);
        assert!(
            !rows.is_empty(),
            "{view} ran but nothing was attributed to it"
        );
        assert!(
            rows.iter().any(|event| event.method == "eth_getStorageAt"),
            "{view} asked the state for nothing it had to read a word for: {:?}",
            rows.iter()
                .map(|event| event.method.as_str())
                .collect::<Vec<_>>()
        );
    }

    // §5's point of doing this at all: the storage reads the EVM demands are attributed
    // to a plan step, not to the phase that happened to be named before execution.
    assert!(
        arm.provider_calls()
            .iter()
            .any(|event| event.method == "eth_getStorageAt"
                && event
                    .caller
                    .as_deref()
                    .is_some_and(|caller| caller.starts_with("execute: step "))),
        "no storage read was attributed to a step, so the labels cannot be telling \
         who actually asked: {:?}",
        arm.provider_calls()
            .iter()
            .map(|event| (&event.method, event.caller.as_ref()))
            .collect::<Vec<_>>()
    );
    assert!(
        !arm.caller("state_changes: accounts").is_empty(),
        "§36's before-values are read after the run and should say so"
    );

    // §13's tenet, read off the same records: this run asked for state at one height.
    let heights: Vec<&str> = arm
        .provider_calls()
        .iter()
        .map(|event| {
            event
                .block
                .as_deref()
                .expect("a state read names its height")
        })
        .collect();
    assert!(
        heights.iter().all(|height| *height == BLOCK.to_string()),
        "a simulation read went out at a height other than the pin: {:?}",
        heights
            .iter()
            .filter(|height| **height != BLOCK.to_string())
            .copied()
            .collect::<Vec<_>>()
    );
}

/// §12 at the engine layer: the instrument adds a name to each record and nothing to the
/// wire. The untraced run is the control — same route, same pin, same endpoint shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn naming_who_asked_costs_the_run_no_call_and_changes_no_answer() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let traced = run_arm(
        Arc::clone(&state),
        fixture.route.clone(),
        fixture.header.clone(),
        true,
        false,
    )
    .await;
    let untraced = run_arm(state, fixture.route, fixture.header, false, false).await;

    assert!(
        untraced.events.is_empty(),
        "an untraced arm should record nothing, not a second account of the calls"
    );
    // The whole arrival list, in order — not its length. A stamp that issued one extra
    // `eth_getCode`, or moved a read earlier, would fail here and pass a count check.
    assert_eq!(
        untraced.arrivals, traced.arrivals,
        "stamping the phases changed which calls go out, or what order they go out in"
    );
    // §16's field list, at this layer: the labels are a description of the run, so the
    // run itself has to be the same run. The struct comparison is the strict version;
    // the fingerprint is the one that would name a single changed log byte.
    assert_eq!(untraced.result, traced.result);
    assert_eq!(
        untraced.result.fingerprint(),
        traced.result.fingerprint(),
        "the instrumented run answered differently"
    );
}

/// §28's direction for a label this run did not make: a read issued while no phase had
/// been named records that, rather than keeping whatever name the sink last held.
///
/// This is the rule that makes an unlabeled row in the evidence mean "the run did not say
/// who asked" instead of "some earlier phase asked for it" — and it only holds because a
/// cached read returns before the stamp, so a label can never outlive the statement that
/// set it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_read_issued_before_any_phase_is_named_says_so_instead_of_inheriting_one() {
    let state = Arc::new(ServedState::load());
    let stub = Stub::spawn(Arc::clone(&state));
    let sink = RpcTraceSink::new(
        Instant::now(),
        format!("m8.4.1-unstamped-{BLOCK}"),
        RpcTraceSource::Fixture,
        Some(CHAIN.0),
    );
    let base = HttpChainAdapter::connect_with_trace(stub.url(), Some(sink.clone()))
        .await
        .expect("the stub answers eth_chainId");
    let provider = RpcStateProvider::new(
        Arc::new(base) as Arc<dyn ChainAdapter>,
        BlockPin::new(BlockNumber(BLOCK), state.block_hash),
    );

    // First, nothing named: the read goes out and the record carries no stage, no caller,
    // and the note that says this run did not name one.
    let route = support::Fixture::load().await.route;
    let pool = route.legs[0].pool.address;
    provider
        .storage(pool, U256::from(4u64))
        .await
        .expect("the recorded state answers this word");
    let events = sink.events();
    let unstamped = events
        .iter()
        .find(|event| event.method == "eth_getStorageAt")
        .expect("the read went out and was recorded");
    assert_eq!(unstamped.stage, None);
    assert_eq!(unstamped.caller, None);
    assert_eq!(unstamped.context_note, Some(CONTEXT_NOT_STAMPED));

    // Then a named phase, and the next read of a different word carries it with no note.
    provider.note_read_phase("state_changes: slot 5 by hand");
    provider
        .storage(pool, U256::from(5u64))
        .await
        .expect("the recorded state answers this word too");
    let after = sink.events();
    let labeled = after
        .iter()
        .filter(|event| event.method == "eth_getStorageAt")
        .nth(1)
        .expect("the second word also went out");
    assert_eq!(labeled.stage.as_deref(), Some(SIMULATION));
    assert_eq!(
        labeled.caller.as_deref(),
        Some("state_changes: slot 5 by hand")
    );
    assert_eq!(labeled.context_note, None);

    // Two words, two requests, one connect: the stamping is not a batch and not a retry.
    assert_eq!(
        stub.methods(),
        vec![
            "eth_chainId".to_string(),
            "eth_getStorageAt".to_string(),
            "eth_getStorageAt".to_string(),
        ],
        "naming a phase issued something other than the reads asked for"
    );
}

/// The labels survive the reuse boundary: a cached read issues nothing, so it also gets
/// nothing labeled, and every call that does go out still names who asked for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn reuse_asks_for_fewer_calls_and_labels_every_one_that_still_goes_out() {
    let fixture = support::Fixture::load().await;
    let state = Arc::new(ServedState::load());
    let serial = run_arm(
        Arc::clone(&state),
        fixture.route.clone(),
        fixture.header.clone(),
        true,
        false,
    )
    .await;
    let cached = run_arm(state, fixture.route, fixture.header, true, true).await;

    for event in cached.provider_calls() {
        assert_eq!(event.stage.as_deref(), Some(SIMULATION));
        assert!(
            is_known_phase(event.caller.as_deref().unwrap_or_default()),
            "reuse left a call with a caller that is not a phase the engine says: {event:?}"
        );
        assert_eq!(
            event.context_note, None,
            "a call this run made cannot be attributed to one phase: {event:?}"
        );
    }
    assert!(
        cached.arrivals.len() < serial.arrivals.len(),
        "reuse asked for no fewer calls, so this arm cannot show a cached read is \
         an absent record rather than an unlabeled one: {} vs {}",
        cached.arrivals.len(),
        serial.arrivals.len()
    );
    // §16 again, one switch apart: the answer is the answer.
    assert_eq!(cached.result.fingerprint(), serial.result.fingerprint());
    // And every call the cached arm made is a call the serial arm made, in the same order
    // — M8.3.1 §13's shape rule, here so that a label moving between phases cannot hide
    // behind "reuse removed it".
    let mut serial_calls = serial.arrivals.iter();
    for method in &cached.arrivals {
        assert!(
            serial_calls.any(|candidate| candidate == method),
            "the cached arm made a {method} call the serial arm did not make"
        );
    }
}
