//! §7–§12 and the Pricing half of §45: a route of any hop count, priced hop by
//! hop in exact integers.
//!
//! Three claims this file exists to check, in the order the task makes them:
//!
//! 1. **The route refuses shapes that are not routes** (§7), and each refusal
//!    names the hop it tripped on rather than pointing at the whole trail.
//! 2. **The pricing is the AMM's own arithmetic and nothing else** (§9): the same
//!    formula [`support::quote`] implements in `u128`, folded over the hops. The
//!    oracle here is a second implementation written from the formula, not a
//!    second call into the crate — a quote that checks itself would be a receipt.
//! 3. **A fee nobody proved is not a fee of zero** (§9): the route answers
//!    [`Price::Incomplete`] naming the pool, and prices nothing.
//!
//! Two refusals are declared one layer upstream and are tested at the door that
//! is reachable: a zero reserve is rejected by the state store, so it never
//! becomes an edge, and a token traded against itself never becomes an edge
//! either. `a_zero_reserve_is_refused_before_a_route_can_read_it` names the
//! state-layer error and then shows what the route does with the edge that was
//! never made; the arithmetic of an empty reserve is tested in
//! `crates/opportunity/src/math.rs`, where a reserve can be handed in directly.

mod support;

use alloy_primitives::{address, Address, U256};

use evm_core::{BlockNumber, Fee, LogIndex, PoolMeta, PoolType, ProtocolId};
use evm_graph::{EdgeId, GraphSnapshot};
use evm_opportunity::{price, Gross, MultiHopRoute, Price, RouteError};
use evm_pathfinder::{find_cycles, PathFinderConfig};
use evm_state::{InMemoryStateStore, StateError, StateStore, StateUpdate, UpdatePosition};

use support::{market, market_at, pool, spec, token, FEE};
use support::{A, B, C, CHAIN, P1, P2, P3, P4, P5};

const D: Address = address!("0x000000000000000000000000000000000000000d");

fn edge(p: Address, from: Address, to: Address) -> EdgeId {
    EdgeId::new(pool(p), token(from), token(to))
}

fn u(x: u128) -> U256 {
    U256::from(x)
}

/// The three-pool triangle every multi-hop test here trades round: `A -> B -> C
/// -> A`, one pool per pair, all fees attested.
///
/// The first pool pays out 1 100 000 of B against 1 000 000 of A, so the route's
/// price product is 1.1 against a fee product of `0.997³ ≈ 0.991` — it gains at
/// small inputs and stops gaining as slippage eats the gap, which is the shape a
/// search needs to find.
///
/// The reserves are deliberately six orders of magnitude above the inputs the
/// tests use. At a thousand-unit market an input of a hundred units *is* the
/// trade, so slippage swamps a ten percent price gap and the route loses at every
/// size; a profit has to be small against the pool to exist at all, which is the
/// same reason the real findings M3 priced sit at a fraction of a pool's depth.
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

/// The four-pool square: `A -> B -> C -> D -> A`. §45 asks for four-hop pricing,
/// and M9.3's search deliberately does not walk that far — so this route comes in
/// through the other door, the edge identities themselves.
fn square() -> GraphSnapshot {
    market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, D, 1_000_000, 1_000_000, Some(FEE)),
        spec(P4, D, A, 1_000_000, 1_000_000, Some(FEE)),
    ])
}

fn square_route() -> MultiHopRoute {
    MultiHopRoute::new(
        &square(),
        &[
            edge(P1, A, B),
            edge(P2, B, C),
            edge(P3, C, D),
            edge(P4, D, A),
        ],
    )
    .expect("the square closes")
}

/// The two-pool round trip — M3's shape, through M11's type. Half the depth
/// going out, two and a half times coming back, so the trip is worth 1.25 before
/// fees and 1.2425 after them.
fn pair() -> GraphSnapshot {
    market(&[
        spec(P1, A, B, 1_000_000, 500_000, Some(FEE)),
        spec(P2, B, A, 500_000, 1_250_000, Some(FEE)),
    ])
}

fn pair_route() -> MultiHopRoute {
    MultiHopRoute::new(&pair(), &[edge(P1, A, B), edge(P2, B, A)]).expect("the pair closes")
}

