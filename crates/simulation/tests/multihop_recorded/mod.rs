//! The recorded GIWA market, as the three real-execution files see it.
//!
//! `tests/multihop_revm.rs` proves the recorded route runs and files; `tests/
//! multihop_negative_controls.rs` asks §46's planted failures of the same market; and
//! `tests/multihop_determinism.rs` walks §47's chain twice. All three have to be talking about
//! one thing, so none of them builds it separately: the market, the route, the candidate, the
//! run spec, the filing and §26–§30's policy and context live here. What differs between the
//! files is which field they move, never how the untouched record is assembled.
//!
//! ## Where each number comes from
//!
//! ```text
//! reserves            the recording's own balance words (pool_balance, no constant)
//! graph + route       market_on / MultiHopRoute::new — the pipeline's builder
//! quote, legs         price() and legs(), i.e. Estimated
//! deployment, wallet  fixtures/simulation-m10/*, declared row by row (CONTROLLED_FIXTURE)
//! delivery, gas       the EVM's answer, i.e. Simulated
//! ```
//!
//! The header, the chain, the pools, the tokens and their reserves are GIWA's; the executor
//! deployment, the operator's balance and the approvals are declared scaffolding. That is why
//! [`fixture`] hands over a state source starting with `CONTROLLED_FIXTURE` and nothing here
//! publishes a real-market verdict (§41).
//!
//! Included by more than one test binary, so each one sees the others' helpers as unused. The
//! same allowance the sibling fixture modules carry.
#![allow(dead_code)]

use alloy_primitives::U256;
use evm_core::{BlockNumber, Fee, PoolId};
use evm_graph::GraphSnapshot;
use evm_opportunity::{MultiHopRoute, OptimizedCandidate};
use evm_pathfinder::{find_cycles, CycleCandidate, FeeStatus, PathFinderConfig};
use evm_simulation::executor::{run, ExecutorOutcome, ExecutorRun};
use evm_simulation::state::StateDump;
use evm_simulation::{
    executor_run, EvmRules, GasPricing, MultiHopBuildError, RunConfig, SimulatedOpportunity,
};

use evm_execution::{
    executable_plan, ExecutablePlan, ExecutionBinding, MarketKind, MultihopBinding,
    MultihopPlanContext, MultihopPlanRefusal, SenderFunding,
};
use evm_risk::{MarketFacts, MultihopAcceptance, MultihopRiskDecision, MultihopRiskPolicy};

use crate::executor_state::{
    amount_in, pool_balance, Fixture, Knobs, BLOCK, CHAIN, ENDOWMENT, EXECUTOR, FIXTURE_LABEL,
    GAS_LIMIT, MID, POOL_A, POOL_B, WETH,
};
use crate::multihop_market::{at_amount, edge_on, market_on, reserves};

// ---------------------------------------------------------------------------
// The recorded market, projected as a graph
// ---------------------------------------------------------------------------

/// The two recorded pools with the fee both of them are attested at — or with `fee_b` missing,
/// which is §46's `NC4` and nothing else's use.
///
/// `token0` is the mid token because `0x07d4… < 0x4200…`, which is the pair's own ordering
/// and the one the runs compare against (§16's rule: the pool answers, the caller does not
/// assume). Nothing here types in a reserve: a wrong slot makes `pool_balance` panic rather
/// than silently price a different market.
pub fn recorded_market_at(dump: &StateDump, fee_b: Option<Fee>) -> GraphSnapshot {
    market_on(
        CHAIN,
        BLOCK,
        &[
            reserves(
                POOL_A,
                MID,
                WETH,
                pool_balance(dump, MID, POOL_A),
                pool_balance(dump, WETH, POOL_A),
                Some(crate::multihop_market::FEE),
            ),
            reserves(
                POOL_B,
                MID,
                WETH,
                pool_balance(dump, MID, POOL_B),
                pool_balance(dump, WETH, POOL_B),
                fee_b,
            ),
        ],
    )
}

/// The recorded market at the fee M10 already proved by execution.
pub fn recorded_market(dump: &StateDump) -> GraphSnapshot {
    recorded_market_at(dump, Some(crate::multihop_market::FEE))
}

