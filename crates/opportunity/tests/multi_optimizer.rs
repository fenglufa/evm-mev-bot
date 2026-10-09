//! §13–§18 and the Optimizer half of §45: a bounded search for the input that
//! pays best on a multi-hop route, checked against a brute-force scan.
//!
//! The seven cases §45 names for the optimizer are all here — small brute-force
//! oracle, boundary optimum, interior optimum, all-negative, zero-profit, large
//! `U256`, determinism — plus the two properties the task cares about most: the
//! candidate space is finite by policy (§15), and the route is never modified
//! (§17).
//!
//! The oracle is [`scan`]: every input in the window, priced by the `u128` fold
//! in `tests/support`, with no call into the crate. Where the search is allowed
//! to *equal* the oracle is a rule, not a preference: a domain small enough to
//! walk is walked, and then the answer must agree input-for-input (§16). A domain
//! too wide for that is sampled, and then the only promise is that the search
//! never reports **more** than the scan found — the test that shows the gap is
//! the point of §13, so it is asserted rather than glossed.

mod support;

use alloy_primitives::{address, Address, U256};

use evm_core::{BlockNumber, Fee};
use evm_graph::GraphSnapshot;
use evm_opportunity::{
    optimize, search_domain, Gross, MultiHopRoute, OptimizationPolicy, OptimizationStrategy,
    RouteError, Termination,
};
use evm_opportunity::{MathError, OpportunityError, Price};

use support::{market, pool, spec, token, A, B, C, CHAIN, FEE, P1, P2, P3, P4, P5};

const D: Address = address!("0x000000000000000000000000000000000000000d");

fn edge(p: Address, from: Address, to: Address) -> evm_graph::EdgeId {
    evm_graph::EdgeId::new(pool(p), token(from), token(to))
}

fn u(x: u128) -> U256 {
    U256::from(x)
}

// ---------------------------------------------------------------------------
// Fixtures. Same three markets as `tests/multihop.rs`, at the same scale: big
// reserves, small inputs, so a price gap is not swamped by slippage.
// ---------------------------------------------------------------------------

const TRIANGLE_HOPS: [(u128, u128); 3] = [
    (1_000_000, 1_100_000),
    (1_000_000, 1_000_000),
    (1_000_000, 1_000_000),
];
const SQUARE_HOPS: [(u128, u128); 4] = [
    (1_000_000, 1_100_000),
    (1_000_000, 1_000_000),
    (1_000_000, 1_000_000),
    (1_000_000, 1_000_000),
];
const PAIR_HOPS: [(u128, u128); 2] = [(1_000_000, 500_000), (500_000, 1_250_000)];
/// The same pair a thousand times shallower: the same 1.25 price product, but a
/// domain of 1 249 inputs — small enough that a search may walk all of it and
/// therefore may call its answer the maximum (§16).
const SHALLOW_PAIR_HOPS: [(u128, u128); 2] = [(1_000, 500), (500, 1_250)];

fn triangle() -> GraphSnapshot {
    market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, A, 1_000_000, 1_000_000, Some(FEE)),
    ])
}

fn triangle_route() -> MultiHopRoute {
    MultiHopRoute::new(
        &triangle(),
        &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)],
    )
    .expect("the triangle closes")
}

fn square_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, D, 1_000_000, 1_000_000, Some(FEE)),
        spec(P4, D, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    MultiHopRoute::new(
        &snapshot,
        &[
            edge(P1, A, B),
            edge(P2, B, C),
            edge(P3, C, D),
            edge(P4, D, A),
        ],
    )
    .expect("the square closes")
}

fn pair_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 500_000, Some(FEE)),
        spec(P2, B, A, 500_000, 1_250_000, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).expect("the pair closes")
}

/// [`pair_route`]'s market at a thousandth the depth, so its whole domain fits
/// inside the exhaustive limit.
fn shallow_pair_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000, 500, Some(FEE)),
        spec(P2, B, A, 500, 1_250, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)])
        .expect("the shallow pair closes")
}

