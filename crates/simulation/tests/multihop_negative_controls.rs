//! §46's planted failures, each asked of the one recorded GIWA market.
//!
//! ```text
//! cargo test -p evm-simulation --test multihop_negative_controls -- --test-threads=1
//! ```
//!
//! §43 forbids a milestone that only prints PASS, and §46 names the fifteen refusals this
//! milestone has to be able to produce. A refusal nobody can demonstrate is not a safety
//! property, it is a sentence, so each `nc0N_…` test here moves exactly one field of the
//! untouched record — the chain, the head, a fee, a reserve, a floor, a guard, an
//! acceptance, a validity window — and reads the answer the layer that owns that field
//! gives. Every one carries its positive control in the same test, because a rejection that
//! would fire whatever you moved is not a rejection, it is a broken gate. The memory of
//! M9's gate failures is the reason: an always-red control looks identical to a working one
//! until something actually needs to pass.
//!
//! ## Which layer answers which id
//!
//! ```text
//! nc01 chain        risk `chain_validity`, then M10's plan binding
//! nc02 staleness    risk `simulation_freshness`, `state_freshness`, `Unknown` when unasked
//! nc03 route shape  MultiHopRoute::new — five refusals, one per broken shape
//! nc04 fee evidence pricing `Incomplete`, optimizer `UnattestedFee`, `InvalidFee`
//! nc05 liquidity    pricing `InvalidReserve` / empty domain, then the pair's own revert
//! nc06 boundary     search domain clipped at the route's ceiling, ceiling+1 refused
//! nc07 revert       executor `PairNotAllowed`, risk check 1, plan `not_delivered`
//! nc08 min output   adapter `UnreachableGuard`, contract `FinalShortfall`, edited record
//! nc09 profit       risk floor, strict: `gain` rejects, `gain - 1` accepts
//! nc10 calldata     two real runs of one market, `calldata_hash_mismatch`
//! nc11 plan hash    two plans of one run: same bytes, different plan hash
//! nc15 expiry       the plan's own `freshness_at`, three heads, one answer each
//! ```
//!
//! `NC12` nonce collision, `NC13` capital collision and `NC14` duplicate lane belong to the
//! lane ledger and are named there — `crates/execution/tests/multihop_lanes.rs`, which is
//! also where the ledger's own expiration control sits. They are not restated here: a second
//! copy of a control that already has an id is how two files end up disagreeing about whether
//! the defect class is covered.
//!
//! ## Where each number comes from
//!
//! The market, the route, the candidate, the request, the policy and the plan context all come
//! from `mod multihop_recorded`, so the object these tests break is the object
//! `tests/multihop_revm.rs` proves works. The reserves are the recording's own balance words;
//! the pools, tokens, header and chain are GIWA's; the executor deployment, the operator's
//! balance and the approvals are the M10 fixture's declared rows. The synthetic markets in
//! `nc03`/`nc05`/`nc06` are hand-typed and priced only — no EVM is asked and no real-market
//! verdict is published for them (§41).
//!
//! Every run here is answered by `DumpStateProvider`, which serves the pinned dump that
//! `executor_state` loaded from disk: nothing in this file sends a request anywhere, which is
//! why §44's `rpc_count` for the M11 evidence is 0 over these runs.
//!
//! Nothing here signs, broadcasts, or holds a key.

use alloy_primitives::U256;
use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};

use evm_execution::ExecutionBinding;
use evm_opportunity::{
    optimize, price, search_domain, swap_exact_in, Gross, MathError, MultiHopRoute,
    OpportunityError, OptimizationPolicy, OptimizationStrategy, Price, RouteError,
};
use evm_risk::{MarketFacts, RiskCheck};
use evm_simulation::{MultiHopBuildError, SimulationStatus, StepStatus};
use evm_state::{InMemoryStateStore, StateError, StateStore, StateUpdate, UpdatePosition};

mod executor_state;
mod multihop_market;
mod multihop_recorded;

use executor_state::{
    amount_in, pool_balance, BLOCK, BYSTANDER, CHAIN, EXECUTOR, MID, POOL_A, POOL_B, WETH,
};
use multihop_market::{edge, edge_on, market, spec, A, B, C, D, FEE, P1, P2};
use multihop_recorded::{
    ask, assert_no_residue, blame, candidate, candidate_at, decide, fixture, fixture_with,
    fresh_recorded, plan_of, policy_at, recorded_acceptance, recorded_binding, recorded_context,
    recorded_facts, recorded_market, recorded_policy, recorded_route, recorded_route_at, request,
    simulate, try_file, try_request,
};

/// The refusal's label, for an assertion that should fail loudly when the *reason* moves.
fn check_of(decision: &evm_risk::MultihopRiskDecision) -> String {
    decision
        .check()
        .map(|check| check.label().to_string())
        .unwrap_or_else(|| format!("{}: no check answered", decision.name()))
}

/// The two block facts, with one of them deliberately unanswered — the form §45's Risk row
/// `stale` cannot be asked without.
fn facts_with(head: Option<BlockNumber>, state_version: Option<BlockNumber>) -> MarketFacts {
    MarketFacts {
        head,
        state_version,
        provenance: format!(
            "one of the two block facts withheld on purpose, at block {BLOCK}'s recording: the \
             layer has to say it was not told rather than guess"
        ),
    }
}

// ---------------------------------------------------------------------------
// NC1 — the same bytes on another chain
// ---------------------------------------------------------------------------