// ---------------------------------------------------------------------------
// The independent oracle, for any number of hops. `u128`, from §9's formula,
// with no reference to anything in the crate under test.
// ---------------------------------------------------------------------------

/// Fold a route's `(reserve_in, reserve_out)` pairs: each hop spends what the
/// previous one produced, whole.
fn round_trip(hops: &[(u128, u128)], x: u128) -> u128 {
    round_trip_at(hops, x, FEE)
}

fn round_trip_at(hops: &[(u128, u128)], x: u128, fee: Fee) -> u128 {
    let mut amount = x;
    for (reserve_in, reserve_out) in hops {
        amount = support::quote_at(*reserve_in, *reserve_out, amount, fee);
        if amount == 0 {
            return 0;
        }
    }
    amount
}

/// One hop as the two halves of its rational instead of a division, so a test
/// can point at the remainder the floor threw away rather than trust a second
/// call into the same helper. `out = (x * n * Ro) / (Ri * d + x * n)`, at the
/// route's attested fee.
fn exact_hop(reserves: (u128, u128), amount_in: u128) -> (u128, u128) {
    let retained = amount_in * u128::from(FEE.numerator);
    (
        retained * reserves.1,
        reserves.0 * u128::from(FEE.denominator) + retained,
    )
}

const PAIR_HOPS: [(u128, u128); 2] = [(1_000_000, 500_000), (500_000, 1_250_000)];
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

// ---------------------------------------------------------------------------
// §7: the route's own rules
// ---------------------------------------------------------------------------

/// A trail that is not a closed trail of distinct pools is refused, and each
/// refusal names the hop it tripped on rather than saying "invalid".
#[test]
fn a_trail_that_is_not_a_route_is_refused_by_name() {
    let cases: Vec<(&str, Vec<support::Spec>, Vec<EdgeId>, RouteError)> = vec![
        (
            "one hop is a token traded against itself",
            vec![
                spec(P1, A, B, 1_000, 1_100, Some(FEE)),
                spec(P2, B, C, 1_000, 1_000, Some(FEE)),
            ],
            vec![edge(P1, A, B)],
            RouteError::TooFewHops {
                found: 1,
                minimum: 2,
            },
        ),
        (
            "a trail that does not return to its input token",
            vec![
                spec(P1, A, B, 1_000, 1_100, Some(FEE)),
                spec(P2, B, C, 1_000, 1_000, Some(FEE)),
            ],
            vec![edge(P1, A, B), edge(P2, B, C)],
            RouteError::NotACycle {
                start: token(A),
                ended_on: token(C),
            },
        ),
        (
            "a round trip through one pool is not cross-market",
            vec![spec(P1, A, B, 1_000, 1_100, Some(FEE))],
            vec![edge(P1, A, B), edge(P1, B, A)],
            RouteError::RepeatedPool {
                index: 1,
                pool: pool(P1),
            },
        ),
        (
            "a hop that buys what the previous hop did not pay out",
            vec![
                spec(P1, A, B, 1_000, 1_100, Some(FEE)),
                spec(P2, B, C, 1_000, 1_000, Some(FEE)),
                spec(P3, C, A, 1_000, 1_000, Some(FEE)),
            ],
            vec![edge(P1, A, B), edge(P3, C, A)],
            RouteError::BrokenContinuity {
                index: 1,
                expected: token(B),
                found: token(C),
            },
        ),
        (
            "a token paid out twice before the route closes",
            vec![
                spec(P1, A, B, 1_000, 1_100, Some(FEE)),
                spec(P2, B, C, 1_000, 1_000, Some(FEE)),
                spec(P5, C, B, 1_000, 1_000, Some(FEE)),
            ],
            vec![edge(P1, A, B), edge(P2, B, C), edge(P5, C, B)],
            RouteError::RepeatedToken {
                index: 2,
                token: token(B),
            },
        ),
        (
            "an edge this snapshot does not carry",
            vec![spec(P1, A, B, 1_000, 1_100, Some(FEE))],
            vec![edge(P1, A, B), edge(P4, B, A)],
            RouteError::EdgeNotInGraph(edge(P4, B, A)),
        ),
    ];
    for (what, pools, edges, expected) in cases {
        let snapshot = market(&pools);
        let error = MultiHopRoute::new(&snapshot, &edges)
            .err()
            .unwrap_or_else(|| panic!("{what}: the route was accepted"));
        assert_eq!(error, expected, "{what}");
        // §49: a refusal is a value, and stating it out loud must not be empty.
        assert!(!error.to_string().is_empty(), "{what}: an empty refusal");
    }
}