/// Three pools of equal depth: no direction of this route can pay, because the
/// fees take a slice of a round trip that buys nothing extra.
fn symmetric_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("the symmetric triangle closes")
}

/// The same triangle a thousand times deeper, so the domain is a number no
/// `u64` can hold (§10: pricing is `U256` end to end, and a search that widened
/// to `u128` internally would not survive it).
fn deep_triangle_route() -> MultiHopRoute {
    let deep = 1_000_000_000_000_000_000_000_000_000u128; // 1e30
    let wider = 1_100_000_000_000_000_000_000_000_000u128; // 1.1e30
    let snapshot = market(&[
        spec(P1, A, B, deep, wider, Some(FEE)),
        spec(P2, B, C, deep, deep, Some(FEE)),
        spec(P3, C, A, deep, deep, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("the deep triangle closes")
}

/// A route with one pool whose fee nobody proved.
fn unattested_route() -> MultiHopRoute {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, None),
        spec(P3, C, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("the shape closes")
}

// ---------------------------------------------------------------------------
// The brute-force oracle.
// ---------------------------------------------------------------------------

/// One hop, `u128`, written from §9's formula and from nothing else.
fn quote(reserve_in: u128, reserve_out: u128, amount: u128, fee: Fee) -> u128 {
    let retained = amount * fee.numerator as u128;
    (retained * reserve_out) / (reserve_in * fee.denominator as u128 + retained)
}

/// The round trip at one input over `hops`.
fn round_trip(hops: &[(u128, u128)], x: u128) -> u128 {
    let mut amount = x;
    for (reserve_in, reserve_out) in hops {
        amount = quote(*reserve_in, *reserve_out, amount, FEE);
        if amount == 0 {
            return 0;
        }
    }
    amount
}

/// Every input in `low ..= high`, priced. `first` is the smallest input that
/// reaches `profit`, which is the same tie rule the optimizer documents, so a
/// disagreement about the input is a real disagreement and not two conventions.
struct Scan {
    first: u128,
    output: u128,
    profit: i128,
    maximisers: u128,
}

fn scan(hops: &[(u128, u128)], low: u128, high: u128) -> Scan {
    let mut best: Option<i128> = None;
    let mut first = 0u128;
    let mut output = 0u128;
    let mut maximisers = 0u128;
    for x in low..=high {
        let profit = round_trip(hops, x) as i128 - x as i128;
        match best {
            None => {
                best = Some(profit);
                first = x;
                output = round_trip(hops, x);
                maximisers = 1;
            }
            Some(current) => {
                if profit > current {
                    best = Some(profit);
                    first = x;
                    output = round_trip(hops, x);
                    maximisers = 1;
                } else if profit == current {
                    maximisers += 1;
                }
            }
        }
    }
    Scan {
        first,
        output,
        profit: best.expect("a domain of at least one input"),
        maximisers,
    }
}

/// The profit a search reported, as a signed number, so it can be compared with
/// a scan without pretending a loss is zero.
fn profit_of(input: U256, output: U256, gross: Gross) -> i128 {
    match gross {
        Gross::Gain(amount) => amount.to::<u128>() as i128,
        Gross::Even => 0,
        Gross::Loss(amount) => -(amount.to::<u128>() as i128),
    }
    .assert_matches_input(input, output)
}

trait Asserted {
    fn assert_matches_input(self, input: U256, output: U256) -> i128;
}

impl Asserted for i128 {
    /// The signed profit has to be the difference the quote actually shows; if
    /// the helper and the numbers diverge the test is lying about its own oracle.
    fn assert_matches_input(self, input: U256, output: U256) -> i128 {
        let from_numbers = output.to::<u128>() as i128 - input.to::<u128>() as i128;
        assert_eq!(
            self, from_numbers,
            "the gross and the quoted amounts disagree"
        );
        self
    }
}

// ---------------------------------------------------------------------------
// §16: the small domain, where the search must equal the scan
// ---------------------------------------------------------------------------

/// A domain small enough to walk is walked, and then `best_input`,
/// `best_output` and `best_profit` must all equal the scan's — not merely its
/// profit. The tie rule is tested at the same time: eleven inputs share the top
/// step below, and the smallest of them is the answer.
#[test]
fn a_walkable_domain_is_walked_and_agrees_input_for_input() {
    let route = triangle_route();
    for high in [100u128, 500, 1_000, 4_096] {
        let found = optimize(&route, u(1), u(high), OptimizationPolicy::default())
            .expect("search the triangle");
        let oracle = scan(&TRIANGLE_HOPS, 1, high);
        assert_eq!(
            found.search.strategy,
            OptimizationStrategy::Exhaustive,
            "a domain of {high} inputs is inside the exhaustive limit"
        );
        assert_eq!(found.search.termination, Termination::DomainExhausted);
        assert_eq!(found.search.best_input, u(oracle.first), "to {high}");
        assert_eq!(found.search.best_output, u(oracle.output), "to {high}");
        assert_eq!(
            found.search.gross.gain(),
            if oracle.profit > 0 {
                Some(u(oracle.profit as u128))
            } else {
                None
            },
            "to {high}"
        );
        assert_eq!(
            found.search.evaluations, high as u64,
            "every input was priced, none skipped"
        );
        assert_eq!(found.search.refusals, 0, "nothing overflowed at this scale");
    }
    // And the tie count proves the claim is about inputs, not just amounts: the
    // plateau the search lands on has more than one maximiser.
    let plateau = optimize(&route, u(1), u(40), OptimizationPolicy::default()).expect("search");
    let oracle = scan(&TRIANGLE_HOPS, 1, 40);
    assert!(
        oracle.maximisers > 1,
        "the scan found {} inputs at the top step",
        oracle.maximisers
    );
    assert_eq!(plateau.search.best_input, u(oracle.first));
}

/// The four-hop route is the one M9.3 never hands over, so its search is the one
/// §13's warning is really about — and on a walkable domain it still agrees with
/// the scan.
#[test]
fn a_four_hop_route_searches_the_same_way() {
    let route = square_route();
    let found =
        optimize(&route, u(1), u(1_000), OptimizationPolicy::default()).expect("search the square");
    let oracle = scan(&SQUARE_HOPS, 1, 1_000);
    assert_eq!(found.hop_count(), 4);
    assert_eq!(found.search.best_input, u(oracle.first));
    assert_eq!(found.search.best_output, u(oracle.output));
    assert_eq!(
        profit_of(
            found.search.best_input,
            found.search.best_output,
            found.search.gross
        ),
        oracle.profit
    );
    assert!(found.search.gross.is_gain(), "{:?}", found.search.gross);
}

/// The two-hop round trip through M11's type must give the same answer M3's
/// machine gives over the same domain — otherwise the new search is a second
/// opinion about the same market rather than a generalisation of it.
#[test]
fn a_two_hop_route_agrees_with_the_two_hop_scan() {
    let route = pair_route();
    let found =
        optimize(&route, u(1), u(4_096), OptimizationPolicy::default()).expect("search the pair");
    let oracle = scan(&PAIR_HOPS, 1, 4_096);
    assert_eq!(found.search.best_input, u(oracle.first));
    assert_eq!(found.search.best_output, u(oracle.output));
    assert_eq!(
        profit_of(
            found.search.best_input,
            found.search.best_output,
            found.search.gross
        ),
        oracle.profit
    );
}

// ---------------------------------------------------------------------------
// §45: boundary and interior optima
// ---------------------------------------------------------------------------

/// When the caller's window ends before the route's peak does, the answer is the
/// window edge, and the result says so by echoing the clipped domain.
#[test]
fn a_window_that_ends_before_the_peak_returns_its_own_edge() {
    let route = triangle_route();
    let found = optimize(&route, u(1), u(2_000), OptimizationPolicy::default())
        .expect("search a short window");
    assert_eq!(found.search.domain_max, u(2_000), "the caller's window");
    assert_eq!(
        found.search.best_input, found.search.domain_max,
        "the peak of the whole curve is beyond what was asked"
    );
    let oracle = scan(&TRIANGLE_HOPS, 1, 2_000);
    assert_eq!(found.search.best_input, u(oracle.first));
    // The route's own ceiling is further out, so this window is not the domain.
    assert!(found.search.domain_max < route.input_upper_bound().expect("a domain"));
    assert!(!found.search.covered_the_route_domain(&route));
}

/// The opposite case: the peak sits strictly inside the window, which is what
/// makes the search worth running at all.
#[test]
fn an_interior_peak_is_found_and_is_not_a_domain_edge() {
    let route = triangle_route();
    let found = optimize(&route, u(1), u(100), OptimizationPolicy::default())
        .expect("search a window around the peak");
    let oracle = scan(&TRIANGLE_HOPS, 1, 100);
    assert!(
        found.search.best_input > found.search.domain_min,
        "the answer is not the floor"
    );
    assert!(
        found.search.best_input < found.search.domain_max,
        "the answer is not the ceiling either"
    );
    assert_eq!(found.search.best_input, u(oracle.first));
    assert_eq!(
        found.search.domain_min,
        found.search.best_input - u(oracle.first - 1),
        "the domain floor is the first input searched"
    );
}

/// A caller's window that runs past the route's ceiling is clipped to it, and
/// the clip is reported rather than hidden. What is *not* claimed here is
/// coverage: this route's domain is 999 999 inputs wide, which is wider than the
/// policy's own exhaustive cap, so the clipped domain is sampled and the result
/// may not call its answer the maximum. The next test is the case where it may.
#[test]
fn a_window_past_the_routes_ceiling_is_clipped_and_says_so() {
    let route = triangle_route();
    let ceiling = route.input_upper_bound().expect("a domain");
    let found = optimize(&route, u(1), u(u128::MAX), OptimizationPolicy::default())
        .expect("search the whole route");
    assert_eq!(found.search.domain_max, ceiling);
    assert_eq!(found.search.domain_min, u(1));
    assert_eq!(
        found.search.strategy,
        OptimizationStrategy::CoarseThenRefine,
        "the route's own domain is wider than any policy may walk"
    );
    assert!(
        !found.search.covered_the_route_domain(&route),
        "a sampled search does not get to claim it saw the peak"
    );
    // `search_domain` is the same clipping, visible without running a search.
    assert_eq!(
        search_domain(&route, u(1), u(u128::MAX)),
        Some((u(1), ceiling))
    );
    assert_eq!(
        search_domain(&route, u(500), u(400)),
        None,
        "an inverted window"
    );
    assert_eq!(
        search_domain(&route, U256::ZERO, u(10)),
        Some((u(1), u(10))),
        "a floor of zero is not a legal input, so the domain starts at one"
    );
}

/// The other side of the same claim: when the route's whole domain is small
/// enough to walk, the clipped search is exhaustive over it, says so, and then
/// §16 requires it to agree with the brute-force scan input for input — with no
/// window edge and no sampling left to blame.
#[test]
fn a_walkable_route_domain_is_claimed_and_matches_the_full_scan() {
    let route = shallow_pair_route();
    let ceiling = route.input_upper_bound().expect("a domain");
    assert_eq!(ceiling, u(1_249), "the fixture's own depth sets the domain");
    let found = optimize(&route, u(1), u(u128::MAX), OptimizationPolicy::default())
        .expect("search the shallow pair");
    assert_eq!(found.search.domain_max, ceiling, "the clip is the domain");
    assert_eq!(
        found.search.strategy,
        OptimizationStrategy::Exhaustive,
        "and that domain was walked"
    );
    assert!(found.search.covered_the_route_domain(&route));
    assert_eq!(found.search.evaluations, 1_249);
    assert_eq!(found.search.refusals, 0);
    let oracle = scan(&SHALLOW_PAIR_HOPS, 1, 1_249);
    assert_eq!(found.search.best_input, u(oracle.first));
    assert_eq!(found.search.best_output, u(oracle.output));
    assert!(found.search.gross.is_gain(), "{:?}", found.search.gross);
    assert_eq!(
        profit_of(
            found.search.best_input,
            found.search.best_output,
            found.search.gross
        ),
        oracle.profit
    );
    // A narrower window over the same route does not get to make the claim.
    let partial = optimize(&route, u(1), u(500), OptimizationPolicy::default()).expect("search");
    assert!(!partial.search.covered_the_route_domain(&route));
}

// ---------------------------------------------------------------------------
// §4: all-negative and zero-profit are answers, and neither is a gain
// ---------------------------------------------------------------------------

/// A route that cannot pay at any input is reported as its smallest loss. The
/// trap this test exists for is `best_profit`, which is zero here for the same
/// reason it is zero for a break-even route — which is exactly why `gross` is
/// the field downstream layers read (§4).
#[test]
fn a_route_that_never_pays_reports_its_smallest_loss() {
    let route = symmetric_route();
    let found = optimize(&route, u(1), u(1_000), OptimizationPolicy::default())
        .expect("search a route that cannot pay");
    let oracle = scan(
        &TRIANGLE_HOPS
            .iter()
            .map(|_| (1_000_000u128, 1_000_000u128))
            .collect::<Vec<_>>(),
        1,
        1_000,
    );
    assert!(
        matches!(found.search.gross, Gross::Loss(_)),
        "{:?}",
        found.search.gross
    );
    assert_eq!(found.search.best_profit, U256::ZERO, "a loss pays nothing");
    assert_eq!(
        profit_of(
            found.search.best_input,
            found.search.best_output,
            found.search.gross
        ),
        oracle.profit,
        "and the loss is the scan's smallest loss, not an arbitrary one"
    );
    assert_eq!(found.search.best_input, u(oracle.first));
    assert!(!found.search.gross.is_gain());
    assert_eq!(found.search.gross.name(), "loss");
}

/// A break-even round trip is the third answer, distinct from both a gain and a
/// loss: the triangle at a dust-sized window returns exactly what it spent, and
/// `Gross::Even` is what says.
#[test]
fn a_route_that_breaks_even_is_reported_as_even() {
    let route = triangle_route();
    // Inputs 1 ..= 31 of this route: the ones below 21 lose a unit to the floor,
    // the ones from 21 up come back whole, and the gain only appears past 31.
    let found =
        optimize(&route, u(1), u(31), OptimizationPolicy::default()).expect("search a dust window");
    assert_eq!(found.search.gross, Gross::Even, "{:?}", found.search.gross);
    assert_eq!(found.search.gross.name(), "even");
    assert_eq!(found.search.best_profit, U256::ZERO);
    assert_eq!(
        found.search.best_input,
        u(21),
        "the smallest break-even input"
    );
    assert_eq!(found.search.best_output, found.search.best_input);
    assert_eq!(
        found.search.evaluations, 31,
        "and it was found by comparing every input, not by guessing"
    );
}

// ---------------------------------------------------------------------------
// §15: the candidate space is finite, and sampling never overstates
// ---------------------------------------------------------------------------

/// A domain wider than the exhaustive limit is sampled, and two things must
/// hold: the number of points priced is bounded by the policy, and the profit
/// reported is never more than a full scan of the same domain found. That second
/// clause is §13's warning turned into an assertion — the sampled peak may sit
/// *below* the true one, which is why this module does not claim a maximum.
#[test]
fn a_wide_domain_is_sampled_and_never_overstates() {
    let route = triangle_route();
    let policy = OptimizationPolicy {
        coarse_points: 256,
        refine_span: 64,
        exhaustive_limit: 4_096,
    };
    let ceiling = route.input_upper_bound().expect("a domain");
    let found = optimize(&route, u(1), ceiling, policy).expect("sample the whole route");
    assert_eq!(
        found.search.strategy,
        OptimizationStrategy::CoarseThenRefine
    );
    assert_eq!(found.search.termination, Termination::WindowRefined);
    let budget = policy.coarse_points + 2 * policy.refine_span + 1;
    assert!(
        found.search.evaluations <= budget,
        "{} points priced, budget {budget}",
        found.search.evaluations
    );
    let oracle = scan(&TRIANGLE_HOPS, 1, ceiling.to::<u128>());
    let sampled = profit_of(
        found.search.best_input,
        found.search.best_output,
        found.search.gross,
    );
    assert!(
        sampled <= oracle.profit,
        "the search reported {sampled} where the domain's best is {}: a sampled number may not exceed a scanned one",
        oracle.profit
    );
    assert!(
        sampled < oracle.profit,
        "this fixture is expected to show the gap §13 warns about; the scan's peak is at {} and the search's at {}",
        oracle.first,
        found.search.best_input
    );
    assert!(
        !found.search.covered_the_route_domain(&route),
        "a sampled answer is never a statement about the whole domain"
    );
}

/// The caps in [`OptimizationPolicy`] are the bound §15 promises, so a caller
/// that asks for a million grid points gets the cap, and the result echoes what
/// actually ran rather than what was requested.
#[test]
fn a_policy_outside_its_caps_is_clamped_and_reported() {
    let route = triangle_route();
    let greedy = OptimizationPolicy {
        coarse_points: u64::MAX,
        refine_span: u64::MAX,
        exhaustive_limit: u64::MAX,
    };
    let found = optimize(&route, u(1), u(100_000), greedy).expect("search with a wild policy");
    assert_eq!(
        found.search.policy.coarse_points,
        OptimizationPolicy::MAX_COARSE_POINTS
    );
    assert_eq!(
        found.search.policy.refine_span,
        OptimizationPolicy::MAX_REFINE_SPAN
    );
    assert_eq!(
        found.search.policy.exhaustive_limit,
        OptimizationPolicy::MAX_EXHAUSTIVE_WIDTH
    );
    let bound = found.search.policy.coarse_points + 2 * found.search.policy.refine_span + 1;
    assert!(found.search.evaluations <= bound);

    // A degenerate policy is still a search, not a division by zero.
    let degenerate = OptimizationPolicy {
        coarse_points: 0,
        refine_span: 0,
        exhaustive_limit: 0,
    };
    let one_point = optimize(&route, u(1), u(50_000), degenerate).expect("search with no knobs");
    assert_eq!(one_point.search.policy.coarse_points, 1, "one grid point");
    assert_eq!(
        one_point.search.evaluations, 2,
        "the grid point and its own window"
    );
    assert_eq!(
        one_point.search.best_input,
        u(1),
        "which is the domain floor"
    );

    // A domain inside the clamped limit is still walked exactly.
    let walked = optimize(
        &route,
        u(1),
        u(500),
        OptimizationPolicy {
            coarse_points: 1,
            refine_span: 0,
            exhaustive_limit: u64::MAX,
        },
    )
    .expect("walk a small domain");
    assert_eq!(walked.search.strategy, OptimizationStrategy::Exhaustive);
    assert_eq!(walked.search.evaluations, 500);
}

// ---------------------------------------------------------------------------
// §10 and §45's large U256 case
// ---------------------------------------------------------------------------

/// A market a thousand times deeper than the tests above puts the domain beyond
/// `u64` and beyond `u128`'s comfort, and the search still runs in `U256`: the
/// answer it reports is an amount no smaller integer type can hold, priced
/// through the same route.
#[test]
fn a_deep_market_is_searched_in_u256_all_the_way() {
    let route = deep_triangle_route();
    let far = 100_000_000_000_000_000_000u128; // 1e20, wider than u64::MAX
    let found = optimize(&route, u(1), u(far), OptimizationPolicy::default())
        .expect("search the deep market");
    assert_eq!(
        found.search.domain_max,
        u(far),
        "the route's own ceiling is far above the caller's window"
    );
    assert_eq!(
        found.search.strategy,
        OptimizationStrategy::CoarseThenRefine
    );
    assert_eq!(
        u64::try_from(found.search.best_input).err().map(|_| ()),
        Some(()),
        "the answer is {:#x}, which does not fit a u64 — the search cannot have widened to one",
        found.search.best_input
    );
    assert_eq!(
        found.search.refusals, 0,
        "1e30 reserves × 1e20 input fits 256 bits"
    );
    assert!(found.search.gross.is_gain(), "{:?}", found.search.gross);
    // The quote carried out of the search is the quote the route gives at that
    // input, recomputed here in a second call.
    let again = evm_opportunity::price(&route, found.search.best_input);
    let quoted = again.quote().cloned().expect("priceable");
    assert_eq!(quoted, found.search.quote);
}

// ---------------------------------------------------------------------------
// §12: determinism, §17: the route is not touched, §18: the name
// ---------------------------------------------------------------------------

/// Same route, same window, same policy, same answer — twice in a row and from
/// a rebuilt market. A search that drifted could not be replayed for evidence
/// (§12), and a search that depended on the snapshot object rather than its
/// contents would drift.
#[test]
fn the_search_is_reproducible() {
    let route = triangle_route();
    let policy = OptimizationPolicy::default_search();
    let first = optimize(&route, u(1), u(1_000), policy).expect("first run");
    let second = optimize(&route, u(1), u(1_000), policy).expect("second run");
    assert_eq!(first, second, "one route, two runs");
    let rebuilt = optimize(&triangle_route(), u(1), u(1_000), policy).expect("third run");
    assert_eq!(
        first.search, rebuilt.search,
        "a rebuilt market is the same market"
    );
    assert_eq!(first.search.evaluations, second.search.evaluations);
}

/// §17: the route is an input to the search, not a scratch pad. Nothing the
/// optimizer does may change the route it searched — the identity, the block and
/// the reserves all have to be what the caller handed over.
#[test]
fn searching_a_route_does_not_change_it() {
    let route = square_route();
    let before = route.clone();
    let found =
        optimize(&route, u(1), u(1_000), OptimizationPolicy::default()).expect("search the square");
    assert_eq!(route, before, "the route is untouched by being searched");
    assert_eq!(
        found.route, before,
        "and the candidate carries that same route"
    );
    assert_eq!(
        found.search.policy,
        OptimizationPolicy::default_search().clamped()
    );
    assert_eq!(
        found.route.target_block(),
        before.target_block(),
        "the block the reserves were read at survives the search"
    );
}

/// §18: the output is a *candidate*. It carries the route and the analytical
/// quote, and nothing else — no simulation, no gas, no risk verdict — which is
/// why it is not called an opportunity and cannot be mistaken for one.
#[test]
fn a_candidate_carries_the_route_and_the_quote_and_nothing_else() {
    let route = triangle_route();
    let found = optimize(&route, u(1), u(1_000), OptimizationPolicy::default())
        .expect("search the triangle");
    assert_eq!(found.chain_id, CHAIN);
    assert_eq!(found.target_block, BlockNumber(support::BLOCK));
    assert_eq!(found.hop_count(), 3);
    assert_eq!(found.search.quote.input, found.search.best_input);
    assert_eq!(found.search.quote.output, found.search.best_output);
    assert_eq!(found.gross(), found.search.gross);
    // The legs chain: what one hop paid out is what the next spent (§21's rule,
    // true of the analytical quote before any EVM sees it).
    assert_eq!(found.search.quote.hops.len(), 3);
    for index in 1..found.search.quote.hops.len() {
        assert_eq!(
            found.search.quote.hops[index].amount_in,
            found.search.quote.hops[index - 1].amount_out,
            "hop {index} spends exactly what hop {} produced",
            index - 1
        );
    }
    // The result is the route priced at the answer — not a third number.
    let at_answer = evm_opportunity::price(&found.route, found.search.best_input);
    match at_answer {
        Price::Quoted { quote, gross } => {
            assert_eq!(quote, found.search.quote);
            assert_eq!(gross, found.search.gross);
        }
        other => panic!("the answer should price, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// A domain with no input in it is an error with both bounds in it, never a
/// result of zero. Three ways to get there, all refused.
#[test]
fn an_empty_domain_is_a_refusal_not_a_zero() {
    let route = triangle_route();
    let inverted = optimize(&route, u(500), u(400), OptimizationPolicy::default())
        .expect_err("an inverted window is refused");
    assert_eq!(
        inverted,
        OpportunityError::EmptySearchDomain {
            lower: u(500),
            upper: u(400)
        }
    );

    let all_zero = optimize(
        &route,
        U256::ZERO,
        U256::ZERO,
        OptimizationPolicy::default(),
    )
    .expect_err("a window of nothing is refused");
    assert!(
        matches!(all_zero, OpportunityError::EmptySearchDomain { .. }),
        "{all_zero:?}"
    );

    // A route whose exit pool holds one unit has no domain at all.
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(P5, B, A, 1_000_000, 1, Some(FEE)),
    ]);
    let narrow = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P5, B, A)]).expect("closes");
    assert_eq!(narrow.input_upper_bound(), None);
    assert_eq!(
        optimize(&narrow, u(1), u(1_000), OptimizationPolicy::default()).err(),
        Some(OpportunityError::EmptySearchDomain {
            lower: u(1),
            upper: u(1_000)
        })
    );
}

/// A missing fee is not a fee of zero, and a search cannot sample its way past
/// it: the number is the same at every input, so refusing the search is refusing
/// the market, by name.
#[test]
fn an_unattested_fee_stops_the_search_and_names_the_pool() {
    let route = unattested_route();
    let error = optimize(&route, u(1), u(1_000), OptimizationPolicy::default())
        .expect_err("the search is refused");
    assert_eq!(
        error,
        OpportunityError::UnattestedFee(pool(P2), token(B)),
        "the second hop's pool is the one nobody proved a fee for"
    );
}

/// The route type and the search type refuse for their own reasons, and a
/// refusal from the route never arrives dressed as a search error.
#[test]
fn a_trail_that_is_not_a_route_never_reaches_the_search() {
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    let error = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C)]).err();
    assert_eq!(
        error,
        Some(RouteError::NotACycle {
            start: token(A),
            ended_on: token(C)
        })
    );
}