/// §46 `NC1 wrong chain`. Two layers own this question and both answer, because they are
/// handed the record at different moments: the risk policy carries the chain a route was
/// priced on, and M10's plan binding carries the chain the deployment was read from. A plan
/// built by the second path never consults the first, so the refusal has to be reachable
/// from either side alone.
///
/// The record is untouched — the *policy* and the *binding* are what moves. `ChainId(1)` is
/// Ethereum mainnet, an address no part of this fixture claims to be.
#[tokio::test]
async fn nc01_wrong_chain_is_refused_by_the_policy_and_by_the_plan_binding() {
    let fx = fixture();
    let simulated = simulate(&fx).await;

    // Positive control: the policy that names this recording's chain accepts, and the binding
    // that names it produces a plan.
    let granted = recorded_acceptance(&simulated);
    let policy = recorded_policy(simulated.gas_used);
    assert_eq!(
        policy.chain_id, CHAIN,
        "the policy's chain is the recording's"
    );
    assert!(
        decide(&policy, &simulated, &fresh_recorded()).accepted(),
        "the untouched record passes the untouched policy"
    );
    let (plan, binding) = plan_of(
        &simulated,
        &granted,
        &recorded_context(&fx),
        &recorded_binding(),
    )
    .expect("the recorded run binds to a plan on its own chain");
    assert_eq!(plan.plan().chain_id, CHAIN.0);
    assert!(binding.bound(), "{binding:?}");

    // The refusal, at the risk layer: one field of the policy.
    let elsewhere = policy_at(simulated.gas_used, |policy| policy.chain_id = ChainId(1));
    let decision = decide(&elsewhere, &simulated, &fresh_recorded());
    assert!(!decision.accepted());
    assert_eq!(check_of(&decision), RiskCheck::ChainValidity.label());
    let detail = decision.detail();
    assert!(
        detail.contains(&format!("chain {}", CHAIN.0)) && detail.contains("chain 1"),
        "the rejection quotes both numbers, so a reader can tell which side was wrong: {detail}",
    );

    // The refusal, at the plan boundary: the same record, a binding that says another network.
    let wrong_deployment = ExecutionBinding {
        chain_id: 1,
        executor: EXECUTOR,
    };
    let refused = plan_of(
        &simulated,
        &granted,
        &recorded_context(&fx),
        &wrong_deployment,
    )
    .expect_err("a plan for chain 91 342 does not bind to a deployment read from chain 1");
    assert_eq!(refused.code(), "plan_rejected");
    let text = refused.to_string();
    assert!(
        text.contains(&CHAIN.0.to_string()) && text.contains("chain 1"),
        "M10's rejection names both the plan's chain and the configured one: {text}",
    );
}

// ---------------------------------------------------------------------------
// NC2 — a judgement about a block that is no longer now
// ---------------------------------------------------------------------------

/// §46 `NC2 stale simulation`. The age question has two halves and they answer separately:
/// how far the chain has moved since the run's header, and how far the pipeline's live state
/// has moved since the state the run read. Both are asked against this test's own constants,
/// and both boundaries are walked, because `maximum_simulation_age: 3` is only a fact if 3
/// passes and 4 does not.
///
/// The third arm is the one §45's Risk row `stale` would otherwise hide: a caller with no
/// head answers `Unknown`, and `Unknown` is not a pass.
#[tokio::test]
async fn nc02_staleness_is_measured_at_both_boundaries_and_is_unknown_when_unasked() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let policy = recorded_policy(simulated.gas_used);
    assert_eq!(policy.maximum_simulation_age, 3);
    assert_eq!(policy.maximum_state_age, 3);

    // Positive control, at the boundary itself: exactly the ceiling is inside it.
    let at_the_line = decide(
        &policy,
        &simulated,
        &recorded_facts(BlockNumber(BLOCK + 3), BlockNumber(BLOCK + 3)),
    );
    assert!(
        at_the_line.accepted(),
        "an age of exactly {age} is within the bound: {at_the_line}",
        age = policy.maximum_simulation_age,
    );

    for (over, label) in [
        (4, RiskCheck::SimulationFreshness.label()),
        (30, RiskCheck::SimulationFreshness.label()),
    ] {
        let decision = decide(
            &policy,
            &simulated,
            &recorded_facts(BlockNumber(BLOCK + over), BlockNumber(BLOCK)),
        );
        assert!(!decision.accepted(), "head {over} blocks past the run");
        assert_eq!(decision.check().map(RiskCheck::label), Some(label));
    }

    // The state half: the run's header is current, the live state has moved on past it.
    let state_moved = decide(
        &policy,
        &simulated,
        &recorded_facts(BlockNumber(BLOCK), BlockNumber(BLOCK + 4)),
    );
    assert_eq!(
        state_moved.check().map(RiskCheck::label),
        Some(RiskCheck::StateFreshness.label()),
        "the header question passes and the state question does not — they are two facts: \
         {state_moved}",
    );

    // Nobody asked: the answer is `Unknown`, and it is not an acceptance.
    let unasked = decide(
        &policy,
        &simulated,
        &facts_with(None, Some(BlockNumber(BLOCK))),
    );
    assert!(!unasked.accepted());
    assert_eq!(
        unasked.check().map(RiskCheck::label),
        Some(RiskCheck::SimulationFreshness.label()),
    );
    assert_eq!(unasked.name(), "unknown");
    let no_state = decide(
        &policy,
        &simulated,
        &facts_with(Some(BlockNumber(BLOCK)), None),
    );
    assert_eq!(
        no_state.check().map(RiskCheck::label),
        Some(RiskCheck::StateFreshness.label()),
        "the state question is unknown on its own, not folded into the header question",
    );
}

// ---------------------------------------------------------------------------
// NC3 — a route that is not a route
// ---------------------------------------------------------------------------

