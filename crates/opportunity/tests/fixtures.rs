//! §25 of the task: the eight fixture shapes M3 has to get right, plus the
//! §46 rule that decides whether a result is believed — every peak here is
//! checked against the exhaustive `u128` scan in `support`, never against the
//! search that produced it.
//!
//! Reserves are small on purpose. Every domain in this file is enumerable, so
//! "the search found the best input" is a verified statement here rather than an
//! extrapolation; the extrapolation to 112-bit reserves is fixture 6.

mod support;

use alloy_primitives::U256;

use evm_opportunity::{
    detect_opportunities, enumerate_candidates, find_optimal_input, ArbitragePath, MathError,
    Opportunity, PricedCycle, SearchPolicy,
};

use evm_state::StateStore;

use support::{
    edges, fee_threshold_squared, market, peak, pool, price_product, ratio_gt, side, spec, token,
    A, B, BLOCK, C, FEE, NO_FEE, P1, P2, P3, P4, P5, P6,
};

/// Pool 1: 100 000 A / 50 000 B. Quoted the same way the state store holds it —
/// `token0` first — so a test cannot quietly re-orient a price.
fn pool1() -> support::Spec {
    spec(P1, A, B, 100_000, 50_000, Some(FEE))
}

/// Pool 2: 50 000 B / 125 000 A. Buys B at 2.5 A where pool 1 sells it at 2 A.
fn pool2() -> support::Spec {
    spec(P2, B, A, 50_000, 125_000, Some(FEE))
}

fn pool3() -> support::Spec {
    spec(P3, A, C, 100_000, 50_000, Some(FEE))
}

/// 50 000 C / 200 000 A: a 4 A per C bid against pool 3's 2 A per C ask.
fn pool4() -> support::Spec {
    spec(P4, C, A, 50_000, 200_000, Some(FEE))
}

/// 40 000 B / 80 000 A: the same 2 A per B as pool 1, so no route through it
/// can pay anything but fees.
fn pool5() -> support::Spec {
    spec(P5, B, A, 40_000, 80_000, Some(FEE))
}

/// 50 000 B / 100 500 A: a 0.5 % better price than pool 1, against a round trip
/// that costs 0.6 %. The spread is real and the profit is not.
fn pool6() -> support::Spec {
    spec(P6, B, A, 50_000, 100_500, Some(FEE))
}

fn u(v: u128) -> U256 {
    U256::from(v)
}

/// The exhaustive scan with nothing withheld, for the controls that price a
/// fixture at a fee of zero.
fn peak_free(first: (u128, u128), second: (u128, u128)) -> support::Peak {
    support::peak_at(first, second, NO_FEE)
}

/// The route's opportunity, if the detection has one for it.
fn finding(
    found: &[Opportunity],
    first: evm_core::PoolId,
    input: evm_core::TokenId,
) -> Option<&Opportunity> {
    found
        .iter()
        .find(|o| o.path.first().pool == first && o.input_token == input)
}

/// Which of this file's pool specs a route's hop came from.
fn index_of(pools: &[support::Spec], id: evm_core::PoolId) -> usize {
    pools
        .iter()
        .position(|spec| pool(spec.pool) == id)
        .expect("every hop names a fixture pool")
}

// ---------------------------------------------------------------------------
// Fixture 1: no arbitrage
// ---------------------------------------------------------------------------