/// §48: the arithmetic refusal is counted, not swallowed. A domain whose far end
/// leaves 256 bits has points that cannot be priced; the search keeps the ones
/// that can and reports how many it had to drop.
#[test]
fn a_domain_that_leaves_256_bits_reports_its_refusals() {
    let top = u128::MAX;
    let snapshot = market(&[
        spec(P1, A, B, top, top, Some(FEE)),
        spec(P5, B, A, top, top / 2, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P5, B, A)])
        .expect("the deep pair closes");
    let ceiling = route.input_upper_bound().expect("a domain");
    let found = optimize(&route, u(1), ceiling, OptimizationPolicy::default())
        .expect("sample the widest pair");
    assert!(
        found.search.refusals > 0,
        "an input near 2^128 against 2^128 reserves must leave 256 bits somewhere"
    );
    assert!(
        found.search.evaluations > 0,
        "and yet the domain has a peak"
    );
    // Every priced point still agrees with a second call into the pricer.
    let again = evm_opportunity::price(&route, found.search.best_input);
    assert_eq!(again.quote().cloned(), Some(found.search.quote));
    // A window whose every point leaves 256 bits is the arithmetic's refusal, and
    // it keeps that name in both branches: the far end of this route's own domain
    // is large enough that a fee product against reserves of the same size does
    // not fit 256 bits, while the window itself is not empty.
    let sampled_only = OptimizationPolicy {
        coarse_points: 1,
        refine_span: 0,
        exhaustive_limit: 0,
    };
    for policy in [OptimizationPolicy::default(), sampled_only] {
        assert_eq!(
            optimize(&route, ceiling, ceiling, policy).err(),
            Some(OpportunityError::Math(MathError::Overflow)),
            "one point, and it overflowed: {} coarse points",
            policy.coarse_points
        );
    }
    // Control: the same route at a small input prices cleanly, so the refusal
    // above is about the amount searched, not about the market.
    let shallow = optimize(&route, u(1), u(1_000), OptimizationPolicy::default())
        .expect("a small window in the deep market");
    assert_eq!(shallow.search.refusals, 0);
    assert_eq!(shallow.search.evaluations, 1_000);
}
