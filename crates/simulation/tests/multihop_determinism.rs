//! §47's five claims, walked twice on one recording.
//!
//! ```text
//! cargo test -p evm-simulation --test multihop_determinism -- --test-threads=1
//! ```
//!
//! §47 is a short sentence with five nouns in it: from the same `GraphSnapshot` and the same
//! `CycleCandidate` come the same route, the same quote, the same optimizer result, the same plan
//! hash and the same calldata hash, over at least two consecutive runs. Each of those is a claim
//! about a different stage, and a stage that agrees with itself for the wrong reason is the
//! failure mode this file is written against — a search that returns a fixed vector, a quote
//! cached in a `HashMap` iteration order, a hash taken over a timestamp. So every equality here is
//! between two objects assembled by two separate loads of the same files from disk:
//! [`Fixture::committed`] re-reads the recording and re-checks its bytes each time, the graph is
//! projected twice, the cycle search runs twice, the pricing and the search run twice, the EVM is
//! asked twice, and the risk layer and the plan are built twice. Nothing is computed once and
//! compared to itself.
//!
//! ## Where each number comes from
//!
//! The market, the route, the candidate, the request, the policy and the plan context all come
//! from `mod multihop_recorded` — the same module `tests/multihop_revm.rs` and
//! `tests/multihop_negative_controls.rs` read, so the chain being repeated here is the chain those
//! files prove works rather than a fourth arrangement of it. §47 names `CycleCandidate`, so the
//! candidate side goes through M9.3's `find_cycles` rather than through a test's own edge list,
//! and the two are then checked against each other: same builder, same graph, one route.
//!
//! The third test is the control that makes the first two evidence. One wei taken off the stake,
//! and every figure §47 names has to move — the quote, the optimizer's answer, the calldata bytes,
//! the calldata hash, the plan hash — while the route identity stays exactly where it was. Two runs
//! that agree while nothing was changed could also be two runs of a stub; two runs that agree *and*
//! disagree when the stake moves can only be two runs of the market. The fourth test is the mirror
//! image of the same claim on the field the identity does not carry: a re-attested fee moves the
//! quote and leaves the route alone, while a fee attested at nothing stops the quote. The fifth
//! measures the fixture's own funding bound, which is why the nudge goes down rather than up.
//!
//! Everything is answered by `DumpStateProvider` over the pinned dump, so nothing here sends a
//! request anywhere (§44's `rpc_count` for this file is 0). One caveat travels with the fourth
//! test: the alternative fee it re-attests a recorded pool at is a declared counterfactual, not a
//! measurement of that pool, so the quote it produces is a statement about the pricing function
//! and no real-market verdict is published from it (§41).
//!
//! Nothing here signs, broadcasts, or holds a key.

use alloy_primitives::U256;
use evm_execution::{ExecutablePlan, MultihopBinding};
use evm_opportunity::{
    optimize, price, MultiHopRoute, OptimizationPolicy, OptimizedCandidate, Price,
};
use evm_pathfinder::CycleCandidate;
use evm_risk::MultihopAcceptance;
use evm_simulation::executor::ExecutorRun;
use evm_simulation::SimulatedOpportunity;

mod executor_state;
mod multihop_market;
mod multihop_recorded;

use executor_state::{amount_in, Fixture, BLOCK, CHAIN, EXECUTOR, POOL_A, POOL_B};
use multihop_recorded::{
    assert_no_residue, blame, candidate_at, file, fixture, plan_of, recorded_acceptance,
    recorded_binding, recorded_context, recorded_cycle_candidate, recorded_cycles, recorded_market,
    recorded_market_at, recorded_route, recorded_route_at, recorded_route_from_candidate, request,
    run_ok,
};

// ---------------------------------------------------------------------------
// One chain, as many times as a test needs it
// ---------------------------------------------------------------------------

/// Every stage §47 names, in the order the pipeline runs them, holding what each answered.
/// Assembled from `mod multihop_recorded`'s own helpers, so this is a repetition of the pipeline
/// and not a fourth description of it.
struct Walked {
    candidate: CycleCandidate,
    route: MultiHopRoute,
    priced: OptimizedCandidate,
    spec: ExecutorRun,
    simulated: SimulatedOpportunity,
    granted: MultihopAcceptance,
    plan: ExecutablePlan,
    binding: MultihopBinding,
}