/// §46 `NC3 broken route`, five shapes at once. All of them are refusals at the graph
/// boundary, before a price is computed and long before a call is built, and each names a
/// different defect — which is the point: a single "invalid route" answer could not tell a
/// reader whether the route was one pool, a pool used twice, a trail that stops, or a trail
/// that ends somewhere other than where it began.
///
/// The broken shapes that need a market the recording does not contain (a disjoint pair, a
/// three-token trail) are typed by hand on `multihop_market`'s own synthetic chain, so the
/// recorded pools are never asked to describe a market they are not in.
#[test]
fn nc03_five_broken_route_shapes_are_refused_before_any_price() {
    let fx = fixture();
    let dump = &fx.dump;

    // One pool trading against itself is not a cross-market finding.
    let one_leg = MultiHopRoute::new(&recorded_market(dump), &[edge_on(CHAIN, POOL_A, WETH, MID)]);
    assert_eq!(
        one_leg.expect_err("a single edge is refused"),
        RouteError::TooFewHops {
            found: 1,
            minimum: 2
        },
    );

    // An edge nobody showed. `BYSTANDER` is a wallet address in this fixture, not a pool.
    let not_there = MultiHopRoute::new(
        &recorded_market(dump),
        &[
            edge_on(CHAIN, POOL_A, WETH, MID),
            edge_on(CHAIN, BYSTANDER, MID, WETH),
        ],
    );
    assert!(matches!(
        not_there.expect_err("the graph holds no pool at the bystander address"),
        RouteError::EdgeNotInGraph(edge) if edge.pool == PoolId::new(CHAIN, BYSTANDER),
    ));

    // One pool spent twice: continuity and the closing token both hold, so only the reuse is
    // wrong — and the refusal says `hop 1`, the hop that repeated it.
    let twice = MultiHopRoute::new(
        &recorded_market(dump),
        &[
            edge_on(CHAIN, POOL_A, WETH, MID),
            edge_on(CHAIN, POOL_A, MID, WETH),
        ],
    );
    assert_eq!(
        twice.expect_err("a round trip through one pool is refused"),
        RouteError::RepeatedPool {
            index: 1,
            pool: PoolId::new(CHAIN, POOL_A),
        },
    );

    // Two disjoint pools: the trail stops being walkable at the hand-off.
    let disjoint = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(P2, C, D, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    let broken = MultiHopRoute::new(
        &disjoint,
        &[
            edge(multihop_market::P1, A, B),
            edge(multihop_market::P2, C, D),
        ],
    );
    assert_eq!(
        broken.expect_err("hop 1 buys what hop 0 never paid out"),
        RouteError::BrokenContinuity {
            index: 1,
            expected: TokenId::new(multihop_market::CHAIN, B),
            found: TokenId::new(multihop_market::CHAIN, C),
        },
    );

    // A trail that walks out and never comes back.
    let open = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    let not_a_cycle = MultiHopRoute::new(&open, &[edge(P1, A, B), edge(P2, B, C)]);
    assert_eq!(
        not_a_cycle.expect_err("a route that ends on a different token is not an arbitrage"),
        RouteError::NotACycle {
            start: TokenId::new(multihop_market::CHAIN, A),
            ended_on: TokenId::new(multihop_market::CHAIN, C),
        },
    );

    // Positive control: the recorded pair, both edges, one pool each, closes.
    let route = recorded_route(dump);
    assert_eq!(route.hop_count(), 2);
    assert_eq!(route.pools().len(), 2, "two distinct pools");
    assert!(route.is_priceable(), "{route:?}");
}

// ---------------------------------------------------------------------------
// NC4 — a fee that was never attested is not a fee of zero
// ---------------------------------------------------------------------------

/// §46 `NC4 missing fee`. The pricing has three answers and only one of them is a number:
/// a route whose fee evidence is absent comes back `Incomplete` naming the pool, the token
/// and the hop, and the optimizer refuses the whole search with `UnattestedFee` rather than
/// inventing a price for a market it cannot describe. §9's rule is what this guards: `None`
/// is not `0`.
///
/// Both halves are on the recorded market, so the *only* difference from `multihop_revm.rs`'s
/// passing run is that pool B's attestation is gone — the reserves, the edges and the stake
/// are the same words read out of the same dump.
#[test]
fn nc04_an_absent_fee_and_an_impossible_fee_are_both_refused() {
    let fx = fixture();
    let unattested = recorded_route_at(&fx.dump, None);
    assert!(!unattested.is_priceable());
    assert_eq!(
        unattested.unattested(),
        Some((PoolId::new(CHAIN, POOL_B), TokenId::new(CHAIN, MID))),
        "the route itself says which pool is missing evidence",
    );

    let priced = price(&unattested, amount_in());
    assert_eq!(
        priced,
        Price::Incomplete {
            pool: PoolId::new(CHAIN, POOL_B),
            token_in: TokenId::new(CHAIN, MID),
            hop_index: 1,
        },
    );
    assert_eq!(priced.state(), "incomplete");
    assert!(
        priced.quote().is_none() && priced.gross().is_none(),
        "an incomplete answer carries no output and no profit: a zero here would be a number \
         the market never gave",
    );

    // The search refuses for the same reason, before it samples a single point.
    let refused = optimize(
        &unattested,
        amount_in(),
        amount_in(),
        OptimizationPolicy::default_search(),
    )
    .expect_err("a route with no fee evidence cannot be searched");
    assert_eq!(
        refused,
        OpportunityError::UnattestedFee(PoolId::new(CHAIN, POOL_B), TokenId::new(CHAIN, MID)),
    );
    assert!(
        refused.to_string().contains("is not a fee of zero"),
        "the refusal says which confusion it is preventing: {refused}",
    );

    // The sibling: a fee present but impossible. `numerator > denominator` would pay out more
    // than the input before any pool, so it is refused as arithmetic, not as missing evidence.
    let greedy = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(
            P2,
            B,
            A,
            1_000_000,
            1_000_000,
            Some(Fee {
                numerator: 1_000,
                denominator: 997,
            }),
        ),
    ]);
    let greedy_route = MultiHopRoute::new(&greedy, &[edge(P1, A, B), edge(P2, B, A)])
        .expect("the shape is a legal round trip; only its fee is not");
    assert_eq!(
        price(&greedy_route, U256::from(1_000u32)),
        Price::Unpriceable(MathError::InvalidFee),
    );

    // Positive control: the same route with both fees attested is priced, and the profit it
    // reports is the recording's own.
    let attested = recorded_route(&fx.dump);
    assert!(attested.is_priceable());
    let quoted = price(&attested, amount_in());
    assert_eq!(quoted.state(), "quoted");
    assert!(quoted.gross().is_some_and(|gross| gross.is_gain()));
}