/// `WETH → pool A → MID → pool B → WETH`: the recorded route, entered where M10 enters it.
///
/// The route is rebuilt from the same `fee_b` the market is projected with, so a test that
/// removes pool B's attestation gets a route whose second edge genuinely carries no fee — §46's
/// `NC4` cannot be answered by a route assembled from a different graph than the one under test.
pub fn recorded_route_at(dump: &StateDump, fee_b: Option<Fee>) -> MultiHopRoute {
    MultiHopRoute::new(
        &recorded_market_at(dump, fee_b),
        &[
            edge_on(CHAIN, POOL_A, WETH, MID),
            edge_on(CHAIN, POOL_B, MID, WETH),
        ],
    )
    .expect("the recorded pair closes on WETH")
}

/// The recorded route at the fee both pools are attested at — [`recorded_route_at`]'s normal case.
pub fn recorded_route(dump: &StateDump) -> MultiHopRoute {
    recorded_route_at(dump, Some(crate::multihop_market::FEE))
}

/// The candidate: the recorded route priced at exactly the amount the fixture stakes.
///
/// The search domain is the single point `amount_in()`, so `best_input` is that amount and
/// `best_output` is the quote's own answer — no downstream stage gets to disagree about which
/// input is being discussed (§20's leg checks are about *these* numbers).
pub fn candidate(dump: &StateDump) -> OptimizedCandidate {
    at_amount(&recorded_route(dump), amount_in())
}

