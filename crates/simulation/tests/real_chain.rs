//! M4's §67 chain, end to end on one real block: the captured block becomes a
//! graph, the graph becomes M3's opportunity, the opportunity becomes a simulation
//! of the deployed bytecode against the *node's* state at that block's end, and
//! every read the node served is frozen into a fixture on the way out.
//!
//! ```text
//! cargo test -p evm-simulation --test real_chain -- --ignored --nocapture
//! ```
//!
//! It is `#[ignore]`d because it is the only thing in this crate that touches a
//! network (§49 forbids simulating against anything but an explicit pinned block,
//! and the four validation gates must not depend on a node being up). The dump it
//! writes is what the offline tests in `dump_replay.rs` run against, so the fixture
//! and this run cannot drift: they are the same reads, recorded once.
//!
//! Nothing in here is a number someone typed. The route comes from the detector, the
//! header from the node, and the node's address from
//! `data/simulation-m4/execution-evidence.json` — including the block hash this run
//! checks the node's answer against (§20) before executing anything.

use std::sync::Arc;

use alloy_primitives::U256;

use evm_chain::{ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;
use evm_simulation::{
    engine::run, BlockPin, DumpStateProvider, ExecutionStatus, RpcStateProvider, StateProvider,
};

mod support;
use support::{dump_path, report, request, route, BLOCK, CHAIN};

/// The evidence file M4-Step1 wrote: the RPC endpoint, and the header hash that
/// pins this run's state.
fn evidence() -> serde_json::Value {
    let path = support::workspace_root().join("data/simulation-m4/execution-evidence.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&raw).expect("the evidence file is JSON")
}

fn rpc_url() -> String {
    evidence()["chain"]["rpc"]
        .as_str()
        .expect("the evidence file names an rpc")
        .to_string()
}

fn pinned_hash() -> String {
    evidence()["state_pin"]["block_hash"]
        .as_str()
        .expect("the evidence file records the pinned block hash")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live archive node; writes the fixture the offline tests use"]
async fn simulate_the_real_route_and_freeze_the_state_it_read() {
    let opportunity = support::opportunity().await;
    let route = route(&opportunity);
    println!("{opportunity}");
    println!(
        "analytical: input {} mid {} output {} gross profit {}",
        route.input_amount,
        route.analytical_mid_amount,
        route.analytical_output,
        route.analytical_gross_profit,
    );

    let adapter = HttpChainAdapter::connect(&rpc_url())
        .await
        .expect("the evidence file's rpc connects");
    let pin = BlockPin::new(
        BlockNumber(BLOCK),
        pinned_hash().parse().expect("a block hash"),
    );
    let header = adapter
        .get_block_context(BlockNumber(BLOCK))
        .await
        .expect("the node answers for the pinned block");
    assert_eq!(
        header.chain_id, CHAIN,
        "the rpc is the chain the route is on"
    );
    assert_eq!(
        format!("{:?}", header.hash),
        pinned_hash(),
        "the node's block {BLOCK} is the block the evidence file pinned"
    );

    // One provider for the whole ladder: its caches mean a rung costs the node no
    // read another rung already caused, and its recording is the union of every read
    // the run needed, which is what makes the fixture re-executable offline.
    let provider = Arc::new(RpcStateProvider::new(Arc::new(adapter), pin));
    let shared: Arc<dyn StateProvider> = provider.clone();
    let source = provider.source();

    // Rung one: ask M3's analytical output, the zero-slippage form of §21. If the
    // route pays as predicted, this is also the answer.
    let exact = run(
        Arc::clone(&shared),
        &request(
            route.clone(),
            header.clone(),
            route.analytical_output,
            source.clone(),
        ),
    )
    .await
    .expect("the analytical-ask run produces a result or a refusal, not a crash");
    report("analytical ask", &exact);

    // Rung two: ask the smallest amount that is still a trade. The second leg can
    // only fail for a reason the pool itself supplies, so whatever this run hands
    // back is the sequence's own measurement of what it can reach.
    let floor = run(
        Arc::clone(&shared),
        &request(route.clone(), header.clone(), U256::ONE, source),
    )
    .await
    .expect("the floor-ask run produces a result or a refusal, not a crash");
    report("ask of one", &floor);

    let dump = provider.dump();
    let path = dump_path();
    std::fs::create_dir_all(path.parent().expect("fixtures/simulation-m4")).expect("dir");
    dump.write_file(&path).expect("the dump writes");
    println!(
        "wrote {} ({} accounts, {} storage slots, {} reads recorded)",
        path.display(),
        dump.accounts.len(),
        dump.storage
            .values()
            .map(std::collections::BTreeMap::len)
            .sum::<usize>(),
        dump.reads.len(),
    );
    assert!(
        matches!(exact.status, ExecutionStatus::Completed)
            || matches!(floor.status, ExecutionStatus::Completed),
        "neither rung completed, so nothing was executed and the fixture would record no \
         trade: {:?}",
        exact.status,
    );

    // §62's claim — the fixture and this run cannot drift, because they are the same
    // reads — as an assertion rather than a comment: the floor ask runs a second time
    // against the dump this provider recorded, and the two results are compared in full.
    // `state_source` is the one field the replay cannot share, since it names which
    // object answered; the engine refuses a request that names a source other than the
    // provider it runs on, so the field is rewritten only after being checked to differ.
    let replay_source = format!("dump-of-this-run:{}", path.display());
    let replay: Arc<dyn StateProvider> =
        Arc::new(DumpStateProvider::new(dump.clone(), replay_source.clone()));
    let replayed = run(
        replay,
        &request(route.clone(), header.clone(), U256::ONE, replay_source),
    )
    .await
    .expect("the dump this run recorded replays it");
    assert_ne!(
        replayed.state_source, floor.state_source,
        "the replay reads the dump and the live run read the node — that difference is \
         the one thing about the replay that is not a copy of the run"
    );
    let mut replayed = replayed;
    replayed.state_source = floor.state_source.clone();
    assert_eq!(
        replayed, floor,
        "the fixture does not replay the run it was recorded from"
    );
    println!(
        "the fixture replays this run exactly: {} ({} reads, {} steps)",
        floor.fingerprint(),
        dump.reads.len(),
        floor.steps.len(),
    );
}