// ---------------------------------------------------------------------------
// NC5 — the market cannot pay what the plan asks
// ---------------------------------------------------------------------------

/// §46 `NC5 insufficient liquidity`, at the three levels where a market that cannot pay is
/// stopped.
///
/// The first stop is not a pricing answer: `InMemoryStateStore` refuses a zero reserve outright,
/// so a pool with nothing to pay out never becomes a row in a graph, and a route over it is
/// never built. That is the gate this id is actually about in the pipeline, and it firing one
/// stage earlier is why the pricing-level `InvalidReserve` is shown here as the math's own
/// answer to a caller that bypassed the store rather than as something the pipeline reaches.
///
/// The third level is the form the pipeline *does* reach: a pool with one wei of the paying
/// token is a real, registered, synced, priceable market — and it has no input that can be
/// asked of it. The route's ceiling disappears, the domain is empty rather than cheap, and the
/// quote that does come back rounds each hop down to zero output, which is the §45 Pricing row
/// `integer rounding` arriving from the other side.
#[test]
fn nc05_a_market_that_cannot_pay_is_stopped_before_any_price() {
    // Level 1 — the state boundary, through the same two production calls `market_on` runs for
    // every passing market in this file.
    let mut store = InMemoryStateStore::new(multihop_market::CHAIN);
    store
        .apply(StateUpdate::PoolRegistered(PoolMeta {
            id: PoolId::new(multihop_market::CHAIN, P1),
            protocol: ProtocolId::new("test-v2"),
            token0: multihop_market::token_on(multihop_market::CHAIN, A),
            token1: multihop_market::token_on(multihop_market::CHAIN, B),
            fee: Some(FEE),
            pool_type: PoolType::ConstantProduct,
        }))
        .expect("register a fixture pool");
    let refused = store
        .apply(StateUpdate::PoolSynced {
            pool: PoolId::new(multihop_market::CHAIN, P1),
            reserve0: U256::from(500_000u64),
            reserve1: U256::ZERO,
            position: UpdatePosition::new(BlockNumber(multihop_market::BLOCK), LogIndex(1)),
        })
        .expect_err("a pool whose paying side is empty is not a market this system holds");
    match refused {
        StateError::InvalidReserves {
            pool,
            reserve0,
            reserve1,
            position,
        } => {
            assert_eq!(
                pool,
                format!("chain {} pool {}", multihop_market::CHAIN.0, P1),
                "the refusal names the pool it turned away",
            );
            assert_eq!(
                (reserve0.as_str(), reserve1.as_str()),
                ("500000", "0"),
                "and quotes back both sides, so a reader sees which one is empty",
            );
            assert_eq!(position.block_number, BlockNumber(multihop_market::BLOCK));
        }
        other => panic!("the store turned the sync away for another reason: {other:?}"),
    }
    assert!(store
        .apply(StateUpdate::PoolSynced {
            pool: PoolId::new(multihop_market::CHAIN, P1),
            reserve0: U256::from(500_000u64),
            reserve1: U256::ONE,
            position: UpdatePosition::new(BlockNumber(multihop_market::BLOCK), LogIndex(1)),
        })
        .is_ok());

    // Level 2 — the math, for a caller that hands it a reserve the store would never have kept.
    assert_eq!(
        swap_exact_in(
            U256::from(500_000u64),
            U256::ZERO,
            FEE,
            U256::from(1_000u64)
        ),
        Err(MathError::InvalidReserve),
    );

    // Level 3 — the reachable form: one wei of the paying token.
    let thin = market(&[
        spec(P1, A, B, 1_000_000, 500_000, Some(FEE)),
        spec(P2, B, A, 500_000, 1, Some(FEE)),
    ]);
    let thin_route = MultiHopRoute::new(&thin, &[edge(P1, A, B), edge(P2, B, A)])
        .expect("a one-wei reserve is a pool, not a broken route");
    assert!(
        thin_route.is_priceable(),
        "nothing is missing from its description — the market is thin, not unknown",
    );
    assert_eq!(
        thin_route.input_upper_bound(),
        None,
        "and there is no stake it could be asked to fill",
    );
    assert_eq!(
        search_domain(&thin_route, U256::ONE, U256::from(1_000u64)),
        None,
        "the requested window is clipped away entirely, not sampled anyway",
    );
    assert_eq!(
        optimize(
            &thin_route,
            U256::ONE,
            U256::from(1_000u64),
            OptimizationPolicy::default_search(),
        )
        .expect_err("an empty domain is refused, not searched to nothing"),
        OpportunityError::EmptySearchDomain {
            lower: U256::ONE,
            upper: U256::from(1_000u64),
        },
    );
    let Price::Quoted { quote, gross } = price(&thin_route, U256::from(1_000u64)) else {
        panic!("a thin route is priceable: the answer is a number, not a refusal");
    };
    assert_eq!(quote.hops.len(), 2, "both hops were walked");
    assert!(
        quote.output.is_zero() && !quote.truncated,
        "the second hop rounds down to nothing on the full stake: {:?}",
        quote.hops[1],
    );
    assert_eq!(
        gross,
        Gross::Loss(U256::from(1_000u64)),
        "and the round trip's answer is that the whole stake is the loss",
    );

    // The two boundary arms of the same answer.
    let route = multihop_market::pair_route();
    assert_eq!(
        price(&route, U256::ZERO),
        Price::Unpriceable(MathError::InvalidAmount),
        "a zero stake is not a quote of zero output",
    );
    assert_eq!(
        price(&route, U256::MAX),
        Price::Unpriceable(MathError::Overflow),
        "the fee product of the largest 256-bit amount is not a number this type can hold",
    );

    // Positive control: the same pools with the reserve put back price the same route.
    let wet = market(&[
        spec(P1, A, B, 1_000_000, 500_000, Some(FEE)),
        spec(P2, B, A, 500_000, 625_000, Some(FEE)),
    ]);
    let wet_route = MultiHopRoute::new(&wet, &[edge(P1, A, B), edge(P2, B, A)])
        .expect("the route is the same shape at a pay-able reserve");
    assert_eq!(price(&wet_route, U256::from(1_000u32)).state(), "quoted");
    assert!(wet_route.input_upper_bound().is_some());
}

