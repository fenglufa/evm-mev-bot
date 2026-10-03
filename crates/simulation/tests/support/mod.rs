//! The stages both of M4's end-to-end tests need, kept in one place so the live
//! run and the offline replay are provably the *same* simulation: same block
//! capture, same detector call, same route, same transaction shape. Only the state
//! source differs — one reads the archive node at the pin, the other reads the dump
//! of those exact reads.
//!
//! This is a module, not a test target: integration tests in `tests/` are separate
//! crates, and each one pulls this in with `mod support;`.
//!
//! Which also means each one compiles *all* of it, and no single test target uses
//! every helper here — `report` belongs to the replay suite, and the refusal tests
//! never print a report. Hence the allow below rather than a duplicate of these
//! stages in every file.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::{Address, U256};

use evm_chain::RecordedChainAdapter;
use evm_core::{BlockNumber, ChainId};
use evm_graph::MarketGraphBuilder;
use evm_opportunity::{detect_opportunities, swap_exact_in, Hop, Opportunity, PricedHop};
use evm_protocol::{Registry, V2Adapter};
use evm_replay::ReplayEngine;
use evm_simulation::{
    engine::run, route::RouteLeg, DumpStateProvider, EvmRules, GasPricing, PricedRoute,
    SimulationRequest, SimulationResult, StateProvider, StateSpec, TransactionSpec,
};
use evm_state::InMemoryStateStore;

pub const CHAIN: ChainId = ChainId(91342);
pub const BLOCK: u64 = 37_191_169;

/// The JSON-RPC endpoint both M8.3.1's and M8.3.3's offline arms read through, so the two
/// milestones are provably asking the same recorded state.
pub mod stub;

pub const TOKEN_WETH: Address = Address::new([
    0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x06,
]);

/// The state the offline tests run against: the dump the live run froze out of the
/// archive node's answers at this block.
pub const DUMP: &str = "fixtures/simulation-m4/dump-37191169.json";

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

pub fn dump_path() -> PathBuf {
    workspace_root().join(DUMP)
}

/// The opportunity the detector reports for this block, with its own reserves and
/// attested fee — the same route `crates/opportunity/tests/real_data.rs` verifies,
/// reached the same way rather than transcribed from its constants.
pub async fn opportunity() -> Opportunity {
    let registry = Registry::load_dir(&workspace_root().join("data/protocols-m3"))
        .expect("the committed M3 registry loads and validates");
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real-m3"), CHAIN)
        .expect("the captured block loads");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    engine
        .replay_block(BlockNumber(BLOCK), &mut report)
        .await
        .expect("the block replays");
    let snapshot = MarketGraphBuilder::new()
        .build(&engine.snapshot())
        .expect("both pools synced in this block");
    let detection = detect_opportunities(&snapshot).expect("detection");
    *detection
        .opportunities
        .iter()
        .find(|o| o.input_token.address == TOKEN_WETH)
        .expect("a route spends WETH on this block")
}

/// The same finding as something an EVM can be handed.
pub fn route(opportunity: &Opportunity) -> PricedRoute {
    let [first, second] = opportunity.hops;
    let [first_hop, second_hop] = opportunity.path.hops();
    let leg = |hop: Hop, priced: PricedHop| RouteLeg {
        pool: priced.pool,
        token_in: hop.token_in,
        token_out: hop.token_out,
        reserve_in: priced.reserve_in,
        reserve_out: priced.reserve_out,
        fee: priced.fee,
    };
    let mid = swap_exact_in(
        first.reserve_in,
        first.reserve_out,
        first.fee,
        opportunity.input_amount,
    )
    .expect("the first hop quotes");
    PricedRoute::new(
        opportunity.chain_id,
        opportunity.block_number,
        leg(first_hop, first),
        leg(second_hop, second),
        opportunity.input_amount,
        mid,
        opportunity.output_amount,
        opportunity.gross_profit,
        format!(
            "evm-opportunity::detect_opportunities on block {}, search {:?}",
            opportunity.block_number.0, opportunity.search.strategy,
        ),
    )
    .expect("a route the detector produced is executable-shaped")
}

/// §58's sender is funded for a plan of this shape: native in, wrapped, traded,
/// unwrapped — so the run can also make §32's denomination proof.
///
/// `state_source` is the identity of whatever answers the reads: the live run names
/// the node, the offline run names the fixture file, and §62's one-provider rule is
/// checked against this string by the engine.
pub fn request(
    route: PricedRoute,
    header: evm_chain::BlockContext,
    asked_output: U256,
    state_source: String,
) -> SimulationRequest {
    request_with_gas(
        route,
        header,
        asked_output,
        state_source,
        TransactionSpec::DEFAULT_GAS_LIMIT_PER_STEP,
    )
}