/// Search, route, price, optimise, request, run, file, judge, plan — once, at `stake`.
async fn walk(fx: &Fixture, stake: U256) -> Walked {
    let candidate = recorded_cycle_candidate(&fx.dump);
    let route = recorded_route_from_candidate(&fx.dump, &candidate);
    let priced = candidate_at(&fx.dump, stake);
    let spec = request(fx, &priced);
    let outcome = run_ok(fx, &spec).await;
    let simulated = file(&priced, &spec, outcome);
    let granted = recorded_acceptance(&simulated);
    let (plan, binding) = plan_of(
        &simulated,
        &granted,
        &recorded_context(fx),
        &recorded_binding(),
    )
    .expect("the recorded run binds to a plan");
    Walked {
        candidate,
        route,
        priced,
        spec,
        simulated,
        granted,
        plan,
        binding,
    }
}

/// The five figures §47 lists, in one line, for a comparison that has to say which one moved.
fn five_claims(walked: &Walked) -> (String, String, String, String, String) {
    (
        format!("{:?}", walked.route.identity()),
        format!("{:?}", walked.priced.search.quote),
        format!(
            "{} / {}",
            walked.priced.search.best_input, walked.priced.search.domain_min
        ),
        format!("{:#}", walked.plan.plan_hash()),
        format!("{:#}", walked.plan.calldata_hash()),
    )
}

// ---------------------------------------------------------------------------
// The first three claims: route, quote, optimizer result
// ---------------------------------------------------------------------------

/// §47's first three nouns, on the pricing side of the pipeline, where no EVM is involved.
///
/// The route claim is made twice over, because §47 names a `CycleCandidate` as the input: once
/// from two loads through the search, and once between the search's candidate and the edge list
/// `tests/multihop_revm.rs` enters the same builder by hand. Those are different code paths into
/// `MultiHopRoute::new`, and if they disagreed the two files would be describing two markets while
/// both passing.
#[test]
fn two_loads_of_the_recording_search_price_and_optimise_to_one_answer() {
    let first = fixture();
    let second = fixture();
    assert!(
        !std::ptr::eq(&first.dump, &second.dump),
        "two loads have to be two objects, or the comparison below is one object agreeing with \
         itself",
    );

    // The search's whole reported set, in the order it reports it.
    let cycles_first = recorded_cycles(&first.dump);
    let cycles_second = recorded_cycles(&second.dump);
    assert_eq!(
        cycles_first, cycles_second,
        "the search returned a different set, or a different order",
    );
    assert!(
        cycles_first.len() >= 2,
        "the pair closes both ways, so an empty or single-entry result would make the equality \
         above a comparison of one item and not of a search",
    );

    let candidate_first = recorded_cycle_candidate(&first.dump);
    let candidate_second = recorded_cycle_candidate(&second.dump);
    assert_eq!(candidate_first, candidate_second);
    assert_eq!(
        candidate_first.canonical_key, candidate_second.canonical_key,
        "and the rotation-invariant key the search is built to hand out",
    );
    assert_eq!(candidate_first.chain_id, CHAIN);
    assert_eq!(candidate_first.target_block.0, BLOCK);
    assert_eq!(candidate_first.hop_count, 2);
    assert_eq!(candidate_first.edges.len(), 2);

    // The route, three ways: two searches and the hand-written edge list.
    let route_first = recorded_route_from_candidate(&first.dump, &candidate_first);
    let route_second = recorded_route_from_candidate(&second.dump, &candidate_second);
    let route_typed = recorded_route(&second.dump);
    assert_eq!(
        route_first, route_second,
        "same candidate, two loads, one route"
    );
    assert_eq!(
        route_first, route_typed,
        "the route the search's edges build is the route this file's edge list builds"
    );
    assert_eq!(route_first.identity(), route_typed.identity());
    assert_eq!(
        route_first.pools(),
        vec![
            evm_core::PoolId::new(CHAIN, POOL_A),
            evm_core::PoolId::new(CHAIN, POOL_B)
        ],
        "in trade order, which is the part of the route an identity hash cannot hide",
    );

    // The quote: the whole `Price`, so every hop's amount in and out and the gross are compared,
    // not just the headline output.
    let stake = amount_in();
    let quote_first = price(&route_first, stake);
    let quote_second = price(&route_second, stake);
    assert_eq!(quote_first, quote_second);
    let Price::Quoted { quote, gross } = &quote_first else {
        panic!("the recorded route prices: {quote_first:?}");
    };
    assert_eq!(quote.input, stake);
    assert_eq!(quote.hops.len(), 2);
    assert!(!quote.truncated);
    assert!(
        gross.is_gain(),
        "and what it says about the round trip is a gain: {gross:?}"
    );

    // The optimizer result, over a real window rather than this file's one-point domain.
    let policy = OptimizationPolicy::default_search();
    let searched_first = optimize(&route_first, U256::ONE, stake, policy)
        .expect("the recorded route has a domain to search");
    let searched_second = optimize(&route_second, U256::ONE, stake, policy)
        .expect("the same domain on the second load");
    assert_eq!(
        searched_first, searched_second,
        "same route, same window, different answer"
    );
    assert_eq!(
        searched_first.search.evaluations, searched_second.search.evaluations,
        "including how many inputs were priced, which is the count a non-deterministic search \
         would move first"
    );
    assert!(
        searched_first.search.evaluations > 1,
        "the window is wide enough that this is a search and not one point quoted twice: {} \
         evaluations",
        searched_first.search.evaluations,
    );
    assert_eq!(
        searched_first.route, searched_second.route,
        "and the candidate carries the route it was searched on"
    );
    assert_eq!(
        format!("{:?}", searched_first.search.quote),
        format!("{:?}", searched_second.search.quote),
        "same quote — the whole multi-hop quote the winning input carries, not only its \
         headline output",
    );
}