/// The execution half of §46 `NC5`, on the recorded pools.
#[tokio::test]
async fn nc05_an_ask_the_pool_cannot_pay_is_reverted_by_the_pool() {
    let fx = fixture();
    let priced = candidate(&fx.dump);

    // The leg the pricing produced, doubled on its output. Everything else in the call — the
    // pools, the tokens, the chain of amounts, the final guard — stays as the route priced it.
    let mut legs = fx.route.legs();
    let asked = legs[1].amount_out;
    legs[1].amount_out = asked * U256::from(2u32);
    assert_eq!(
        legs[1].amount_in, legs[0].amount_out,
        "the legs still chain, so what fires is the market and not the contract's own \
         continuity check",
    );
    let call = fx.execute(legs.clone(), fx.route.weth_out_leg2);
    let spec = fx.spec(call);
    let outcome = ask(&fx, &spec).await.expect("the harness ran");

    assert!(
        matches!(outcome.status, StepStatus::Reverted(..)),
        "the run did not deliver: {:?}",
        outcome.status,
    );
    assert_eq!(outcome.delivered, None);
    assert!(
        outcome.contract_error.is_none(),
        "the pair answered, not the executor — this is exactly why §25 keeps the two apart: {}",
        blame(&outcome),
    );
    assert_no_residue(&outcome);

    // The adapter will not file it: the call's legs are not the priced legs, so the record
    // would claim a route the pricing never quoted.
    let refused = try_file(&priced, &spec, outcome.clone())
        .expect_err("a hand-doubled leg cannot be filed under the untouched candidate");
    assert_eq!(refused, MultiHopBuildError::RunLegMismatch { index: 1 });

    // Positive control: the identical route at the priced ask delivers, and the difference
    // between the two runs is the one number this test moved.
    let straight = simulate(&fx).await;
    assert_eq!(straight.status.name(), "delivered");
    assert_eq!(
        straight.final_amount.expect("a delivery"),
        fx.route.weth_out_leg2,
        "and it paid exactly what the recording prices",
    );
    assert_eq!(
        asked,
        priced.search.quote.amount_out_of_hop(1).expect("hop 2"),
        "the ask this test doubled is the quote's own second-hop output, read back out of the \
         candidate rather than typed in",
    );
}

// ---------------------------------------------------------------------------
// NC6 — the search may not step past the market
// ---------------------------------------------------------------------------

/// §46 `NC6 optimizer boundary violation`. Every input above the route's ceiling is a route
/// the market cannot pay for, so the domain ends there and the ceiling is not a constant: it
/// is the last hop's reserve minus one wei, read out of the recording's own balance word. This
/// test checks the definition against that word, then walks both sides of it.
///
/// The reporting half of the boundary is here too: a search that stopped one input short of
/// the ceiling does not get to say it covered the route's domain. On a route small enough to
/// walk entirely, the same claim is true — so the flag is about the width actually searched,
/// not about which route it was computed on.
#[test]
fn nc06_the_search_domain_ends_at_the_route_ceiling_and_says_so() {
    let fx = fixture();
    let route = recorded_route(&fx.dump);

    let ceiling = route
        .input_upper_bound()
        .expect("the recorded route has an output to run out of");
    let last_reserve = pool_balance(&fx.dump, WETH, POOL_B);
    assert_eq!(
        ceiling + U256::ONE,
        last_reserve,
        "the ceiling is one wei under what the last hop holds — the definition, checked against \
         the word the recording carries",
    );

    assert_eq!(
        search_domain(&route, U256::ONE, U256::MAX),
        Some((U256::ONE, ceiling)),
        "an unbounded ask is clipped to the market, not to a number this file chose",
    );
    assert_eq!(
        search_domain(&route, ceiling + U256::ONE, U256::MAX),
        None,
        "a window that starts past the ceiling is not a window",
    );
    assert_eq!(
        optimize(
            &route,
            ceiling + U256::ONE,
            U256::MAX,
            OptimizationPolicy::default_search(),
        )
        .expect_err("the optimizer refuses past the market"),
        OpportunityError::EmptySearchDomain {
            lower: ceiling + U256::ONE,
            upper: U256::MAX,
        },
    );

    // At the ceiling the point is legal, and the reported domain is the clipped one.
    let at_ceiling = optimize(
        &route,
        ceiling,
        U256::MAX,
        OptimizationPolicy::default_search(),
    )
    .expect("the ceiling itself is inside the market");
    assert_eq!(at_ceiling.search.domain_max, ceiling);
    assert_eq!(at_ceiling.search.best_input, ceiling);

    let wide = optimize(
        &route,
        U256::ONE,
        U256::MAX,
        OptimizationPolicy::default_search(),
    )
    .expect("a wide window is searchable");
    assert_eq!(wide.search.domain_max, ceiling);
    assert!(
        wide.search.best_input <= ceiling,
        "no winner can be outside the domain: {}",
        wide.search.best_input,
    );
    assert!(
        !wide.search.covered_the_route_domain(&route),
        "a sampled search over a {}-wide domain does not claim to have seen the ceiling: {:?}",
        ceiling,
        wide.search.strategy,
    );

    // The same claim on a route small enough to walk: ceiling reached, so coverage is true;
    // stopped one short, so it is false.
    let small = market(&[
        spec(P1, A, B, 1_000, 1_100, Some(FEE)),
        spec(P2, B, A, 1_000, 1_050, Some(FEE)),
    ]);
    let small_route = MultiHopRoute::new(&small, &[edge(P1, A, B), edge(P2, B, A)])
        .expect("the two-pool pair closes");
    let top = small_route
        .input_upper_bound()
        .expect("the second hop pays out 1 050 of A");
    assert_eq!(top, U256::from(1_049u32));
    let walked = optimize(
        &small_route,
        U256::ONE,
        U256::MAX,
        OptimizationPolicy::default_search(),
    )
    .expect("a thousand inputs are inside the exhaustive limit");
    assert_eq!(walked.search.strategy, OptimizationStrategy::Exhaustive);
    assert!(
        walked.search.covered_the_route_domain(&small_route),
        "the window that reaches the ceiling searched the route's whole domain: {:?}",
        walked.search,
    );
    let one_short = optimize(
        &small_route,
        U256::ONE,
        top - U256::ONE,
        OptimizationPolicy::default_search(),
    )
    .expect("the window one below the ceiling is still searchable");
    assert!(
        !one_short.search.covered_the_route_domain(&small_route),
        "coverage is a claim about the width searched, and this one stopped short: {:?}",
        one_short.search,
    );

    // The policy caps are part of §15's promise, so they hold whatever a caller asks for.
    assert_eq!(
        OptimizationPolicy {
            coarse_points: u64::MAX,
            refine_span: u64::MAX,
            exhaustive_limit: u64::MAX,
        }
        .clamped(),
        OptimizationPolicy {
            coarse_points: OptimizationPolicy::MAX_COARSE_POINTS,
            refine_span: OptimizationPolicy::MAX_REFINE_SPAN,
            exhaustive_limit: OptimizationPolicy::MAX_EXHAUSTIVE_WIDTH,
        },
    );
    assert_eq!(
        OptimizationPolicy {
            coarse_points: 0,
            refine_span: 0,
            exhaustive_limit: 0,
        }
        .clamped()
        .coarse_points,
        1,
        "zero grid points would divide by zero; one point is the degenerate grid, not a crash",
    );
}