/// The recorded pair as M9.3's search reports it instead of as edges a test typed.
///
/// Selection is by pool set, start token and fee status — the same semantic keys
/// `tests/multihop_e2e.rs` selects by — because the search also reports this cycle walked the
/// other way, so a control that picked by position would change meaning when the graph grew.
/// §47 asks its five claims of the pair in both forms: entered from hand-written edges, and
/// entered from a candidate the search found.
pub fn recorded_cycle_candidate(dump: &StateDump) -> CycleCandidate {
    let wanted = [PoolId::new(CHAIN, POOL_A), PoolId::new(CHAIN, POOL_B)];
    let found = recorded_cycles(dump);
    let matches: Vec<CycleCandidate> = found
        .into_iter()
        .filter(|candidate| {
            candidate.hop_count == wanted.len()
                && candidate.pools() == wanted.to_vec()
                && candidate.start_token.address == WETH
                && candidate.fee_status == FeeStatus::Complete
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one candidate walks the recorded pair entered at WETH",
    );
    matches.into_iter().next().expect("the one match")
}

/// The whole reported set for the recorded market, unfiltered — the object §47's "same
/// `CycleCandidate`" claim is actually about, including the order the search returns it in.
pub fn recorded_cycles(dump: &StateDump) -> Vec<CycleCandidate> {
    find_cycles(
        &recorded_market(dump),
        &PathFinderConfig::new(MIN_CYCLE_HOPS),
    )
    .unwrap_or_else(|error| panic!("the search refused the recorded pair: {error}"))
}

/// The recorded route entered from a search candidate rather than from a test's own edge list.
/// Same builder, same graph, same order of edges — which is the whole of §47's first claim.
pub fn recorded_route_from_candidate(
    dump: &StateDump,
    candidate: &CycleCandidate,
) -> MultiHopRoute {
    MultiHopRoute::new(&recorded_market(dump), candidate.edges.as_slice())
        .expect("the candidate is a route on the graph it was found in")
}

/// The depth M9.3's search is asked to close at for this pair: two hops is the smallest cycle
/// the pathfinder honours, and it is what the recorded pair is.
pub const MIN_CYCLE_HOPS: usize = 2;

/// A candidate on the recorded route at a stake a test chooses — §46's `NC10` needs two real
/// runs of one market, and a second stake is the only difference between them that the pricing
/// itself produces.
pub fn candidate_at(dump: &StateDump, amount: U256) -> OptimizedCandidate {
    at_amount(&recorded_route(dump), amount)
}

// ---------------------------------------------------------------------------
// The request and the run
// ---------------------------------------------------------------------------

/// The run spec M11 asks for: the same addresses, gas limit, rules, pricing and endowment
/// M10's [`Fixture::spec`] builds, with the route and the block now coming out of the
/// candidate. §23's claim is that the caller has no field to restate them with.
pub fn config(fx: &Fixture) -> RunConfig {
    RunConfig {
        state_source: fx.source.clone(),
        executor: EXECUTOR,
        operator: fx.knobs.caller,
        recipient: fx.knobs.recipient,
        gas_limit: GAS_LIMIT,
        rules: EvmRules::Prague,
        pricing: GasPricing::Eip1559 {
            priority_fee_per_gas: 0,
            provenance: format!(
                "block {BLOCK}'s own base fee as the recorded header reports it, with no tip: a \
                 hypothetical transaction on a historical block is not competing to be included \
                 in it"
            ),
        },
        endowment: Some(ENDOWMENT),
    }
}

/// The request, at the tightest guard the pricing can claim.
pub fn request(fx: &Fixture, candidate: &OptimizedCandidate) -> ExecutorRun {
    executor_run(candidate, &config(fx), candidate.search.best_output).unwrap_or_else(|error| {
        panic!("the recorded candidate refused to build a request: {error}")
    })
}

/// The same request, asking for the refusal instead of panicking — for the test that wants to
/// read §46's `NC8` answer rather than trip an assertion.
pub fn try_request(
    fx: &Fixture,
    candidate: &OptimizedCandidate,
    guard: U256,
) -> Result<ExecutorRun, MultiHopBuildError> {
    executor_run(candidate, &config(fx), guard)
}

/// The committed fixture: the recording plus exactly the rows M10 declared.
pub fn fixture() -> Fixture {
    Fixture::committed()
}

/// The fixture with one knob moved, everything else as [`Knobs::standard`] states it.
pub fn fixture_with(move_knob: impl FnOnce(&mut Knobs)) -> Fixture {
    let mut knobs = Knobs::standard();
    move_knob(&mut knobs);
    Fixture::build(knobs)
}

/// One run, through M10's own entry point. A harness refusal is returned as an error rather
/// than swallowed: §71 keeps "the EVM refused to answer" apart from "the EVM answered: no".
pub async fn ask(fx: &Fixture, spec: &ExecutorRun) -> Result<ExecutorOutcome, String> {
    run(fx.provider(), spec)
        .await
        .map_err(|error| format!("the harness refused to run: {error}"))
}

pub async fn run_ok(fx: &Fixture, spec: &ExecutorRun) -> ExecutorOutcome {
    ask(fx, spec).await.expect("the harness ran")
}

/// File a run under the candidate it is claimed to be the run of. A refusal is a test failure
/// here: the files that use this want the record, and the refusals are the subject of
/// `tests/multihop_negative_controls.rs`, which calls [`try_file`] for them.
pub fn file(
    priced: &OptimizedCandidate,
    spec: &ExecutorRun,
    outcome: ExecutorOutcome,
) -> SimulatedOpportunity {
    try_file(priced, spec, outcome)
        .unwrap_or_else(|error| panic!("the run was refused as not the candidate's: {error}"))
}

/// The filing decision itself, for a test whose subject *is* the refusal.
pub fn try_file(
    priced: &OptimizedCandidate,
    spec: &ExecutorRun,
    outcome: ExecutorOutcome,
) -> Result<SimulatedOpportunity, MultiHopBuildError> {
    SimulatedOpportunity::new(priced, spec, outcome)
}

/// The whole §24 object for one run of the recorded route.
pub async fn simulate(fx: &Fixture) -> SimulatedOpportunity {
    let priced = candidate(&fx.dump);
    let spec = request(fx, &priced);
    let outcome = run_ok(fx, &spec).await;
    file(&priced, &spec, outcome)
}

/// The contract's own named error, or a sentence saying the revert was not the contract's.
pub fn blame(outcome: &ExecutorOutcome) -> String {
    match &outcome.contract_error {
        Some(name) => name.clone(),
        None => format!(
            "not the executor: {:?} — {}",
            outcome.revert_kind,
            outcome
                .revert()
                .map(|data| data.reason())
                .unwrap_or_else(|| "no revert data".to_string())
        ),
    }
}

/// §27 at the level of storage: a reverted run leaves no word different anywhere this
/// fixture watches.
pub fn assert_no_residue(outcome: &ExecutorOutcome) {
    assert!(
        !outcome.market_moved(),
        "a reverted run moved a reserve or a balance: {outcome:#?}",
    );
    for watched in [POOL_A, POOL_B, WETH, MID, EXECUTOR] {
        let changed = outcome.changed_slots_in(watched);
        assert!(
            changed.is_empty(),
            "{watched} keeps {} changed word(s) after a reverted run: {changed:?}",
            changed.len(),
        );
    }
}

// ---------------------------------------------------------------------------
// §26–§30: the policy, the facts and the plan context this market is judged with
// ---------------------------------------------------------------------------

/// §27's policy at the two numbers this run itself supplies: a profit floor of 0, so the check
/// asks only the question §27 asks (`output > input` is check 3, and check 4 is a no-op here), and
/// a gas ceiling set to the run's own measurement — the boundary case, inclusive, so the gas line
/// can never be the reason a recorded run is refused, and the ceiling still has a real number in
/// it rather than a slack one.
pub fn recorded_policy(gas_used: u64) -> MultihopRiskPolicy {
    MultihopRiskPolicy {
        minimum_gross_profit: U256::ZERO,
        maximum_gas: gas_used,
        maximum_simulation_age: 3,
        maximum_state_age: 3,
        executor: EXECUTOR,
        chain_id: CHAIN,
        provenance: "M11's recorded-market thresholds: the ceiling is this run's own measured \
                     burn, the floor is 0 because §27's `output > input` is the question asked \
                     above it, and both block facts come from the pinned header rather than from a \
                     node"
            .to_string(),
    }
}

/// [`recorded_policy`] with one threshold moved, which is the form §46's profit and gas controls
/// need: the record stays the record, the ceiling stays the run's own measured burn, and the
/// answer changes because the line moved.
pub fn policy_at(
    gas_used: u64,
    move_policy: impl FnOnce(&mut MultihopRiskPolicy),
) -> MultihopRiskPolicy {
    let mut policy = recorded_policy(gas_used);
    move_policy(&mut policy);
    policy
}

/// The two block facts, both at the header the state was loaded from: age zero, so a freshness
/// rejection can only be caused by a test moving one of them.
pub fn recorded_facts(head: BlockNumber, state_version: BlockNumber) -> MarketFacts {
    MarketFacts {
        head: Some(head),
        state_version: Some(state_version),
        provenance: format!(
            "block {BLOCK} as the recording's own header reports it; no node was asked, which is \
             what §28 requires of this layer"
        ),
    }
}

pub fn fresh_recorded() -> MarketFacts {
    recorded_facts(BlockNumber(BLOCK), BlockNumber(BLOCK))
}

/// The plan's deployment binding: the chain and executor of the recording, not of a configuration
/// an endpoint could answer for (§7/§36).
pub fn recorded_binding() -> ExecutionBinding {
    ExecutionBinding {
        chain_id: CHAIN.0,
        executor: EXECUTOR,
    }
}

/// The four facts a simulation cannot know, at the values this fixture actually holds: the sender
/// is the operator the state was funded for, the market label is `REAL_MARKET` because the pools
/// and reserves are the recording's (§29's label with its evidence), and the funding is real state
/// because the operator's WETH balance is a row in it.
pub fn recorded_context(fx: &Fixture) -> MultihopPlanContext {
    MultihopPlanContext {
        sender: fx.knobs.caller,
        correlation_id: FIXTURE_LABEL.to_string(),
        max_block_age: 3,
        validity_provenance: "§8's window as M11's fixture declares it".to_string(),
        funding: SenderFunding::RealState {
            source: "§34: the operator's balance and allowance are rows in the pinned dump, read \
                     by the run rather than written into it"
                .to_string(),
        },
        market: MarketKind::RealMarket {
            attested_by: format!(
                "chain {}, block {BLOCK}: both pools, both tokens and every reserve in this \
                 fixture come from the recording M7 froze out of the archive node",
                CHAIN.0
            ),
        },
        state_fingerprint: format!("{BLOCK}:0"),
        floor_provenance: "§12's floor, mirrored from the guard the call carries".to_string(),
    }
}

/// The acceptance the policy grants over a recorded run, at the policy the test hands it. A
/// rejection is returned as the decision, not as a panic, because §46's controls reach this
/// function to read the answer a moved threshold gives.
pub fn decide(
    policy: &MultihopRiskPolicy,
    simulated: &SimulatedOpportunity,
    facts: &MarketFacts,
) -> MultihopRiskDecision {
    policy.evaluate(simulated, facts)
}

/// The acceptance the run's own measured gas earns from [`recorded_policy`], or a panic quoting
/// the answer it gave. Used where a test wants a grant and nothing else.
pub fn recorded_acceptance(simulated: &SimulatedOpportunity) -> MultihopAcceptance {
    match recorded_policy(simulated.gas_used).evaluate(simulated, &fresh_recorded()) {
        MultihopRiskDecision::Accept(figures) => figures,
        other => panic!("the recorded run was not accepted: {other}"),
    }
}

/// The plan the recorded run binds to, or a refusal quoted. Both arms are answers: §46's
/// controls assert on the refusal and the controls' positive arm asserts on the plan.
pub fn plan_of(
    simulated: &SimulatedOpportunity,
    granted: &MultihopAcceptance,
    context: &MultihopPlanContext,
    binding: &ExecutionBinding,
) -> Result<(ExecutablePlan, MultihopBinding), MultihopPlanRefusal> {
    executable_plan(simulated, granted, context, binding)
}