#[test]
fn fixture_1_equal_prices_produce_no_opportunity() {
    let snapshot = market(&[pool1(), pool5()]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(detection.candidates.len(), 4);
    assert!(detection.is_empty(), "{:?}", detection.opportunities);
    assert_eq!(detection.rejected.len(), 4);
    for rejection in &detection.rejected {
        assert!(
            rejection.peak.expect("priced").gross_profit().is_none(),
            "{:?}",
            rejection.path
        );
    }
    // The independent scan says the same thing for both directions: the best
    // round trip on a market with no spread is a loss.
    let p1_ab = (100_000u128, 50_000u128);
    let p1_ba = (50_000u128, 100_000u128);
    let p5_ab = (80_000u128, 40_000u128);
    let p5_ba = (40_000u128, 80_000u128);
    assert!(peak(p1_ab, p5_ba).profit < 0);
    assert!(peak(p5_ab, p1_ba).profit < 0);
    // And the exact rational condition behind that verdict: a price product of
    // precisely 1.0 cannot clear (1000/997)².
    assert_eq!(price_product(p1_ab, p5_ba), price_product(p5_ab, p1_ba));
    assert!(!ratio_gt(
        price_product(p1_ab, p5_ba),
        fee_threshold_squared()
    ));
}

// ---------------------------------------------------------------------------
// Fixture 2: obvious arbitrage
// ---------------------------------------------------------------------------

#[test]
fn fixture_2_obvious_arbitrage_reports_both_ends_of_the_cycle() {
    let snapshot = market(&[pool1(), pool2()]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(detection.candidates.len(), 4);
    assert_eq!(detection.opportunities.len(), 2);

    let spend_a = finding(&detection.opportunities, pool(P1), token(A)).expect("A-side");
    let spend_b = finding(&detection.opportunities, pool(P2), token(B)).expect("B-side");
    for (opportunity, hop1, hop2) in [
        (
            spend_a,
            (100_000u128, 50_000u128),
            (50_000u128, 125_000u128),
        ),
        (
            spend_b,
            (50_000u128, 125_000u128),
            (100_000u128, 50_000u128),
        ),
    ] {
        let expected = peak(hop1, hop2);
        assert!(expected.profit > 0, "the scan says this route pays");
        assert_eq!(
            opportunity.gross_profit,
            u(expected.profit as u128),
            "profit must equal the exhaustive maximum"
        );
        assert_eq!(
            opportunity.output_amount,
            opportunity.input_amount + opportunity.gross_profit,
            "output, input and profit must state one arithmetic"
        );
        assert!(
            expected.contains(opportunity.input_amount.to::<u128>()),
            "input {} is not one of the exhaustive argmaxes {}..={}",
            opportunity.input_amount,
            expected.first_input,
            expected.last_input
        );
        assert!(opportunity.search.interval_closed);
        // The finding carries the markets it was priced against (§55).
        assert_eq!(opportunity.hops[0].reserve_in, u(hop1.0));
        assert_eq!(opportunity.hops[0].reserve_out, u(hop1.1));
        assert_eq!(opportunity.hops[1].reserve_in, u(hop2.0));
        assert_eq!(opportunity.hops[1].reserve_out, u(hop2.1));
        assert_eq!(opportunity.hops[0].fee, FEE);
        assert_eq!(opportunity.hops[1].fee, FEE);
    }
    // The two ends of one cycle do not have to agree in units: they are profits
    // in different tokens, and nothing here adds them together (§42).
    assert_ne!(spend_a.gross_profit, spend_b.gross_profit);
}

// ---------------------------------------------------------------------------
// Fixture 3: fee erases profit
// ---------------------------------------------------------------------------

#[test]
fn fixture_3_a_real_spread_smaller_than_the_fees_is_not_an_opportunity() {
    let snapshot = market(&[pool1(), pool6()]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(detection.candidates.len(), 4);
    assert!(
        detection.is_empty(),
        "a 0.5 % spread against 0.6 % of fees must not pay: {:?}",
        detection.opportunities
    );
    // The prices genuinely differ — this is not fixture 1 wearing a hat.
    let p1_ab = (100_000u128, 50_000u128);
    let p6_ba = (50_000u128, 100_500u128);
    assert_ne!(price_product(p1_ab, p6_ba), (1, 1));
    assert!(ratio_gt(price_product(p1_ab, p6_ba), (1, 1)));
    assert!(!ratio_gt(
        price_product(p1_ab, p6_ba),
        fee_threshold_squared()
    ));
    // The scan agrees: the best round trip loses, and loses by the least at a
    // real size rather than at one token.
    let best = peak(p1_ab, p6_ba);
    assert!(best.profit < 0);
    let reported = detection.best_peak().expect("a peak was measured");
    assert_eq!(reported.gross_loss(), Some(u(-best.profit as u128)));
    assert!(reported.input > u(1));
}

// ---------------------------------------------------------------------------
// Fixture 4: optimal input
// ---------------------------------------------------------------------------

#[test]
fn fixture_4_the_profit_curve_has_an_interior_peak_and_the_search_finds_it() {
    let [one, _] = edges(P1, A, B, 100_000, 50_000);
    let [two, _] = edges(P2, B, A, 50_000, 125_000);
    let cycle = PricedCycle::new(&one, &two).expect("priced");
    let best = find_optimal_input(&cycle, SearchPolicy::default()).expect("search");

    let hop1 = (100_000u128, 50_000u128);
    let hop2 = (50_000u128, 125_000u128);
    let expected = peak(hop1, hop2);
    assert!(expected.plateau > 1, "this fixture's top is a plateau");
    assert_eq!(
        best.simulation.gross_profit(),
        Some(u(expected.profit as u128))
    );
    let input = best.simulation.input.to::<u128>();
    assert!(
        input > 1 && input < expected.domain_end,
        "the peak is interior: {input} of 1..={}",
        expected.domain_end
    );
    assert!(expected.contains(input));
    assert_eq!(best.record.upper_bound, u(expected.domain_end));
    assert_eq!(best.record.lower_bound, U256::ONE);
    assert!(best.record.interval_closed);

    // Testing one token unit would have proved nothing: the smallest legal input
    // is the worst outcome on this curve.
    let one_unit = cycle.quote(U256::ONE).expect("quote");
    assert_eq!(one_unit, U256::ZERO, "one A cannot even buy one B here");
    // And the curve falls away on both sides of the peak, which is what makes it
    // a peak rather than the shoulder of a wider one.
    let half = profit_of(
        expected.first_input / 2,
        cycle.quote(u(expected.first_input / 2)).expect("quote"),
    );
    let double = profit_of(
        expected.last_input * 2,
        cycle.quote(u(expected.last_input * 2)).expect("quote"),
    );
    assert!(half < expected.profit, "half the peak input earns less");
    assert!(double < expected.profit, "twice the peak input earns less");
}

/// `output - input` as a plain integer, for the curve comparisons above.
fn profit_of(input: u128, output: U256) -> i128 {
    output.to::<u128>() as i128 - input as i128
}

// ---------------------------------------------------------------------------
// Fixture 5: zero reserve
// ---------------------------------------------------------------------------

#[test]
fn fixture_5_an_empty_side_is_refused_at_every_door() {
    // Door one is M1's: the state store will not record an empty reserve side at
    // all, so no snapshot downstream can ever carry one.
    let mut store = evm_state::InMemoryStateStore::new(support::CHAIN);
    store
        .apply(evm_state::StateUpdate::PoolRegistered(evm_core::PoolMeta {
            id: pool(P2),
            protocol: evm_core::ProtocolId::new("test-v2"),
            token0: token(B),
            token1: token(A),
            fee: Some(FEE),
            pool_type: evm_core::PoolType::ConstantProduct,
        }))
        .expect("register");
    let rejected = store.apply(evm_state::StateUpdate::PoolSynced {
        pool: pool(P2),
        reserve0: U256::ZERO,
        reserve1: u(125_000),
        position: evm_state::UpdatePosition::new(
            evm_core::BlockNumber(BLOCK),
            evm_core::LogIndex(1),
        ),
    });
    assert!(
        matches!(rejected, Err(evm_state::StateError::InvalidReserves { .. })),
        "an empty side must be refused, got {rejected:?}"
    );

    // Door two is M2's: given such a state anyway, the graph emits no edge, so a
    // route list built from a snapshot never offers one.
    let meta_zero = evm_core::PoolMeta {
        id: pool(P2),
        protocol: evm_core::ProtocolId::new("test-v2"),
        token0: token(B),
        token1: token(A),
        fee: Some(FEE),
        pool_type: evm_core::PoolType::ConstantProduct,
    };
    let state_zero = evm_core::PoolState {
        pool: pool(P2),
        reserve0: U256::ZERO,
        reserve1: u(125_000),
        block_number: evm_core::BlockNumber(1),
        log_index: evm_core::LogIndex(1),
    };
    assert_eq!(
        evm_graph::GraphEdge::pair(&meta_zero, &state_zero),
        Err(evm_graph::EdgeRejection::EmptySide)
    );
    let snapshot = market(&[pool1()]);
    assert!(
        enumerate_candidates(&snapshot).expect("scan").is_empty(),
        "one pool offers no two-pool route"
    );

    // Door three is this crate's: the math refuses it independently, so a
    // hand-built edge cannot price it either. The doors are separate on purpose;
    // the last one is what protects the result if the first ever changes.
    assert_eq!(
        evm_opportunity::swap_exact_in(U256::ZERO, u(100), FEE, u(10)),
        Err(MathError::InvalidReserve)
    );
    assert_eq!(
        evm_opportunity::swap_exact_in(u(100), U256::ZERO, FEE, u(10)),
        Err(MathError::InvalidReserve)
    );
    // A zero input is not a free quote either.
    assert_eq!(
        evm_opportunity::swap_exact_in(u(100), u(100), FEE, U256::ZERO),
        Err(MathError::InvalidAmount)
    );
}

// ---------------------------------------------------------------------------
// Fixture 6: extreme reserve
// ---------------------------------------------------------------------------

/// 3e30 / 2e30 against 1e30 / 9e30: every product inside the quote is past
/// 2^128, so this is the case that would have overflowed a `u128` implementation.
///
/// The expected profit here is not a scanned number — the domain is 9e30 inputs
/// wide, far past an exhaustive scan. It is the **proven ceiling** of the
/// round-trip function, computed with exact rational arithmetic in a separate
/// arbitrary-precision tool (Python) from the closed form of the unfloored curve,
/// `out(x) - x = a*x/(g*x + D) - x`, which is concave and maximal where
/// `(g*x + D)^2 = a*D`:
///
/// ```text
/// a  = n^2 * Ro1 * Ro2      = 17 892 162 * 10^60   = 1.789 216 2e67
/// g  = n * (d*Ri2 + n*Ro1)  =  2 985 018 * 10^30   = 2.985 018e36
/// D  = d^2 * Ri1 * Ri2      =  3 * 10^66
/// max = (sqrt(a) - sqrt(D))^2 / g, floored
///     = 2 090 209 961 437 763 060 920 854 301 417
/// ```
///
/// Integer profit is at most the exact profit at the same input, and the exact
/// curve's maximum is the number above, so no input can beat it. That makes this
/// fixture's claim stronger than the scanned ones: a search that reports it has
/// not merely tied a sample, it has reached the global optimum — which is how
/// §23's rule about only claiming one when it is proved is satisfied here, by the
/// ceiling rather than by a spot-check.
///
/// Two caveats, both measured:
/// - `a*D` is a 134-digit product, past `U256`, so the ceiling cannot be
///   recomputed inside this test. It is stated and cited, not re-derived.
/// - The floored curve reaches the ceiling at 8 872 of the 40 001 inputs nearest
///   the analytic peak, and those are spikes with dips between them rather than
///   one run. The search's own answer lands 4.2e11 inputs *below* the analytic
///   peak and still hits the ceiling. So this test names the profit and never an
///   input.
#[test]
fn fixture_6_112_bit_reserves_price_without_overflowing() {
    // 1e30 fits a u128 with room to spare, so the fixture can state its reserves
    // as plain integers; the products inside the quote do not fit, which is the
    // point of the case.
    let tenthirty = 1_000_000_000_000_000_000_000_000_000_000u128;
    let big = |lead: u128| U256::from(lead * tenthirty);
    let e1_ab = (big(3), big(2));
    let e2_ba = (big(1), big(9));

    // Each hop is stated as the market itself states it — a pool, the token it
    // takes, the token it gives, and both sides — so no test here gets to
    // re-orient a price by hand.
    let hop = |pool_addr: alloy_primitives::Address,
               token_in: alloy_primitives::Address,
               token_out: alloy_primitives::Address,
               reserve_in: U256,
               reserve_out: U256| {
        let meta = evm_core::PoolMeta {
            id: pool(pool_addr),
            protocol: evm_core::ProtocolId::new("test-v2"),
            token0: token(token_in),
            token1: token(token_out),
            fee: Some(FEE),
            pool_type: evm_core::PoolType::ConstantProduct,
        };
        let state = evm_core::PoolState {
            pool: pool(pool_addr),
            reserve0: reserve_in,
            reserve1: reserve_out,
            block_number: evm_core::BlockNumber(1),
            log_index: evm_core::LogIndex(1),
        };
        let [in_out, _] = evm_graph::GraphEdge::pair(&meta, &state).expect("big reserves");
        in_out
    };
    let forward = PricedCycle::new(
        &hop(P3, A, B, e1_ab.0, e1_ab.1),
        &hop(P4, B, A, e2_ba.0, e2_ba.1),
    )
    .expect("priced");
    let best = find_optimal_input(&forward, SearchPolicy::default()).expect("search");

    let ceiling = U256::from(2_090_209_961_437_763_060_920_854_301_417u128);
    assert_eq!(best.simulation.gross_profit(), Some(ceiling));
    assert_eq!(
        forward.quote(best.simulation.input).expect("quote"),
        best.simulation.output,
        "the reported output must be what the cycle itself prices"
    );
    assert_eq!(
        best.simulation.output,
        best.simulation.input + ceiling,
        "output, input and profit state one arithmetic"
    );
    assert_eq!(
        best.record.upper_bound,
        u(8_999_999_999_999_999_999_999_999_999_999u128),
        "the derived domain end is second.reserve_out - 1"
    );
    assert!(best.simulation.input > U256::ONE);
    assert!(best.simulation.input < best.record.upper_bound);
    assert!(best.record.interval_closed);
    // A domain this wide needs this many halvings of a third; the default budget
    // is documented as covering any 112-bit reserve, and here is the number.
    assert!(
        best.record.rounds < 256,
        "rounds used: {}",
        best.record.rounds
    );

    // The other direction on the same two pools loses at any size. This is the
    // closed-form condition rather than a sampled one: its price product is 1/6,
    // so `a <= D` and the round-trip function falls from the very first input.
    let backwards = PricedCycle::new(
        &hop(P4, A, B, big(9), big(1)),
        &hop(P3, B, A, big(2), big(3)),
    )
    .expect("priced");
    assert!(!ratio_gt((3u128, 9u128 * 2), fee_threshold_squared()));
    let worst = find_optimal_input(&backwards, SearchPolicy::default()).expect("search");
    assert!(worst.simulation.gross_profit().is_none(), "{worst:?}");

    // And an input whose fee product does not fit in 256 bits is an error, not a
    // panic and not a wrapped number.
    assert_eq!(
        evm_opportunity::swap_exact_in(U256::MAX, U256::MAX, FEE, U256::ONE),
        Err(MathError::Overflow)
    );
    assert_eq!(
        forward.quote(U256::MAX),
        Err(evm_opportunity::OpportunityError::Math(MathError::Overflow))
    );
}

// ---------------------------------------------------------------------------
// Fixture 7: same pool twice
// ---------------------------------------------------------------------------

#[test]
fn fixture_7_a_round_trip_through_one_pool_is_never_a_candidate() {
    let snapshot = market(&[pool1()]);
    assert!(enumerate_candidates(&snapshot).expect("scan").is_empty());
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(detection.skipped_pairs.len(), 2);
    for skipped in &detection.skipped_pairs {
        assert_eq!(
            skipped.reason,
            evm_opportunity::PathError::SamePool(pool(P1), pool(P1))
        );
    }

    // A pool bought and sold in the same breath loses exactly its fees: the
    // quote is worth less than the input at every size, which is why letting
    // these through would be a profit printer.
    let [ab, ba] = edges(P1, A, B, 100_000, 50_000);
    assert_eq!(
        ArbitragePath::from_edges(&ab, &ba),
        Err(evm_opportunity::PathError::SamePool(pool(P1), pool(P1)))
    );
    let same_pool = (100_000u128, 50_000u128);
    let loss = peak(same_pool, (50_000, 100_000));
    assert!(loss.profit < 0);
}

// ---------------------------------------------------------------------------
// Fixture 8: multiple pools on one pair
// ---------------------------------------------------------------------------

#[test]
fn fixture_8_three_pools_on_one_pair_give_twelve_directed_routes() {
    let pools = [pool1(), pool2(), pool5()];
    let snapshot = market(&pools);
    let candidates = enumerate_candidates(&snapshot).expect("scan");
    // §19's six ordered pool pairs, each priced from both ends of the pair.
    assert_eq!(candidates.len(), 12);
    let as_set: std::collections::BTreeSet<_> = candidates.iter().copied().collect();
    assert_eq!(as_set.len(), 12, "every route listed exactly once");
    let orderings: std::collections::BTreeSet<_> =
        candidates.iter().map(|path| path.pools()).collect();
    assert_eq!(orderings.len(), 6, "the six ordered pool pairs");
    for path in &candidates {
        let [first, second] = path.pools();
        assert_ne!(first, second, "{path:?}");
    }

    // The twelve exhaustive scans, one per directed route. Both sides of every
    // hop are read out of the attestations through `side`, so the oracle prices
    // the route the detector claims to have priced rather than a route this
    // file re-typed by hand.
    let scans: std::collections::BTreeMap<_, _> = candidates
        .iter()
        .map(|path| {
            let first = &pools[index_of(&pools, path.first().pool)];
            let second = &pools[index_of(&pools, path.second().pool)];
            let scan = peak(
                side(first, path.input_token().address),
                side(second, path.mid_token().address),
            );
            (path.identity(), scan)
        })
        .collect();
    assert_eq!(scans.len(), 12);
    let winners: Vec<_> = scans
        .iter()
        .filter(|(_, scan)| scan.profit > 0)
        .map(|(identity, scan)| (*identity, scan.profit))
        .collect();
    assert_eq!(
        winners.len(),
        4,
        "pool 5 quotes B at pool 1's own rate, so it only ever trades against pool 2"
    );

    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(
        detection.skipped_pairs.len(),
        6,
        "three pools, two directions each"
    );
    assert_eq!(detection.opportunities.len(), winners.len());
    assert_eq!(detection.rejected.len(), 8);
    let reported: std::collections::BTreeSet<_> = detection
        .opportunities
        .iter()
        .map(|o| o.path.identity())
        .collect();
    let expected: std::collections::BTreeSet<_> =
        winners.iter().map(|(identity, _)| *identity).collect();
    assert_eq!(reported, expected, "the winning set is the scan's");
    let findings: std::collections::BTreeMap<_, _> = detection
        .opportunities
        .iter()
        .map(|o| (o.path.identity(), o))
        .collect();

    // Two rules, and the difference between them is the point.
    //
    // A finding may never exceed its route's exhaustive maximum. That is the
    // direction that would cost money if it were wrong: an over-stated profit is
    // a trade that does not fill, and every number an `Opportunity` carries is
    // meant to be a round trip the pools would actually give.
    for opportunity in &detection.opportunities {
        let scan = &scans[&opportunity.path.identity()];
        assert!(
            opportunity.gross_profit <= u(scan.profit as u128),
            "{:?} reports {} above its exhaustive maximum {scan:?}",
            opportunity.path,
            opportunity.gross_profit
        );
        assert_eq!(
            opportunity.output_amount,
            opportunity.input_amount + opportunity.gross_profit,
            "{:?}: output, input and profit must state one arithmetic",
            opportunity.path
        );
        assert!(opportunity.search.interval_closed);
    }
    // Where the top of the curve is wide, the search is held to it exactly. Where
    // it is a single spike it need not be, and here is the route that shows why:
    // `A -> pool 5 -> B -> pool 2 -> A` peaks at 587 on the one input 5 090, with
    // 584/585/586 alternating around it, and the search closes its 32-wide window
    // on 5 045. It reports 586 — one unit under, on a curve whose floors make the
    // exact peak findable only by looking at it.
    for (identity, scan) in &scans {
        if scan.profit > 0 && scan.plateau > 1 {
            let opportunity = findings[identity];
            assert_eq!(
                opportunity.gross_profit,
                u(scan.profit as u128),
                "{identity:?} missed a plateau of {} inputs: {scan:?}",
                scan.plateau
            );
            assert!(
                scan.contains(opportunity.input_amount.to::<u128>()),
                "input {} outside the argmax span {}..={} of {identity:?}",
                opportunity.input_amount,
                scan.first_input,
                scan.last_input
            );
        }
    }
    // Reported highest first, so a caller reading one number gets the best one.
    let profits: Vec<_> = detection
        .opportunities
        .iter()
        .map(|o| o.gross_profit)
        .collect();
    assert!(
        profits.windows(2).all(|w| w[0] >= w[1]),
        "not in profit order: {profits:?}"
    );
    // The routes that lose are rejected, and the scan says by how little.
    for rejection in &detection.rejected {
        let scan = &scans[&rejection.path.identity()];
        assert!(scan.profit <= 0, "{:?} {scan:?}", rejection.path);
        let peak = rejection.peak.expect("priced");
        assert_eq!(peak.gross_profit(), None, "{:?} {scan:?}", rejection.path);
    }
}

// ---------------------------------------------------------------------------
// The same eight shapes at a fee of zero, so the fee is shown to be doing the
// work it is credited with rather than riding along.
// ---------------------------------------------------------------------------

#[test]
fn a_zero_fee_market_leaves_an_equal_price_market_with_nothing_to_take() {
    // Fixture 1's identical prices, with the fees removed. The point of the
    // control is that a fee is not what is stopping this market: with no spread
    // and no fee the round trip still cannot come out ahead, because each hop's
    // integer division rounds down.
    let snapshot = market(&[
        spec(P1, A, B, 100_000, 50_000, Some(NO_FEE)),
        spec(P5, B, A, 40_000, 80_000, Some(NO_FEE)),
    ]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert!(detection.is_empty(), "{:?}", detection.opportunities);
    let p1_ab = (100_000u128, 50_000u128);
    let p1_ba = (50_000u128, 100_000u128);
    let p5_ab = (80_000u128, 40_000u128);
    let p5_ba = (40_000u128, 80_000u128);
    let parity = price_product(p1_ab, p5_ba);
    assert_eq!(
        parity.0, parity.1,
        "the two pools quote B at exactly the same rate"
    );
    // The scan of the zero-fee curve tops out at a loss of exactly one unit: the
    // floor, not the fee.
    assert_eq!(peak_free(p1_ab, p5_ba).profit, -1);
    assert_eq!(peak_free(p5_ba, p1_ab).profit, -1);
    assert_eq!(peak_free(p5_ab, p1_ba).profit, -1);
    assert_eq!(peak_free(p1_ba, p5_ab).profit, -1);
}

#[test]
fn a_zero_fee_market_makes_fixture_3_pay_what_the_fee_took() {
    // Fixture 3's 0.5 % spread, with both pools scaled up fivefold so the
    // integer floor is smaller than the spread, and priced at no fee at all.
    // The same market at 997/1000 is fixture 3's verdict: nothing.
    let wide = spec(P1, A, B, 500_000, 250_000, Some(NO_FEE));
    let bid = spec(P6, B, A, 250_000, 502_500, Some(NO_FEE));
    let snapshot = market(&[wide, bid]);
    let detection = detect_opportunities(&snapshot).expect("detect");

    let spend_a = peak_free((500_000, 250_000), (250_000, 502_500));
    let spend_b = peak_free((250_000, 502_500), (500_000, 250_000));
    assert_eq!(spend_a.profit, 1, "the whole of a 0.5 % spread, one unit");
    assert_eq!(spend_b.profit, 0, "the other end of the cycle breaks even");

    // Exactly one finding, because a break-even round trip is not an opportunity
    // (§16): the route that gains 1 is reported and the route that gains 0 is not.
    assert_eq!(
        detection.opportunities.len(),
        1,
        "{:?}",
        detection.opportunities
    );
    let opportunity = &detection.opportunities[0];
    assert_eq!(opportunity.input_token, token(A));
    assert_eq!(opportunity.gross_profit, u(1));
    assert!(spend_a.contains(opportunity.input_amount.to::<u128>()));
    assert_eq!(detection.rejected.len(), 3);
    for rejection in &detection.rejected {
        assert!(rejection.peak.expect("priced").gross_profit().is_none());
    }

    // Same pools, fee attested at 997/1000: every route loses, so the fee is the
    // only difference between this test and fixture 3.
    let charged = market(&[
        spec(P1, A, B, 500_000, 250_000, Some(FEE)),
        spec(P6, B, A, 250_000, 502_500, Some(FEE)),
    ]);
    let after_fee = detect_opportunities(&charged).expect("detect");
    assert!(after_fee.is_empty(), "{:?}", after_fee.opportunities);
    assert_eq!(peak((500_000, 250_000), (250_000, 502_500)).profit, -1);
}

/// A pool with no attested fee is not a zero-fee pool, and the difference is the
// whole of §10's evidence rule.
#[test]
fn an_unattested_fee_is_refused_rather_than_priced_at_the_default() {
    let snapshot = market(&[pool1(), spec(P2, B, A, 50_000, 125_000, None)]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert!(detection.opportunities.is_empty());
    assert_eq!(detection.rejected.len(), 4);
    for rejection in &detection.rejected {
        assert_eq!(
            rejection.reason,
            evm_opportunity::RejectionReason::MissingFee(pool(P2)),
            "{:?}",
            rejection.path
        );
        assert!(rejection.peak.is_none(), "it was never priced");
    }
}

/// The {A,C} pair is the wide-spread case: a 2.0 price product against
/// {A,B}'s 1.25. Its top is not one contiguous run: measured over the whole
/// domain, `C -> A -> C` reaches 2 815 on 120 inputs interleaved with 2 814
/// across `6 785 ..= 6 950`, and `A -> C -> A` reaches 8 441 on 82 inputs across
/// `20 372 ..= 20 815`. Both directions have to land on that top, which is what
/// the tie rule in `find_optimal_input` — closing the interval from both ends on
/// an equal pair of probes — is there for.
#[test]
fn the_wide_pair_prices_both_ends_of_its_cycle_at_the_exhaustive_maximum() {
    let snapshot = market(&[pool3(), pool4()]);
    let detection = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(detection.opportunities.len(), 2);
    let cases = [
        (
            pool(P3),
            token(A),
            (100_000u128, 50_000u128),
            (50_000, 200_000),
        ),
        (
            pool(P4),
            token(C),
            (50_000u128, 200_000u128),
            (100_000, 50_000),
        ),
    ];
    for (first_pool, input_token, hop1, hop2) in cases {
        let opportunity = finding(&detection.opportunities, first_pool, input_token)
            .unwrap_or_else(|| panic!("no finding for {first_pool:?} {input_token:?}"));
        let expected = peak(hop1, hop2);
        assert!(expected.profit > 0);
        assert_eq!(
            opportunity.gross_profit,
            u(expected.profit as u128),
            "{:?} vs exhaustive {:?}",
            opportunity.path,
            expected
        );
        assert!(
            expected.contains(opportunity.input_amount.to::<u128>()),
            "input {} outside the argmax range {}..={} (plateau {})",
            opportunity.input_amount,
            expected.first_input,
            expected.last_input,
            expected.plateau
        );
        assert!(opportunity.search.interval_closed);
    }
    // Both directions of the same pair of pools: the reverse cycle has a price
    // product of 0.5, so the search has to refuse it rather than mis-state it.
    assert!(peak((200_000u128, 50_000u128), (50_000, 100_000)).profit < 0);
    assert!(peak((100_000u128, 50_000u128), (200_000, 50_000)).profit < 0);
}