// ---------------------------------------------------------------------------
// NC7 — the run reverted, so nothing downstream may speak about it
// ---------------------------------------------------------------------------

/// §46 `NC7 simulation revert`. One knob — the deployment's pair allowlist — and every later
/// layer has to answer for itself: the contract names its own error, the record files as a
/// revert with no delivery and no residue, the risk policy refuses at check 1 rather than
/// computing a gross over nothing, and the plan boundary refuses a grant that was earned over
/// a *different* run.
///
/// That last arm is the one worth reading twice: the acceptance handed to it is real — the
/// untouched fixture's own delivery, granted by the untouched policy — and the record it is
/// handed over is the reverted one. A grant does not travel between runs, which is what §26's
/// five-figure comparison and this refusal both exist to say.
#[tokio::test]
async fn nc07_a_reverted_run_is_refused_by_every_layer_after_the_evm() {
    let honest = fixture();
    let granted_elsewhere = recorded_acceptance(&simulate(&honest).await);

    let fx = fixture_with(|knobs| knobs.pair_allowed = false);
    let priced = candidate(&fx.dump);
    let spec = request(&fx, &priced);
    let outcome = ask(&fx, &spec).await.expect("the harness ran");
    assert_eq!(blame(&outcome), "PairNotAllowed");
    assert_no_residue(&outcome);

    let simulated = try_file(&priced, &spec, outcome)
        .expect("a reverted run is still this candidate's run, and files as one");
    assert_eq!(simulated.status.name(), "reverted");
    assert_eq!(simulated.final_amount, None);
    assert_eq!(simulated.gross, None);

    let decision = decide(
        &recorded_policy(simulated.gas_used),
        &simulated,
        &fresh_recorded(),
    );
    assert!(!decision.accepted());
    assert_eq!(
        decision.check().map(RiskCheck::label),
        Some(RiskCheck::SimulationSuccess.label()),
        "the first check is the one that fires, so no profit line is ever computed over a \
         delivery that did not happen: {decision}",
    );
    assert!(
        decision.detail().contains("reverted"),
        "the rejection quotes §25's own word for the ending: {}",
        decision.detail(),
    );

    let refused = plan_of(
        &simulated,
        &granted_elsewhere,
        &recorded_context(&fx),
        &recorded_binding(),
    )
    .expect_err("no plan binds over a run that delivered nothing");
    assert_eq!(refused.code(), "not_delivered");
    assert!(
        refused.to_string().contains("reverted"),
        "and it quotes the ending rather than the grant: {refused}",
    );
}

// ---------------------------------------------------------------------------
// NC8 — a floor the market will not meet
// ---------------------------------------------------------------------------

/// §46 `NC8 min output failure`, in the three forms the floor can fail in.
///
/// Before the EVM: the guard a caller asks for is compared against the quote's own output, and
/// a guard above it is refused as `UnreachableGuard` — the run is never made, because a call
/// that cannot meet its own floor is not an optimistic trade, it is a lie about one.
///
/// Inside the EVM: the same one wei, moved from the record into the contract's
/// `minFinalAmount`. The call is legal-shaped and the legs are the priced ones, so the
/// adapter files it, the pool pays what it pays, and the executor's own `FinalShortfall`
/// reverts the whole transaction. §27's floor is enforced by the contract first.
///
/// After the run: a record edited so its delivery is under the guard it carries. The risk
/// layer's check 6 exists for exactly this artifact, and the test says plainly that it is a
/// forged record rather than a market outcome — a real short delivery reverted, so it never
/// reaches the profit columns.
#[tokio::test]
async fn nc08_an_unreachable_floor_is_refused_before_and_inside_and_after() {
    let fx = fixture();
    let priced = candidate(&fx.dump);
    let quoted = priced.search.best_output;

    // One wei above what the route can pay, asked of the adapter.
    let refused = try_request(&fx, &priced, quoted + U256::ONE)
        .expect_err("a guard the quote cannot reach is refused before any run");
    assert_eq!(
        refused,
        MultiHopBuildError::UnreachableGuard {
            min_final_output: quoted + U256::ONE,
            quoted,
        },
    );
    // The same ask one wei under the quote is the tightest legal guard, and it is the request
    // the passing suite runs.
    assert!(try_request(&fx, &priced, quoted - U256::ONE).is_ok());

    // The same one wei, carried into the contract this time.
    let spec_over = fx.spec(fx.execute(fx.route.legs(), fx.route.weth_out_leg2 + U256::ONE));
    let outcome = ask(&fx, &spec_over).await.expect("the harness ran");
    assert_eq!(blame(&outcome), "FinalShortfall");
    assert_no_residue(&outcome);
    let filed = try_file(&priced, &spec_over, outcome)
        .expect("the legs are the priced legs, so the record files and says it reverted");
    assert_eq!(filed.status.name(), "reverted");
    assert_eq!(filed.final_amount, None);

    // The forged record: a delivery under the guard it claims to have travelled with.
    let mut edited = simulate(&fx).await;
    let delivered = match edited.status {
        SimulationStatus::Delivered { final_amount } => final_amount,
        ref other => panic!("the untouched run was expected to deliver, got {other:?}"),
    };
    edited.min_final_output = delivered + U256::ONE;
    let decision = decide(
        &recorded_policy(edited.gas_used),
        &edited,
        &fresh_recorded(),
    );
    assert_eq!(
        decision.check().map(RiskCheck::label),
        Some(RiskCheck::FinalGuard.label()),
        "check 6 is the artifact question, reachable only past a real run: {decision}",
    );
    assert!(
        decision.detail().contains("disagree"),
        "and it says the record and the call disagree rather than that the market moved: {}",
        decision.detail(),
    );
}