// ---------------------------------------------------------------------------
// The last two claims: plan hash and calldata hash, over two real runs
// ---------------------------------------------------------------------------

/// §47's remaining two nouns, which are only reachable through an execution: the plan hash and
/// the calldata hash are hashes over the bytes a run produced and a policy accepted.
///
/// The comparison goes past the two hashes on purpose. `gas_used` is the EVM's own measurement, so
/// two runs of one state agreeing on it is a statement about the interpreter as well as about the
/// pipeline; `delivered` is the balance delta the pair actually paid. A chain that reproduced the
/// hashes while drifting either of those would be hashing something other than the run.
#[tokio::test]
async fn two_loads_of_the_recording_run_file_judge_and_plan_to_one_hash() {
    let first = fixture();
    let second = fixture();
    let a = walk(&first, amount_in()).await;
    let b = walk(&second, amount_in()).await;

    // §47's five claims, in the order the doc lists them.
    assert_eq!(a.route, b.route, "same route");
    assert_eq!(a.priced, b.priced, "same optimizer result");
    assert_eq!(
        format!("{:?}", a.priced.search.quote),
        format!("{:?}", b.priced.search.quote),
        "same quote",
    );
    assert_eq!(
        a.plan.calldata_hash(),
        b.plan.calldata_hash(),
        "same calldata hash"
    );
    assert_eq!(a.plan.plan_hash(), b.plan.plan_hash(), "same plan hash");

    // The bytes the hashes are taken over, and the request that carried them.
    assert_eq!(
        a.plan.calldata(),
        b.plan.calldata(),
        "one calldata, byte for byte"
    );
    assert_eq!(a.spec.call.encode(), b.spec.call.encode());
    assert_eq!(a.spec.call.selector(), b.spec.call.selector());
    assert_eq!(a.simulated.calldata(), b.simulated.calldata());

    // The run's own measured figures: the EVM answered the same question twice.
    assert_eq!(a.simulated.gas_used, b.simulated.gas_used);
    assert_eq!(a.simulated.gas_charge, b.simulated.gas_charge);
    assert_eq!(a.simulated.final_amount, b.simulated.final_amount);
    assert_eq!(a.simulated.min_final_output, b.simulated.min_final_output);
    assert_eq!(
        a.granted.delivered, b.granted.delivered,
        "the delivered amount the risk layer compared against the floor"
    );
    assert_eq!(a.granted.gross_profit, b.granted.gross_profit);

    // The identities downstream stages key on.
    assert_eq!(a.simulated.identity(), b.simulated.identity());
    assert_eq!(a.simulated.identity_hash(), b.simulated.identity_hash());
    assert_eq!(
        a.simulated.canonical_text(),
        b.simulated.canonical_text(),
        "the serialized run, so one log byte of difference would show here",
    );
    assert_eq!(a.plan.route_id(), b.plan.route_id());

    // The binding: six values, each derived on one side of the simulation/plan boundary. The
    // identity strings and the hashes are compared in their own types, because a `String` and a
    // `B256` that print alike are not the same claim.
    for (name, left, right) in [
        (
            "simulation_id",
            &a.binding.simulation_id,
            &b.binding.simulation_id,
        ),
        (
            "priced_route_id",
            &a.binding.priced_route_id,
            &b.binding.priced_route_id,
        ),
        (
            "simulated_route_id",
            &a.binding.simulated_route_id,
            &b.binding.simulated_route_id,
        ),
        (
            "execution_route_id",
            &a.binding.execution_route_id,
            &b.binding.execution_route_id,
        ),
    ] {
        assert_eq!(
            left, right,
            "{name} moved between two runs of one recording"
        );
    }
    assert_eq!(
        a.binding.simulation_calldata_hash, b.binding.simulation_calldata_hash,
        "the simulation side of the calldata hash"
    );
    assert_eq!(
        a.binding.execution_calldata_hash, b.binding.execution_calldata_hash,
        "the execution side of the same hash"
    );
    assert_eq!(a.binding.plan_hash, b.binding.plan_hash);
    let (first_binding, second_binding) = (&a.binding, &b.binding);
    assert!(
        first_binding.bound() && second_binding.bound(),
        "{first_binding:?} / {second_binding:?}",
    );

    // Both chains are the fixture's chain, so the equality above is not two copies of a different
    // network agreeing with each other.
    assert_eq!(a.simulated.chain_id, CHAIN);
    assert_eq!(a.spec.executor, EXECUTOR);
    assert_eq!(a.simulated.simulation_block.number.0, BLOCK);
}