/// A route that comes back to its input token before the last hop is a closed
/// trail plus a tail: the shorter cycle is the finding, so the longer spelling is
/// refused rather than priced.
#[test]
fn a_closed_trail_with_a_tail_is_refused() {
    let snapshot = market(&[
        spec(P1, A, B, 1_000, 1_100, Some(FEE)),
        spec(P2, B, C, 1_000, 1_000, Some(FEE)),
        spec(P3, C, A, 1_000, 1_000, Some(FEE)),
        // A second token, so the route can leave the start again after closing.
        spec(P4, A, D, 1_000, 1_000, Some(FEE)),
    ]);
    let error = MultiHopRoute::new(
        &snapshot,
        &[
            edge(P1, A, B),
            edge(P2, B, C),
            edge(P3, C, A),
            edge(P4, A, D),
        ],
    )
    .expect_err("the route closes early");
    assert_eq!(
        error,
        RouteError::ClosedEarly {
            start: token(A),
            hops: 4
        }
    );
    assert!(
        error.to_string().contains("closed trail plus a tail"),
        "{error}"
    );
}

/// `A -> B -> C -> A` entered from `B` is one route, not three findings: the
/// identity is the smallest rotation, while the trade order stays the caller's,
/// because pricing has to follow the direction the reserves were oriented in.
#[test]
fn three_seats_of_one_cycle_have_one_identity() {
    let graph = triangle();
    let from_a = MultiHopRoute::new(&graph, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("from A");
    let from_b = MultiHopRoute::new(&graph, &[edge(P2, B, C), edge(P3, C, A), edge(P1, A, B)])
        .expect("from B");
    let from_c = MultiHopRoute::new(&graph, &[edge(P3, C, A), edge(P1, A, B), edge(P2, B, C)])
        .expect("from C");
    assert_eq!(from_a.identity(), from_b.identity());
    assert_eq!(from_a.identity(), from_c.identity());
    assert_eq!(
        from_a.identity().hop_count(),
        3,
        "the identity keeps every hop"
    );
    assert_eq!(
        from_a.identity().start_token(),
        Some(from_a.edges()[0].token_in),
        "the identity's start is where its smallest rotation begins"
    );
    // …and the trade order is the caller's, unchanged.
    assert_eq!(from_a.input_token(), token(A));
    assert_eq!(from_b.input_token(), token(B));
    assert_eq!(from_b.edges()[0].pool, pool(P2));
    assert_eq!(from_a.edges()[0].pool, pool(P1));
}

/// §7's last clause: presentation order is not semantic identity *within* a
/// rotation, but the opposite direction is a different route — the reserves are
/// re-oriented, so the numbers differ.
#[test]
fn a_reversed_cycle_is_a_different_route_not_the_same_one() {
    let graph = triangle();
    let there = MultiHopRoute::new(&graph, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("forward");
    let back = MultiHopRoute::new(&graph, &[edge(P3, A, C), edge(P2, C, B), edge(P1, B, A)])
        .expect("reverse");
    assert_ne!(there.identity(), back.identity());
    let forward_price = price(&there, u(1_000));
    let reverse_price = price(&back, u(1_000));
    let forward = forward_price.quote().cloned().expect("forward quote");
    let reverse = reverse_price.quote().cloned().expect("reverse quote");
    assert_ne!(
        forward.output, reverse.output,
        "one direction buys 1 100 000 against 1 000 000 and the other does not"
    );
    assert!(
        matches!(forward_price.gross(), Some(Gross::Gain(_))),
        "the profitable direction is profitable"
    );
    assert!(
        matches!(reverse_price.gross(), Some(Gross::Loss(_))),
        "the other direction loses, and says so"
    );
}

/// The route carries the chain and the block its reserves belong to; §23 needs
/// that number to be part of the object rather than a caller's promise.
#[test]
fn the_route_carries_the_block_it_was_read_at() {
    let route = triangle_route();
    assert_eq!(route.chain_id(), CHAIN);
    assert_eq!(route.target_block().0, support::BLOCK);
    assert_eq!(route.hop_count(), 3);
    assert_eq!(route.pools(), vec![pool(P1), pool(P2), pool(P3)]);
    assert_eq!(
        route.tokens(),
        vec![token(A), token(B), token(C), token(A)],
        "the token list closes"
    );
}

// ---------------------------------------------------------------------------
// §9: the arithmetic
// ---------------------------------------------------------------------------

/// Two, three and four hops each agree with the independent `u128` fold hop by
/// hop and not only at the end — §11 asks for the intermediate amounts to be
/// kept, and agreement at the total alone would not prove the fold.
///
/// The sweep deliberately starts in the dust. An input of one or two units
/// cannot buy a whole unit of the next token, and the fold has to agree about
/// *that* too: the hop which pays out zero is kept, the hops behind it are not,
/// and the total is zero either way.
#[test]
fn quoting_agrees_with_the_oracle_at_every_hop() {
    for (name, route, hops) in [
        ("2 hop", pair_route(), &PAIR_HOPS[..]),
        ("3 hop", triangle_route(), &TRIANGLE_HOPS[..]),
        ("4 hop", square_route(), &SQUARE_HOPS[..]),
    ] {
        for x in [1u128, 2, 3, 10, 97, 1_000, 3_333, 25_000, 100_000] {
            let quote = price(&route, u(x))
                .quote()
                .cloned()
                .unwrap_or_else(|| panic!("{name} at {x}: refused"));
            assert_eq!(quote.input, u(x), "{name}");
            let mut amount = x;
            let mut ran = 0usize;
            for (index, (reserve_in, reserve_out)) in hops.iter().enumerate() {
                let expected = support::quote(*reserve_in, *reserve_out, amount);
                let hop = quote
                    .hops
                    .get(index)
                    .unwrap_or_else(|| panic!("{name} at {x}: hop {index} is missing"));
                assert_eq!(hop.amount_in, u(amount), "{name} hop {index} at {x}");
                assert_eq!(hop.amount_out, u(expected), "{name} hop {index} at {x}");
                assert_eq!(hop.fee, FEE, "{name} hop {index} carries its own fee");
                amount = expected;
                ran = index + 1;
                if expected == 0 {
                    break;
                }
            }
            assert_eq!(quote.hops.len(), ran, "{name} at {x}");
            assert_eq!(
                quote.truncated,
                ran < hops.len(),
                "{name} at {x}: {ran} of {} hops ran, and the flag must say so",
                hops.len()
            );
            assert_eq!(quote.output, u(amount), "{name} at {x}");
            assert_eq!(quote.output, u(round_trip(hops, x)), "{name} at {x}");
            assert_eq!(
                quote.amount_out_of_hop(quote.hops.len()),
                None,
                "{name}: the hop list is bounded"
            );
        }
    }
}

/// The four-hop route is the one M9.3 never hands over, so its shape has to come
/// with an explicit statement of what the type allows: hop count is not capped
/// here, and the cap that exists lives in the plan layer.
#[test]
fn a_four_hop_route_prices_and_reports_its_own_length() {
    let route = square_route();
    assert_eq!(route.hop_count(), 4);
    assert_eq!(MultiHopRoute::MIN_HOPS, 2);
    let quoted = price(&route, u(1_000)).quote().cloned().expect("priced");
    assert_eq!(quoted.hops.len(), 4);
    assert_eq!(quoted.hops[3].token_out, token(A), "the last hop closes");
    assert_eq!(quoted.fees(), vec![FEE, FEE, FEE, FEE]);
    assert_eq!(quoted.output, u(round_trip(&SQUARE_HOPS, 1_000)));
    assert!(
        quoted.output > u(1_000),
        "the square is built to gain at 1 000, got {}",
        quoted.output
    );
    assert!(
        matches!(Gross::between(quoted.input, quoted.output), Gross::Gain(_)),
        "and states it as a gain"
    );
}

/// Integer rounding is not decoration: the floored hop is what makes a route
/// gain at one input and lose at the next, and the quote has to show both.
#[test]
fn rounding_is_floor_at_every_hop_and_the_gain_disappears_as_the_input_grows() {
    let route = triangle_route();
    // Hop 1 at x = 1 is worth 1.096 of B: the floor takes it to 1, neither up to
    // 1.1 nor away to nothing.
    let one = price(&route, u(1)).quote().cloned().expect("one unit");
    assert_eq!(
        one.hops[0].amount_out,
        u(1),
        "the floor is taken, not rounded"
    );

    // Every hop of a real-sized trade lands between two whole units, so the
    // quoted number is the lower one and what is discarded is always less than
    // one unit of that hop's own token.
    let x = 1_000u128;
    let priced = price(&route, u(x));
    let quoted = priced.quote().cloned().expect("priced");
    let mut amount = x;
    let mut floor_at_hop_zero = 0u128;
    for (index, reserves) in TRIANGLE_HOPS.iter().enumerate() {
        let (numerator, denominator) = exact_hop(*reserves, amount);
        let floor = numerator / denominator;
        let hop = &quoted.hops[index];
        assert_eq!(hop.amount_out, u(floor), "hop {index}: the floor is taken");
        assert_ne!(
            numerator % denominator,
            0,
            "hop {index}: a whole hop would make the rounding claim vacuous"
        );
        if index == 0 {
            floor_at_hop_zero = floor;
        }
        amount = floor;
    }
    assert_eq!(
        floor_at_hop_zero, 1_095,
        "1 000 in, 1 095.6 of B out — the half unit is gone, and it is gone at every hop"
    );

    // Small against the pool it gains, larger it gains at a worse rate, and by
    // the domain ceiling the same route loses: slippage has eaten the gap.
    let small = priced.gross().expect("a result at 1 000");
    let Gross::Gain(gain_at_thousand) = small else {
        panic!("at {x}: {small:?}");
    };
    let mid = price(&route, u(25_000)).gross().expect("a mid-sized input");
    assert!(mid.is_gain(), "at 25 000: {mid:?}");
    let Gross::Gain(gain_at_25_000) = mid else {
        panic!("at 25 000: {mid:?}");
    };
    assert!(
        gain_at_25_000 < gain_at_thousand * u(25),
        "25x the input must buy less than 25x the gain, or the curve is not the AMM's: {gain_at_thousand} vs {gain_at_25_000}"
    );
    let beyond = price(&route, u(100_000))
        .gross()
        .expect("a result at 100 000");
    assert!(matches!(beyond, Gross::Loss(_)), "at 100 000: {beyond:?}");
    let ceiling = route.input_upper_bound().expect("a domain");
    let top = price(&route, ceiling)
        .gross()
        .expect("a result at the ceiling");
    assert!(matches!(top, Gross::Loss(_)), "at {ceiling}: {top:?}");
    // Between 25 000 and 100 000 the sign changes, which is what a search is for
    // (§13+); nothing here claims to know where, only that both states exist.
}

/// A loss is not zero and zero is not a loss: `Gross` keeps three states apart
/// because `U256` cannot spell a negative, and §4 forbids writing a loss as the
/// number that already means "exactly even".
#[test]
fn a_loss_is_stated_as_a_loss_with_its_size() {
    let route = triangle_route();
    let ceiling = route.input_upper_bound().expect("a domain");
    let loss = price(&route, ceiling).gross().expect("priced");
    let Gross::Loss(size) = loss else {
        panic!("expected a loss, got {loss:?}");
    };
    assert!(size > U256::ZERO, "a loss of zero is not a loss");
    assert_eq!(loss.name(), "loss");
    assert_eq!(loss.gain(), None, "a loss pays nothing");
    assert!(!loss.is_gain());
    assert_eq!(Gross::Even.name(), "even");
    assert_eq!(Gross::Gain(u(5)).gain(), Some(u(5)));
    assert_eq!(Gross::Gain(u(5)).name(), "gain");
}

/// The three states of the arithmetic, at the level of the round trip rather
/// than the enum: a symmetric market is a strict `Loss` (fees take 0.6 % of the
/// trip), never an `Even` by accident, and a zero input is a third thing again.
#[test]
fn gain_even_and_loss_are_three_answers_and_zero_input_is_a_fourth() {
    assert_eq!(Gross::between(u(10), u(10)), Gross::Even);
    assert_eq!(Gross::between(u(10), u(12)), Gross::Gain(u(2)));
    assert_eq!(Gross::between(u(10), u(7)), Gross::Loss(u(3)));
    let snapshot = market(&[
        spec(P1, A, B, 1_000, 1_000, Some(FEE)),
        spec(P2, B, A, 1_000, 1_000, Some(FEE)),
    ]);
    let route =
        MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).expect("symmetric");
    let gross = price(&route, u(100)).gross().expect("priced");
    assert!(matches!(gross, Gross::Loss(_)), "{gross:?}");
    let zero = price(&route, U256::ZERO);
    assert_eq!(zero.state(), "unpriceable");
    assert!(zero.quote().is_none(), "a refusal quotes nothing");
    assert!(zero.gross().is_none(), "a refusal has no gross");
}

/// §45's zero input: quoting nothing buys nothing, and the answer is a named
/// refusal rather than a zero that could be read as a break-even route.
#[test]
fn a_zero_input_is_refused_rather_than_quoted_as_zero() {
    let quoted = price(&triangle_route(), U256::ZERO);
    assert_eq!(quoted.state(), "unpriceable");
    match quoted {
        Price::Unpriceable(error) => assert_eq!(
            error.to_string(),
            "an input amount of zero buys nothing",
            "the refusal says which"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A hop that buys less than one whole unit ends the round trip there. That is a
/// market outcome, not an error: the output is zero, and `truncated` says the
/// remaining hops never ran.
#[test]
fn a_hop_that_buys_nothing_ends_the_trip_and_says_so() {
    // A pool so shallow on the far side that the second hop cannot buy a unit.
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(FEE)),
        spec(P3, C, A, 1_000, 1_000, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("the shape still closes");
    let quoted = price(&route, u(1))
        .quote()
        .cloned()
        .expect("priced, not refused");
    assert_eq!(
        quoted.hops[0].amount_out,
        U256::ZERO,
        "the first hop is dust"
    );
    assert_eq!(quoted.output, U256::ZERO);
    assert!(quoted.truncated, "hops 2 and 3 never ran");
    assert_eq!(quoted.hops.len(), 1, "only the hops that ran are kept");
    assert!(
        matches!(Gross::between(quoted.input, quoted.output), Gross::Loss(_)),
        "dust is a loss, not a break-even"
    );
    assert_eq!(
        quoted.fees(),
        vec![FEE],
        "and the fee list is truncated too"
    );
}

/// §9's ban on a hard-coded 997/1000, checked by making two pools disagree: each
/// hop carries and uses its own pool's fee.
#[test]
fn each_hop_is_priced_at_its_own_pools_fee() {
    let cheap = Fee {
        numerator: 999,
        denominator: 1_000,
    };
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_100_000, Some(FEE)),
        spec(P2, B, C, 1_000_000, 1_000_000, Some(cheap)),
        spec(P3, C, A, 1_000_000, 1_000_000, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("mixed-fee triangle");
    assert_eq!(
        route.edges().iter().map(|h| h.fee).collect::<Vec<_>>(),
        vec![Some(FEE), Some(cheap), Some(FEE)],
        "the fees are per hop, not one ratio for the route"
    );
    let quote = price(&route, u(1_000)).quote().cloned().expect("priced");
    assert_eq!(
        quote.fees(),
        vec![FEE, cheap, FEE],
        "and the quote reports which fee produced each number"
    );
    let h1 = support::quote_at(1_000_000, 1_100_000, 1_000, FEE);
    let h2 = support::quote_at(1_000_000, 1_000_000, h1, cheap);
    let h3 = support::quote_at(1_000_000, 1_000_000, h2, FEE);
    assert_eq!(quote.hops[0].amount_out, u(h1));
    assert_eq!(
        quote.hops[1].amount_out,
        u(h2),
        "the 999/1000 pool paid at its own rate"
    );
    assert_eq!(quote.output, u(h3));
    assert_ne!(
        quote.output,
        u(round_trip(&TRIANGLE_HOPS, 1_000)),
        "all-997 pricing would be a different number, so this route is not being priced at one ratio"
    );
}

/// §45's missing fee, at the door where it is reachable: a snapshot can carry a
/// pool whose fee nobody proved. The route is a real shape; the pricing refuses
/// to invent the number and names the pool instead.
#[test]
fn an_unattested_fee_prices_nothing_and_names_the_pool() {
    let snapshot = market(&[
        spec(P1, A, B, 1_000, 1_100, Some(FEE)),
        spec(P2, B, C, 1_000, 1_000, None),
        spec(P3, C, A, 1_000, 1_000, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("the shape closes even with an unproved fee");
    assert!(!route.is_priceable());
    assert_eq!(route.unattested(), Some((pool(P2), token(B))));
    let quoted = price(&route, u(100));
    assert_eq!(quoted.state(), "incomplete");
    match quoted {
        Price::Incomplete {
            pool: missing,
            token_in,
            hop_index,
        } => {
            assert_eq!(missing, pool(P2));
            assert_eq!(token_in, token(B));
            assert_eq!(hop_index, 1, "the second hop is the one missing proof");
        }
        other => panic!("expected an incomplete answer, got {other:?}"),
    }
    assert!(quoted.gross().is_none(), "incomplete is not zero profit");
}

/// Fees are asked for in trade order, so a route with two unproved pools names
/// the first it cannot price rather than the last.
#[test]
fn the_first_unattested_hop_is_the_one_named() {
    let snapshot = market(&[
        spec(P1, A, B, 1_000, 1_100, None),
        spec(P2, B, C, 1_000, 1_000, None),
        spec(P3, C, A, 1_000, 1_000, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, C), edge(P3, C, A)])
        .expect("closes");
    assert_eq!(route.unattested(), Some((pool(P1), token(A))));
    assert_eq!(
        price(&route, u(100)),
        Price::Incomplete {
            pool: pool(P1),
            token_in: token(A),
            hop_index: 0,
        }
    );
}

/// §45's overflow boundary: the fee product of a route whose reserves are at the
/// top of the range does not fit in 256 bits, and the answer is a named
/// arithmetic refusal with no number written anywhere.
#[test]
fn an_amount_that_leaves_256_bits_is_refused_not_wrapped() {
    let top = u128::MAX;
    let snapshot = market(&[
        spec(P1, A, B, top, top, Some(FEE)),
        spec(P2, B, A, top, top, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).expect("closes");
    // A small input on the same route is merely a slightly worse price — proof
    // that the refusal below is about the product, not about the market.
    let small = price(&route, u(1_000))
        .quote()
        .cloned()
        .expect("priced at 1000");
    assert!(small.output > U256::ZERO);
    let quoted = price(&route, u(top / 2));
    assert_eq!(quoted.state(), "unpriceable", "{quoted:?}");
    match quoted {
        Price::Unpriceable(error) => assert_eq!(
            error.to_string(),
            "the intermediate product does not fit in 256 bits"
        ),
        other => panic!("expected an arithmetic refusal, got {other:?}"),
    }
}

/// A zero reserve is refused one layer below this crate, and saying which layer
/// is the test's whole job: the state store will not hold a pool with an empty
/// side, so no snapshot can carry an edge for it, and the only answer a route
/// can give is that the edge is not in the graph. Pricing an empty reserve here
/// would mean testing a market the pipeline cannot produce.
#[test]
fn a_zero_reserve_is_refused_before_a_route_can_read_it() {
    let mut store = InMemoryStateStore::new(CHAIN);
    store
        .apply(StateUpdate::PoolRegistered(PoolMeta {
            id: pool(P1),
            protocol: ProtocolId::new("test-v2"),
            token0: token(A),
            token1: token(B),
            fee: Some(FEE),
            pool_type: PoolType::ConstantProduct,
        }))
        .expect("register a fixture pool");
    let rejected = store.apply(StateUpdate::PoolSynced {
        pool: pool(P1),
        reserve0: U256::ZERO,
        reserve1: u(1_000),
        position: UpdatePosition::new(BlockNumber(support::BLOCK), LogIndex(1)),
    });
    assert!(
        matches!(rejected, Err(StateError::InvalidReserves { .. })),
        "the empty side is refused by name, got {rejected:?}"
    );

    // So the pool has no state, therefore no edge, therefore no route.
    let snapshot = market(&[spec(P2, B, A, 1_000, 1_000, Some(FEE))]);
    assert!(
        snapshot.edge(edge(P1, A, B)).is_none(),
        "a pool with no synced state projects no edge"
    );
    assert_eq!(
        MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).unwrap_err(),
        RouteError::EdgeNotInGraph(edge(P1, A, B))
    );
}

// ---------------------------------------------------------------------------
// §12's domain bound and §6's adapter
// ---------------------------------------------------------------------------

/// The derived ceiling generalises from M3's two hops to any count: what comes
/// out of the last pool is strictly less than that pool's reserve of the input
/// token, so an input at or above it is guaranteed to lose.
#[test]
fn the_domain_ceiling_is_the_exit_pools_reserve_less_one() {
    assert_eq!(pair_route().input_upper_bound(), Some(u(1_249_999)));
    assert_eq!(triangle_route().input_upper_bound(), Some(u(999_999)));
    assert_eq!(square_route().input_upper_bound(), Some(u(999_999)));
    // A route whose exit pool holds one unit has no domain at all.
    let snapshot = market(&[
        spec(P1, A, B, 1_000_000, 1_000_000, Some(FEE)),
        spec(P2, B, A, 1_000_000, 1, Some(FEE)),
    ]);
    let route = MultiHopRoute::new(&snapshot, &[edge(P1, A, B), edge(P2, B, A)]).expect("closes");
    assert_eq!(route.input_upper_bound(), None);
}

/// §6: a `CycleCandidate` is widened by nothing. The adapter reads its `edges`
/// and its chain/block, asks the candidate whether it came from this snapshot,
/// and leaves the candidate untouched.
#[test]
fn a_candidate_becomes_a_route_without_changing_its_meaning() {
    let graph = triangle();
    let cycles = find_cycles(&graph, &PathFinderConfig::default()).expect("search the triangle");
    assert!(
        !cycles.is_empty(),
        "the triangle contains cycles for M9.3 to name"
    );
    for candidate in &cycles {
        let before = candidate.clone();
        let route = MultiHopRoute::from_candidate(&graph, candidate)
            .unwrap_or_else(|error| panic!("{} hops: {error}", candidate.hop_count));
        assert_eq!(
            candidate, &before,
            "the adapter did not touch the candidate"
        );
        assert_eq!(route.hop_count(), candidate.edges.len());
        assert_eq!(route.chain_id(), candidate.chain_id);
        assert_eq!(route.target_block(), candidate.target_block);
        assert_eq!(route.input_token(), candidate.start_token);
        // The identity rule is M9.3's, restated on the data this crate holds.
        assert_eq!(
            route.identity().edges(),
            candidate.canonical_key.edges(),
            "the route's identity is the candidate's canonical key"
        );
    }
}

/// A candidate from one block must not be priced against another block's
/// reserves; §6 makes that a refusal carrying both numbers.
#[test]
fn a_candidate_is_not_priced_against_another_blocks_reserves() {
    let found = find_cycles(&triangle(), &PathFinderConfig::default()).expect("search");
    let candidate = found.first().expect("the triangle has a cycle");
    let later = market_at(
        support::BLOCK + 1,
        &[
            spec(P1, A, B, 1_000, 1_100, Some(FEE)),
            spec(P2, B, C, 1_000, 1_000, Some(FEE)),
            spec(P3, C, A, 1_000, 1_000, Some(FEE)),
        ],
    );
    assert_eq!(later.block_number().0, support::BLOCK + 1);
    let error = MultiHopRoute::from_candidate(&later, candidate).unwrap_err();
    assert_eq!(
        error,
        RouteError::CandidateNotInGraph {
            chain_id: CHAIN,
            target_block: candidate.target_block,
            hops: candidate.edges.len(),
        }
    );
    assert!(error.to_string().contains("different market"), "{error}");
}

/// The quote is a value: two calls on one route at one input agree, and the
/// route is unchanged by being priced — §8's separation of route and amount.
#[test]
fn pricing_a_route_does_not_change_it() {
    let route = triangle_route();
    let before = route.clone();
    let first = price(&route, u(100));
    let second = price(&route, u(100));
    assert_eq!(first, second, "same route, same input, same answer");
    assert_eq!(route, before, "quoting left the route alone");
    assert_eq!(route.input_upper_bound(), before.input_upper_bound());
}