// ---------------------------------------------------------------------------
// NC9 — a gain that does not clear the floor
// ---------------------------------------------------------------------------

/// §46 `NC9 final profit failure`. §27's floor is strict and it is a *gross* figure: gas is
/// not netted in here (§24 keeps the bill beside the profit, never inside it), and a gain of
/// exactly the floor is a gain that did not clear it. Both sides of that line are the run's
/// own measured delivery, so the number is not a threshold someone hoped would be near the
/// answer.
#[tokio::test]
async fn nc09_the_profit_floor_is_strict_and_sits_on_the_gross() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let gain = recorded_acceptance(&simulated).gross_profit;
    assert!(
        !gain.is_zero(),
        "the recorded route gains, or there is no boundary here to walk",
    );

    // Exactly the gain is not above it.
    let at_the_line = policy_at(simulated.gas_used, |policy| {
        policy.minimum_gross_profit = gain;
    });
    let decision = decide(&at_the_line, &simulated, &fresh_recorded());
    assert!(!decision.accepted());
    assert_eq!(
        decision.check().map(RiskCheck::label),
        Some(RiskCheck::MinimumGrossProfit.label()),
        "{decision}",
    );
    let text = decision.detail();
    assert!(
        text.contains(&gain.to_string()),
        "the rejection quotes the gain it was compared against: {text}",
    );

    // One wei under, and the same run is accepted with the same figure.
    let one_under = policy_at(simulated.gas_used, |policy| {
        policy.minimum_gross_profit = gain - U256::ONE;
    });
    let accepted = match decide(&one_under, &simulated, &fresh_recorded()) {
        evm_risk::MultihopRiskDecision::Accept(figures) => figures,
        other => panic!("a floor one wei under the gain should pass: {other}"),
    };
    assert_eq!(accepted.gross_profit, gain);
    assert_eq!(accepted.minimum_gross_profit, gain - U256::ONE);
    assert_eq!(accepted.gas_used, simulated.gas_used);
    assert!(
        !text.contains("gas"),
        "§24 keeps the bill beside the profit, so the profit rejection quotes no gas figure. \
         The row of §45's Risk matrix that *is* about gas is answered in \
         `tests/multihop_risk.rs`, where the ceiling is the number that moves — this negative \
         has a positive partner there rather than being its own proof: {text}",
    );
}

// ---------------------------------------------------------------------------
// NC10 — the bytes that would be signed are not the bytes that were run
// ---------------------------------------------------------------------------

/// §46 `NC10 calldata mismatch`. Two real runs of one recorded market at two stakes produce
/// two calldatas, and §30's binding is the claim that they are one. `MultihopBinding::observe`
/// hashes each side through its own path — the simulation over the call it made, the plan over
/// the call it will be signed with — so the inequality here is two independent derivations
/// disagreeing, which is what makes it evidence rather than a string comparison.
///
/// The plan layer's own field-by-field pass fires first and names which figure moved, because
/// a hash tells you *that* the two objects are not a pair and the comparison tells you *why*.
#[tokio::test]
async fn nc10_two_runs_of_one_market_do_not_bind_to_each_others_plan() {
    let fx = fixture();
    let priced_a = candidate(&fx.dump);
    let priced_b = candidate_at(&fx.dump, amount_in() / U256::from(2u32));
    assert_ne!(priced_a, priced_b, "two stakes, so two runs");

    let spec_a = request(&fx, &priced_a);
    let spec_b = request(&fx, &priced_b);
    let a = try_file(&priced_a, &spec_a, ask(&fx, &spec_a).await.expect("ran")).expect("filed a");
    let b = try_file(&priced_b, &spec_b, ask(&fx, &spec_b).await.expect("ran")).expect("filed b");
    assert_ne!(a.calldata(), b.calldata());
    assert_ne!(a.identity(), b.identity());

    let granted_a = recorded_acceptance(&a);
    let (plan_a, binding_a) = plan_of(&a, &granted_a, &recorded_context(&fx), &recorded_binding())
        .expect("run A binds to plan A");
    assert!(binding_a.bound(), "{binding_a:?}");

    // Plan A observed against run B: the headline §30 refusal.
    let crossed = evm_execution::MultihopBinding::observe(&b, plan_a.plan());
    assert!(!crossed.bound());
    let refusal = crossed
        .unbound()
        .expect("a hash that differs is a refusal, not an opinion");
    assert_eq!(refusal.code(), "calldata_hash_mismatch");
    assert_eq!(crossed.simulation_calldata_hash, b.calldata_hash());
    assert_eq!(crossed.execution_calldata_hash, plan_a.calldata_hash());
    assert_ne!(
        crossed.simulation_calldata_hash, crossed.execution_calldata_hash,
        "{crossed:?}"
    );

    // The plan boundary's own pass names the figure rather than the hash.
    let refused = plan_of(&b, &granted_a, &recorded_context(&fx), &recorded_binding())
        .expect_err("run B cannot inherit run A's grant");
    assert_eq!(refused.code(), "acceptance_mismatch");
    let text = refused.to_string();
    assert!(
        text.contains(&a.input_amount.to_string()) && text.contains(&b.input_amount.to_string()),
        "both stakes are quoted, since which side is the old one is the caller's question: {text}",
    );
}