/// The same request with the §28 allowance chosen by the caller. A run at a lower
/// ceiling is how §41 gets tested: the plan, the ask, the state and the price are
/// all untouched, so the only thing the answer can be reporting is the allowance.
pub fn request_with_gas(
    route: PricedRoute,
    header: evm_chain::BlockContext,
    asked_output: U256,
    state_source: String,
    gas_limit_per_step: u64,
) -> SimulationRequest {
    let transaction = TransactionSpec::canonical(
        evm_simulation::Funding::WrapNative,
        evm_simulation::Settle::UnwrapInputToken,
        asked_output,
    );
    let pricing = GasPricing::Eip1559 {
        priority_fee_per_gas: 0,
        provenance: format!(
            "block {BLOCK}'s own base fee as the header reports it, with no tip: §44 takes the \
             bidding question off the table, and a hypothetical transaction on a historical \
             block is not competing to be included in it"
        ),
    };
    SimulationRequest::new(
        header,
        route,
        TransactionSpec {
            rules: EvmRules::Prague,
            gas_limit_per_step,
            ..transaction
        },
        StateSpec::new(state_source),
        pricing,
    )
    .expect("a valid request")
}

/// Everything a report needs about one run, printed rather than asserted. The
/// assertions live in the tests; this is the part a human reads.
pub fn report(label: &str, result: &SimulationResult) {
    println!("--- {label} ---");
    println!("{}", result.summary());
    let logs: usize = result.steps.iter().map(|step| step.logs.len()).sum();
    let topics: usize = result
        .steps
        .iter()
        .flat_map(|step| step.logs.iter())
        .map(|log| log.topics.len())
        .sum();
    println!(
        "  {logs} logs across {} steps, {topics} topics in them",
        result.steps.len()
    );
    for step in &result.steps {
        println!(
            "  step {} {:<28} gas {:>8} {:?} {}",
            step.index,
            step.signature,
            step.gas_used,
            step.status,
            step.measured
                .as_ref()
                .map(|m| format!("{} = {}", m.binding, m.value))
                .unwrap_or_default()
        );
        // §4/§56: the target, the native value and the calldata are what make a step
        // auditable as a transaction rather than as a line in a summary, so a report
        // prints them next to the step that carried them.
        println!(
            "      to {} value {} calldata {} bytes, selector {}",
            step.to,
            step.value,
            step.calldata.len(),
            step.selector,
        );
    }
    println!("  compared: {:?}", result.compared);
    println!("  outcome: {:?}", result.outcome);
    println!("  net profit: {:?}", result.net_profit);
    println!("  fingerprint: {}", result.fingerprint());
}

/// The run's inputs, all of them derived: the route from the detector, the header
/// from the fixture, the state source from the fixture's own path.
///
/// Shared by every offline target — the execution suite and the risk suite decide on
/// the same runs, so a difference between the two can only be in the question asked
/// of the result, never in the result.
pub struct Fixture {
    pub provider: Arc<DumpStateProvider>,
    pub route: PricedRoute,
    pub header: evm_chain::BlockContext,
    pub source: String,
}

impl Fixture {
    pub async fn load() -> Self {
        let path = dump_path();
        assert!(
            path.exists(),
            "{} is missing. It is written by the live run:
    cargo test -p evm-simulation --test real_chain -- --ignored --nocapture
    This suite is the offline half of that run and cannot stand without it.",
            path.display()
        );
        let provider = Arc::new(
            DumpStateProvider::from_file(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
        );
        let opportunity = opportunity().await;
        let route = route(&opportunity);
        let header = provider
            .header()
            .await
            .expect("the fixture carries the header it was read under");
        let source = provider.source();
        assert_eq!(
            header.number, route.block_number,
            "§15: the state and the opportunity are the same block"
        );
        Self {
            provider,
            route,
            header,
            source,
        }
    }

    pub fn shared(&self) -> Arc<dyn StateProvider> {
        let provider: Arc<DumpStateProvider> = Arc::clone(&self.provider);
        provider
    }

    pub fn spec(&self, asked_output: U256) -> SimulationRequest {
        request(
            self.route.clone(),
            self.header.clone(),
            asked_output,
            self.source.clone(),
        )
    }

    pub async fn run_at(&self, asked_output: U256) -> SimulationResult {
        run(self.shared(), &self.spec(asked_output))
            .await
            .unwrap_or_else(|error| panic!("the run at ask {asked_output} refused: {error}"))
    }

    /// The same run with §28's per-step allowance chosen by the test. Nothing else
    /// moves: same route, same ask, same state, same price.
    pub async fn run_with_gas(
        &self,
        asked_output: U256,
        gas_limit_per_step: u64,
    ) -> SimulationResult {
        let request = request_with_gas(
            self.route.clone(),
            self.header.clone(),
            asked_output,
            self.source.clone(),
            gas_limit_per_step,
        );
        run(self.shared(), &request)
            .await
            .unwrap_or_else(|error| panic!("the run at {gas_limit_per_step} gas refused: {error}"))
    }

    /// The same run with §29's price declared as unknown. Everything else — route,
    /// ask, state, allowance, funding — is the run the priced tests use, so the only
    /// thing this variation can change is what the result says about cost.
    pub async fn run_unpriced(&self, asked_output: U256) -> SimulationResult {
        let mut spec = self.spec(asked_output);
        spec.pricing = GasPricing::Unresolved {
            reason: "the test declares no price for this run".to_string(),
        };
        run(self.shared(), &spec)
            .await
            .unwrap_or_else(|error| panic!("the unpriced run refused: {error}"))
    }
}