// ---------------------------------------------------------------------------
// The control: move the stake, and every claim moves
// ---------------------------------------------------------------------------

/// §47's equalities are only evidence if they are about content. One wei off the stake is the
/// smallest change the market can see, and it has to move the quote, the optimizer's answer, the
/// bytes, both hashes and the run's identity — while leaving the route alone. Otherwise the two
/// runs above would be agreeing because nothing downstream reads anything upstream of them.
///
/// The nudge goes *down*, and the reason is a property of this fixture rather than a choice: the
/// operator's WETH balance and its allowance are both set to exactly `amount_in()` (§34's declared
/// rows), so one wei more is a request the wallet cannot fund and the run never reaches the
/// question this control asks. `the_funded_stake_is_the_bound_the_control_nudges_down_from`
/// measures that bound instead of assuming it.
#[tokio::test]
async fn one_wei_less_stake_moves_the_quote_the_calldata_and_both_hashes() {
    let fx = fixture();
    let stake = amount_in();
    let untouched = walk(&fx, stake).await;
    let nudged = walk(&fx, stake - U256::ONE).await;

    assert_ne!(
        untouched.priced, nudged.priced,
        "the optimizer result is about the stake"
    );
    assert_ne!(
        untouched.spec.call.encode(),
        nudged.spec.call.encode(),
        "so is the request's bytes"
    );
    assert_ne!(
        untouched.plan.calldata(),
        nudged.plan.calldata(),
        "and the plan's"
    );

    // §47's five figures, one pair at a time, so a failure names which one failed to move.
    let (before, after) = (five_claims(&untouched), five_claims(&nudged));
    for (position, (name, left, right)) in [
        ("quote", &before.1, &after.1),
        ("optimizer result", &before.2, &after.2),
        ("plan hash", &before.3, &after.3),
        ("calldata hash", &before.4, &after.4),
    ]
    .iter()
    .enumerate()
    {
        assert_ne!(left, right, "§47's claim {position} ({name}) did not move");
    }
    assert_eq!(
        before.0, after.0,
        "and the one figure that must *not* move: one wei of stake is the same route. A test that \
         flagged this as a new route would flag every re-quote of a live market as a new \
         opportunity.",
    );

    // The search still describes the recorded cycle, so the difference is in the amounts and not
    // in which market was found.
    assert_eq!(
        untouched.candidate, nudged.candidate,
        "the search found the same cycle"
    );
    assert_eq!(untouched.route, nudged.route);

    // Both runs delivered, so the difference above is a difference between two working chains and
    // not a refusal on one side of the comparison.
    assert_eq!(untouched.simulated.status.name(), "delivered");
    assert_eq!(nudged.simulated.status.name(), "delivered");
    assert!(
        nudged.simulated.final_amount.expect("a delivery")
            < untouched.simulated.final_amount.expect("a delivery"),
        "on this market one wei less in pays less out, which is why the quote is not flat",
    );
}