// ---------------------------------------------------------------------------
// NC11 — one run, two plans, and why both hashes are needed
// ---------------------------------------------------------------------------

/// §46 `NC11 plan hash mismatch`. The plan hash covers what the calldata does not: the
/// freshness statement the caller declared. Two plans over one simulation, differing only in
/// `max_block_age`, are the same transaction bytes and a different plan — which is the reason
/// §39's identity is `keccak(simulation ‖ plan)` rather than the simulation alone, and the
/// reason the lane ledger keys a lane by the plan hash.
///
/// The calldata hash is *not* what this control moves, so the test asserts both halves: equal
/// bytes, unequal plans. A control that moved the calldata too would be NC10 again.
#[tokio::test]
async fn nc11_the_same_run_under_two_freshness_declarations_is_two_plans_one_call() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let granted = recorded_acceptance(&simulated);
    let binding = recorded_binding();

    let short_window = recorded_context(&fx);
    let age = short_window.max_block_age;
    let (plan_short, observed_short) = plan_of(&simulated, &granted, &short_window, &binding)
        .expect("the recorded run binds at the declared window");

    let mut long_window = recorded_context(&fx);
    long_window.max_block_age = age + 1;
    let (plan_long, observed_long) = plan_of(&simulated, &granted, &long_window, &binding)
        .expect("the same run binds at a wider window too");

    assert_eq!(
        plan_short.calldata(),
        plan_long.calldata(),
        "the transaction bytes are identical: a wider window is not a different trade",
    );
    assert_eq!(
        observed_short.execution_calldata_hash,
        observed_long.execution_calldata_hash,
    );
    assert_ne!(
        plan_short.plan_hash(),
        plan_long.plan_hash(),
        "the plan hash covers the window, so the two plans are not one plan",
    );
    assert_ne!(observed_short.plan_hash, observed_long.plan_hash);

    // §39's id is the reason that matters: over one simulation, two plans are two decisions.
    let id_short = plan_short.to_intent().ids.risk_decision_id;
    let id_long = plan_long.to_intent().ids.risk_decision_id;
    assert_ne!(id_short, id_long, "{id_short:?} vs {id_long:?}");

    // Positive control: the same declaration twice is one plan, byte for byte and hash for
    // hash — so the difference above is the field this test moved and nothing else.
    let (plan_again, observed_again) =
        plan_of(&simulated, &granted, &recorded_context(&fx), &binding)
            .expect("the recorded run binds again");
    assert_eq!(plan_again.plan_hash(), plan_short.plan_hash());
    assert_eq!(
        plan_again.calldata_hash(),
        plan_short.calldata_hash(),
        "and the simulation's own hash is the third copy of the same bytes",
    );
    assert_eq!(observed_again.plan_hash, observed_short.plan_hash);
    assert_eq!(simulated.calldata_hash(), plan_short.calldata_hash());
}

// ---------------------------------------------------------------------------
// NC15 — a plan whose window has closed
// ---------------------------------------------------------------------------

/// §46 `NC15 expired plan`, at the plan's own freshness answer. §45's Lane row `expiration`
/// is the ledger's version of this question and is named in
/// `crates/execution/tests/multihop_lanes.rs`; this is the plan boundary, where the window is
/// a number the caller declared and the plan carries verbatim.
///
/// Three heads, three answers, one artifact: the expiry does not re-encode anything. A plan
/// that went stale is the same calldata the run measured, refused — which is the only honest
/// shape for a decision that arrives after the fact.
#[tokio::test]
async fn nc15_a_plan_past_its_own_window_is_stale_at_the_same_bytes() {
    let fx = fixture();
    let simulated = simulate(&fx).await;
    let granted = recorded_acceptance(&simulated);
    let (executable, _) = plan_of(
        &simulated,
        &granted,
        &recorded_context(&fx),
        &recorded_binding(),
    )
    .expect("the recorded run binds to a plan");
    let age = executable.plan().validity.max_block_age;
    let simulated_at = executable.plan().validity.simulated_at_block;
    assert_eq!(simulated_at.0, BLOCK, "the plan is dated by the run");

    assert_eq!(
        executable.freshness_at(BlockNumber(BLOCK)),
        evm_execution::Freshness::Active,
        "at the block it was priced on, the plan is current",
    );
    assert_eq!(
        executable.freshness_at(BlockNumber(BLOCK + age)),
        evm_execution::Freshness::Active,
        "exactly the window is inside it",
    );
    let expired = executable.freshness_at(BlockNumber(BLOCK + age + 1));
    assert!(
        matches!(expired, evm_execution::Freshness::Stale { .. }),
        "one block past the window it is not: {expired:?}",
    );
    let detail = match expired {
        evm_execution::Freshness::Stale { reason } => reason,
        other => panic!("the answer above was the stale arm: {other:?}"),
    };
    assert!(
        detail.contains(&(age + 1).to_string()) && detail.contains(&age.to_string()),
        "the rejection counts the blocks that were sealed against the bound it was given: {detail}",
    );

    // A head behind the plan is its own refusal: state that has not happened cannot be spent.
    let backwards = executable.freshness_at(BlockNumber(BLOCK - 1));
    assert!(
        matches!(backwards, evm_execution::Freshness::Stale { .. }),
        "{backwards:?}",
    );

    // None of it moved the bytes.
    assert_eq!(
        executable.calldata_hash(),
        simulated.calldata_hash(),
        "expiry is a decision about the run, not a re-encoding of it",
    );
}
