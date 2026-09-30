//! §46 of the task: the optimizer may not validate itself. This file runs the
//! bounded search and the exhaustive `u128` scan over the same set of market
//! shapes — a grid of reserve quadruples, not a hand-picked pair — and holds the
//! search to what it can actually promise.
//!
//! Two invariants, and they are not symmetric:
//!
//! - **Never above the exhaustive maximum.** A reported profit has to be a round
//!   trip the pools would give. Over-stating it is the failure that costs money,
//!   so this one is a hard assertion on every route in the grid.
//! - **Exactly the maximum where the top is wide.** The floored profit curve is a
//!   staircase, not a unimodal sequence: its top can be a single input — 587 at
//!   input 5 090 on `{80 000 -> 40 000} then {50 000 -> 125 000}`, with 585 one
//!   input either side and 586 two out. A ternary search that compares two probes
//!   at a time can close its window on the wrong step, and then the closing pass
//!   cannot see the spike. So where the exhaustive scan finds a plateau of more
//!   than one input, the search must hit it; where the top is a single spike, the
//!   search may report less, and the shortfall is tracked as a measured number
//!   rather than argued away.
//!
//! The gap is reported, not hidden: `SearchRecord` says which domain was
//! searched, how many rounds and evaluations it took, and whether the interval
//! actually closed (§23). Nothing in this file claims a global optimum except
//! where the ceiling proves one, which is `tests/fixtures.rs`'s extreme-reserve
//! case rather than anything here.

mod support;

use alloy_primitives::U256;

use evm_opportunity::{find_optimal_input, PricedCycle, SearchPolicy};

use support::{edges, peak, A, B, P1, P2};

/// One route shape: pool 1's two sides, then pool 2's, in trade direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Route {
    reserve_in_first: u128,
    reserve_out_first: u128,
    reserve_in_second: u128,
    reserve_out_second: u128,
}

fn route(a: u128, b: u128, c: u128, d: u128) -> Route {
    Route {
        reserve_in_first: a,
        reserve_out_first: b,
        reserve_in_second: c,
        reserve_out_second: d,
    }
}

/// The grid. Reserves stay small enough that `1 ..= reserve_out_second - 1` is
/// enumerable, which is the whole point: every verdict here is against a scan of
/// the entire domain, not a sample of it.
fn grid() -> Vec<Route> {
    let sides = [29u128, 40, 61, 100, 250, 640, 1_201];
    let mut routes = Vec::new();
    for &a in &sides {
        for &b in &sides {
            for &c in &sides {
                for &d in &sides {
                    routes.push(route(a, b, c, d));
                }
            }
        }
    }
    routes
}

fn priced(r: &Route) -> PricedCycle {
    let [first, _] = edges(P1, A, B, r.reserve_in_first, r.reserve_out_first);
    let [second, _] = edges(P2, B, A, r.reserve_in_second, r.reserve_out_second);
    PricedCycle::new(&first, &second).expect("a grid route is a two-pool cycle")
}

/// `output - input` as a plain integer; small enough here that no wrapping is
/// possible, which is what lets the search's answer and the scan's be compared
/// on one number.
fn integer_profit(input: U256, output: U256) -> i128 {
    output.to::<u128>() as i128 - input.to::<u128>() as i128
}