/// The bound the control above nudges down from, measured rather than stated: the fixture funds
/// exactly `amount_in()`, so `amount_in() + 1` is a run the pair refuses before the route is ever
/// asked to pay. This is scaffolding biting, not the market running dry (§41's distinction), and
/// it is recorded here because the control test's design depends on it.
#[tokio::test]
async fn the_funded_stake_is_the_bound_the_control_nudges_down_from() {
    let fx = fixture();
    let priced = candidate_at(&fx.dump, amount_in() + U256::ONE);
    let spec = request(&fx, &priced);
    let outcome = run_ok(&fx, &spec).await;
    assert!(
        !outcome.status.succeeded(),
        "an ask the wallet cannot fund should not deliver: {}",
        outcome.describe()
    );
    assert_eq!(
        blame(&outcome),
        "InsufficientBalance",
        "and it should fail for the funding reason rather than for a route reason: {}",
        outcome.describe()
    );
    assert_no_residue(&outcome);
}

/// The other side of the same control: a *different* market must give a different answer, so
/// the equalities in the first two tests are not the only thing this builder can produce.
///
/// What the measurement turned up while writing this is the shape of the claim, so it is the
/// shape stated here. A route's identity is its edge list — pool, token in, token out — and the
/// fee is not in it: re-attesting one pool at 995/1000 instead of 997/1000 leaves
/// [`MultiHopRoute::identity`] exactly as it was and moves the quote. That is the right division
/// of labour, and it is also what makes the first test's quote equality a real statement: two
/// runs agreeing on a quote is agreeing on arithmetic over the fee evidence, not on a label the
/// graph carries. The second arm is the field that does change the route's answer — a fee that is
/// not attested at all, which stops the quote rather than merely re-rating it (§46's `NC4`, which
/// `tests/multihop_negative_controls.rs` proves as a refusal; here it is the control that the
/// pricing function reads the fee at all).
#[test]
fn a_different_fee_moves_the_quote_while_an_absent_fee_stops_it() {
    let fx = fixture();
    let plain = recorded_route(&fx.dump);
    let other_fee = evm_core::Fee {
        numerator: 995,
        denominator: 1_000,
    };
    let costlier = recorded_route_at(&fx.dump, Some(other_fee));
    assert_eq!(plain.hop_count(), costlier.hop_count());
    assert_eq!(plain.pools(), costlier.pools(), "the same two pools");
    assert_eq!(
        plain.identity(),
        costlier.identity(),
        "and the same route identity, because an identity is the edges it walks, not the terms \
         those edges trade on",
    );

    let before = price(&plain, amount_in());
    let after = price(&costlier, amount_in());
    assert_ne!(before, after, "the quote follows the field that moved");
    let (Price::Quoted { gross: first, .. }, Price::Quoted { gross: second, .. }) =
        (&before, &after)
    else {
        panic!("both routes price: {before:?} / {after:?}");
    };
    assert!(
        first.gain().expect("a gain") > second.gain().expect("a gain"),
        "and the cheaper fee pays more: {first:?} against {second:?}",
    );

    // The two graphs hold the same number of edges, so the difference above is one field on one
    // pool — not a market that got bigger or smaller.
    let both = recorded_market(&fx.dump);
    let re_fees = recorded_market_at(&fx.dump, Some(other_fee));
    assert_eq!(
        both.edge_count(),
        4,
        "two pools, each tradable in both directions",
    );
    assert_eq!(both.edge_count(), re_fees.edge_count());

    // The arm that does change the route's answer: pool B attested at nothing.
    let unattested = recorded_route_at(&fx.dump, None);
    assert_eq!(
        unattested.identity(),
        plain.identity(),
        "the same edges again — so what follows is not a different route, it is the same route \
         with no evidence to price",
    );
    assert!(!unattested.is_priceable());
    let Price::Incomplete {
        pool, hop_index, ..
    } = price(&unattested, amount_in())
    else {
        panic!(
            "a route with an attested-out fee answers Incomplete: {:?}",
            price(&unattested, amount_in())
        )
    };
    assert_eq!(pool, evm_core::PoolId::new(CHAIN, POOL_B));
    assert_eq!(hop_index, 1);
}