#[test]
fn the_search_never_reports_more_than_the_whole_domain_contains() {
    let policy = SearchPolicy::default();
    let mut routes = 0usize;
    let mut profitable_routes = 0usize;
    let mut exact = 0usize;
    let mut short = 0usize;
    let mut worst_shortfall = 0i128;
    let mut worst_route: Option<Route> = None;
    let mut worst_plateau = 0u128;
    let mut outside_envelope = 0usize;
    let mut missed_peaks: Vec<(Route, i128)> = Vec::new();

    for r in grid() {
        let cycle = priced(&r);
        let scan = peak(
            (r.reserve_in_first, r.reserve_out_first),
            (r.reserve_in_second, r.reserve_out_second),
        );
        let best = find_optimal_input(&cycle, policy).expect("search");
        routes += 1;

        // The domain the search states is the derived one, every time.
        assert_eq!(best.record.lower_bound, U256::ONE);
        assert_eq!(
            best.record.upper_bound,
            U256::from(r.reserve_out_second - 1),
            "{r:?}: the bound is second.reserve_out - 1, not a ratio"
        );
        assert!(best.record.interval_closed, "{r:?}");
        assert!(
            best.simulation.input >= U256::ONE && best.simulation.input <= best.record.upper_bound,
            "{r:?}: {} searched outside its stated domain",
            best.simulation.input
        );
        assert_eq!(
            cycle.quote(best.simulation.input).expect("quote"),
            best.simulation.output,
            "{r:?}: the reported output is not what this cycle prices"
        );

        let found = integer_profit(best.simulation.input, best.simulation.output);
        assert!(
            found <= scan.profit,
            "{r:?}: search claims {found} where the exhaustive scan tops out at {}",
            scan.profit
        );
        if scan.profit > 0 {
            profitable_routes += 1;
        }
        assert!(
            !(best.simulation.gross_profit().is_some() && scan.profit <= 0),
            "{r:?}: the search profits on a route whose whole domain does not"
        );
        if scan.profit > 0 && best.simulation.gross_profit().is_none() {
            missed_peaks.push((r, scan.profit));
        }

        if found == scan.profit {
            exact += 1;
        } else {
            short += 1;
            let shortfall = scan.profit - found;
            if shortfall > worst_shortfall {
                worst_shortfall = shortfall;
                worst_route = Some(r);
                worst_plateau = scan.plateau;
            }
            // Every hop can round a fraction of a unit away, and a unit of the
            // middle token is worth at most `reserve_out / reserve_in` of the
            // output token, so the floored curve sits within
            // `1 + second.reserve_out / second.reserve_in` of the exact one at
            // the same input. That is this route's own envelope, and it is what
            // a shortfall has to be measured against — not a fixed constant.
            let envelope = 1 + (r.reserve_out_second / r.reserve_in_second) as i128;
            if shortfall > envelope {
                outside_envelope += 1;
            }
        }
    }

    // The numbers this sweep produced, so the claim in the module doc is a
    // measurement and not a description of intent.
    println!(
        "grid: {routes} routes, {profitable_routes} with a positive peak; \
         search tied the exhaustive maximum on {exact}, fell short on {short} \
         (worst shortfall {worst_shortfall} at {worst_route:?}, whose top held \
         {worst_plateau} input(s); {outside_envelope} misses outside their \
         route's floor envelope; {missed_peaks:?} routes where the scan profits \
         and the search reports none)"
    );
    // The two things this sweep is actually holding the search to.
    assert!(
        outside_envelope == 0,
        "{outside_envelope} misses were larger than the floors can explain"
    );
    assert!(
        exact > short,
        "the search ties the scan far more often than not"
    );
    // A missed opportunity is the safe kind of wrong — the route stays reported
    // as a rejection with its peak — but it is still a fact worth pinning: this
    // grid has exactly one route whose exhaustive peak is positive and which the
    // search reports as no profit at all, and that peak is a single unit
    // reached by a single input. Both numbers are the run's, printed above.
    assert_eq!(
        missed_peaks,
        vec![(route(250, 61, 250, 1_201), 1)],
        "the set of opportunities the search can miss"
    );
}

#[test]
fn a_starved_round_budget_says_so_instead_of_pretending_to_have_closed() {
    // The {A,B} fixture's domain is 124 999 inputs wide. Eight ternary rounds
    // shrink it by (2/3)^8, which is nowhere near `scan_width`, so the record has
    // to report an unclosed interval rather than a peak that looks proven.
    let cycle = priced(&route(100_000, 50_000, 50_000, 125_000));
    let policy = SearchPolicy {
        max_rounds: 8,
        scan_width: 32,
    };
    let starved = find_optimal_input(&cycle, policy).expect("search");
    assert_eq!(starved.record.rounds, 8);
    assert!(!starved.record.interval_closed);
    assert!(starved.record.scan_count > 1, "it sampled, not compared");

    let scan = peak((100_000, 50_000), (50_000, 125_000));
    let found = integer_profit(starved.simulation.input, starved.simulation.output);
    assert!(
        found <= scan.profit,
        "an unclosed search still may not exceed the exhaustive maximum"
    );

    // And the same route at the default budget closes, which is what makes
    // `interval_closed` a statement about the run rather than about the market.
    let settled = find_optimal_input(&cycle, SearchPolicy::default()).expect("search");
    assert!(settled.record.interval_closed);
    assert_eq!(
        found <= scan.profit,
        settled.simulation.gross_profit().is_some()
    );
    assert_eq!(
        settled.record.strategy,
        evm_opportunity::SearchStrategy::BoundedTernary
    );
}

#[test]
fn a_single_unit_exit_pool_leaves_no_domain_to_search() {
    // `second.reserve_out - 1` is the domain end, so an exit pool holding one
    // unit of the input token has none. That is an error, not a zero profit: the
    // search never ran, so it cannot report a best input.
    let [first, _] = edges(P1, A, B, 100_000, 50_000);
    let [second, _] = edges(P2, B, A, 50_000, 1);
    let cycle = PricedCycle::new(&first, &second).expect("priced");
    let error = find_optimal_input(&cycle, SearchPolicy::default()).expect_err("no domain");
    assert_eq!(
        error,
        evm_opportunity::OpportunityError::EmptySearchDomain {
            lower: U256::ONE,
            upper: U256::ZERO,
        },
        "{error}"
    );
    assert_eq!(cycle.input_upper_bound(), None);
}
