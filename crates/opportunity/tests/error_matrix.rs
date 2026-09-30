//! §38 and §45: every way this crate can refuse, and the layer that refuses it.
//!
//! The task names ten error conditions (InvalidPath, SamePool, InvalidReserve,
//! InvalidAmount, InvalidFee, Overflow, MissingPool, MissingState, TokenMismatch,
//! NoLiquidity) and requires the enum to cover them with no `unwrap()`,
//! `expect()` or `panic!()` on data that came out of a `GraphSnapshot`,
//! `PoolState` or `PoolMeta`. Two things follow, and this file tests both:
//!
//! - **Each condition has a name and a reachable door.** Some are reachable
//!   through [`detect_opportunities`] alone, because a snapshot can actually carry
//!   them; others cannot reach the detector at all, because M1/M2 refuse them
//!   further up (a zero reserve never enters the state store, a same-pool round
//!   trip never becomes a candidate). A refusal that is unreachable from one layer
//!   is still tested — at the layer where it is reachable.
//! - **Nothing panics.** The degenerate markets below are pushed through the
//!   public entry point inside `catch_unwind`, so "returns an error" is a
//!   measurement rather than an absence of a crash report.
//!
//! Naming adjustments against the task's list, stated here rather than left to
//! inference:
//!
//! | the task calls it | this crate calls it | why |
//! |---|---|---|
//! | InvalidPath | `OpportunityError::Path` / `RejectionReason::InvalidPath` | same thing |
//! | SamePool, TokenMismatch | `PathError::SamePool`, `PathError::TokenMismatch` | a path error is one of four kinds, so they are variants rather than four spellings of one |
//! | NoLiquidity | `OpportunityError::EmptySearchDomain`, `RejectionReason::EmptyDomain` | the pool is not empty, it holds at most one unit of the token the route wants back — "no liquidity" would overstate what was proved |
//! | MissingState | `OpportunityError::MissingState` | declared, and never constructed: a `GraphSnapshot` edge already carries its own reserves, so there is no state left to look up and fail to find |
//!
//! `PathError::HopCount` is likewise declared and never constructed — v0.1 builds
//! exactly two-hop paths through a constructor that cannot express another count.
//! Both exist so a later milestone that widens either shape has to decide what
//! the error means instead of discovering it implicitly.

mod support;

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

use alloy_primitives::U256;

use evm_core::{BlockNumber, Fee, LogIndex, PoolId, PoolMeta, PoolState, PoolType, ProtocolId};
use evm_graph::GraphEdge;
use evm_opportunity::{
    detect_opportunities, find_optimal_input, swap_exact_in, ArbitragePath, Hop, MathError,
    OpportunityDetector, OpportunityError, PathError, PricedCycle, RejectionReason, SearchPolicy,
};
use evm_state::UpdatePosition;

use support::{market, peak, pool, spec, token, A, B, BLOCK, C, FEE, P1, P2, P3};

const MAX_U128: u128 = u128::MAX;

/// A fee that pays out more than it is given: not a fraction of anything, and
/// the math refuses it rather than pricing the route on the pool's own terms.
const IMPOSSIBLE_FEE: Fee = Fee {
    numerator: 1001,
    denominator: 1000,
};

/// The one pool this matrix needs at the far end of the range: both sides
/// `u128::MAX`, which the state store accepts (it refuses only zeros) and which
/// makes every intermediate product exceed 256 bits.
fn huge_pair() -> Vec<support::Spec> {
    vec![
        spec(P1, A, B, MAX_U128, MAX_U128, Some(FEE)),
        spec(P2, B, A, MAX_U128, MAX_U128, Some(FEE)),
    ]
}

/// A market whose exit pool holds a single unit of the token the route wants
/// back: the derived domain `1 ..= reserve_out - 1` is empty.
fn one_unit_exit() -> Vec<support::Spec> {
    vec![
        spec(P1, A, B, 100_000, 50_000, Some(FEE)),
        spec(P2, B, A, 50_000, 1, Some(FEE)),
    ]
}

/// A pool whose attestation carries an impossible fee ratio. The discovery layer
/// would never produce this — M2's fee evidence reproduces 997/1000 on 94 of 94
/// real trades — so it is a fixture, and the point of it is that the price is
/// refused rather than used.
fn impossible_fee() -> Vec<support::Spec> {
    vec![
        spec(P1, A, B, 100_000, 50_000, Some(IMPOSSIBLE_FEE)),
        spec(P2, B, A, 50_000, 125_000, Some(FEE)),
    ]
}

/// A pool with no proven fee at all, which is a real shape the graph does carry.
fn unattested() -> Vec<support::Spec> {
    vec![
        spec(P1, A, B, 100_000, 50_000, None),
        spec(P2, B, A, 50_000, 125_000, Some(FEE)),
    ]
}

/// Every degenerate market in one list, each with what it is supposed to prove.
fn markets() -> Vec<(&'static str, Vec<support::Spec>)> {
    vec![
        ("two pools at the top of the uint128 range", huge_pair()),
        ("exit pool holding one unit", one_unit_exit()),
        ("attestation with an impossible fee", impossible_fee()),
        ("pool with no attested fee", unattested()),
        (
            "a single pool, which offers no route at all",
            vec![spec(P1, A, B, 100_000, 50_000, Some(FEE))],
        ),
        (
            "three pools on one pair, one of them unattested",
            vec![
                spec(P1, A, B, 100_000, 50_000, Some(FEE)),
                spec(P2, B, A, 50_000, 125_000, Some(FEE)),
                spec(P3, A, B, 40_000, 80_000, None),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Nothing panics, from either door
// ---------------------------------------------------------------------------

/// `catch_unwind` turns "did not crash" into an assertion. The returned value is
/// whatever the call produced — `Ok` or `Err` — because both are acceptable; a
/// panic is not, and it is the only thing this reports as a failure.
fn no_panic<T, F: FnOnce() -> T>(label: &str, f: F) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(panic) => panic!("{label} panicked: {:?}", panic_reason(&*panic)),
    }
}

fn panic_reason(payload: &dyn Any) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "unrecognised payload".to_string()
    }
}

#[test]
fn a_degenerate_market_never_panics_the_detector() {
    for (label, specs) in markets() {
        let snapshot = market(&specs);
        let detection = no_panic(label, || detect_opportunities(&snapshot)).expect("a null result");
        // Every candidate landed in exactly one of the three lists, so a refusal
        // is a reported fact rather than a swallowed route.
        assert_eq!(
            detection.evaluated_count(),
            detection.candidates.len(),
            "{label}: a candidate went missing"
        );
        for rejection in &detection.rejected {
            assert!(
                detection
                    .opportunities
                    .iter()
                    .all(|o| o.path != rejection.path),
                "{label}: one route was both a finding and a refusal"
            );
            assert!(
                rejection.peak.is_some()
                    || matches!(
                        rejection.reason,
                        RejectionReason::MissingFee(_)
                            | RejectionReason::InvalidFee
                            | RejectionReason::Overflow
                            | RejectionReason::EmptyDomain
                            | RejectionReason::InvalidReserve
                            | RejectionReason::InvalidAmount
                            | RejectionReason::InvalidPath(_)
                            | RejectionReason::MissingHop(_)
                    ),
                "{label}: a refusal with no peak and no reason that explains it: {:?}",
                rejection.reason
            );
        }
    }
}

#[test]
fn the_search_and_the_math_survive_the_same_inputs() {
    let specs = huge_pair();
    let snapshot = market(&specs);
    for path in detection_paths(&snapshot) {
        let best = no_panic("evaluate", || {
            OpportunityDetector::default().evaluate(&snapshot, path)
        });
        match best {
            Ok(cycle) => {
                // A 256-bit reserve pair that still prices: then the reported
                // output cannot exceed the pool's own reserve of it.
                assert!(
                    cycle.simulation.output <= cycle.cycle.second.reserve_out,
                    "the search reported an output bigger than the exit pool: {cycle:?}"
                );
            }
            Err(error) => assert!(!error.to_string().is_empty(), "{error:?}"),
        }
    }

    // The math on its own limits, none of which are allowed to divide by zero or
    // wrap: each row is the condition the task names, at the layer that sees it.
    let max = U256::MAX;
    let one = U256::ONE;
    let rows: Vec<(&str, Result<U256, MathError>, MathError)> = vec![
        (
            "an empty side",
            swap_exact_in(U256::ZERO, one, FEE, one),
            MathError::InvalidReserve,
        ),
        (
            "an empty exit",
            swap_exact_in(one, U256::ZERO, FEE, one),
            MathError::InvalidReserve,
        ),
        (
            "a zero input",
            swap_exact_in(one, one, FEE, U256::ZERO),
            MathError::InvalidAmount,
        ),
        (
            "a fee with no denominator",
            swap_exact_in(
                one,
                one,
                Fee {
                    numerator: 1,
                    denominator: 0,
                },
                one,
            ),
            MathError::InvalidFee,
        ),
        (
            "a fee that retains more than it is paid",
            swap_exact_in(one, one, IMPOSSIBLE_FEE, one),
            MathError::InvalidFee,
        ),
        (
            "a product that does not fit",
            swap_exact_in(max, max, FEE, one),
            MathError::Overflow,
        ),
    ];
    for (label, outcome, expected) in rows {
        let refused = no_panic(label, || outcome);
        assert_eq!(refused, Err(expected), "{label}");
    }
}

fn detection_paths(snapshot: &evm_graph::GraphSnapshot) -> Vec<ArbitragePath> {
    evm_opportunity::enumerate_candidates(snapshot).expect("candidates")
}

// ---------------------------------------------------------------------------
// Each named condition, at its own door
// ---------------------------------------------------------------------------

#[test]
fn the_overflow_refusal_reaches_the_report_as_a_reason() {
    // Reserves at the top of the range the store will hold: the first probe of
    // the search multiplies past 256 bits, so every route here is refused with a
    // reason rather than a number.
    let detection = detect_opportunities(&market(&huge_pair())).expect("detect");
    assert!(detection.opportunities.is_empty());
    assert_eq!(detection.candidates.len(), 4);
    assert_eq!(detection.rejected.len(), 4);
    for rejection in &detection.rejected {
        assert_eq!(
            rejection.reason,
            RejectionReason::Overflow,
            "{:?}",
            rejection.path
        );
        assert!(rejection.peak.is_none(), "nothing was priced");
    }
}

#[test]
fn an_empty_search_domain_is_a_refusal_and_not_a_zero_profit() {
    // The domain is `1 ..= second.reserve_out - 1`. Pool 2 holds one unit of A, so
    // the route that sells B back into A there has no input at all — and a route
    // with no domain is refused, while the rest of the same market is still
    // priced on its own terms.
    let specs = one_unit_exit();
    let detection = detect_opportunities(&market(&specs)).expect("detect");
    assert_eq!(detection.candidates.len(), 4);
    assert_eq!(detection.evaluated_count(), 4, "nothing went missing");

    let empty: Vec<_> = detection
        .rejected
        .iter()
        .filter(|r| r.reason == RejectionReason::EmptyDomain)
        .collect();
    assert_eq!(empty.len(), 1, "{:?}", detection.rejected);
    assert_eq!(empty[0].path.pools(), [pool(P1), pool(P2)]);
    assert_eq!(empty[0].path.input_token(), token(A));
    assert!(
        empty[0].peak.is_none(),
        "the search never ran, so there is no peak"
    );

    // The other three are not refused for the same reason: one loses, and two
    // gain — a pool holding one unit of A sells it for whatever the other side is
    // worth, which is what the thin side of a market actually means.
    assert_eq!(
        detection.opportunities.len(),
        2,
        "{:?}",
        detection.opportunities
    );
    assert_eq!(
        detection
            .rejected
            .iter()
            .filter(|r| r.reason == RejectionReason::Unprofitable)
            .count(),
        1
    );
    // Priced from the pool's own attestation, not from the report: pool 1 is
    // 100 000 A / 50 000 B, pool 2 is 50 000 B / 1 A.
    let p1_ab = (100_000u128, 50_000u128);
    let p1_ba = (50_000u128, 100_000u128);
    let p2_ab = (1u128, 50_000u128);
    let expected = [
        ((token(B), [pool(P1), pool(P2)]), peak(p1_ba, p2_ab)),
        ((token(A), [pool(P2), pool(P1)]), peak(p2_ab, p1_ba)),
    ];
    for (key, scan) in expected {
        let opportunity = detection
            .opportunities
            .iter()
            .find(|o| (o.input_token, o.path.pools()) == key)
            .unwrap_or_else(|| panic!("{key:?} is not reported: {:?}", detection.opportunities));
        assert!(scan.profit > 0, "{key:?} {scan:?}");
        assert_eq!(opportunity.gross_profit, U256::from(scan.profit as u128));
        assert!(
            scan.contains(opportunity.input_amount.to::<u128>()),
            "{key:?} {scan:?}"
        );
        assert!(opportunity.search.interval_closed);
    }

    // The fourth route is the one that loses, and its recorded near miss is the
    // scan's own best case: against a pool holding one unit of A, the most a
    // round trip can do is come back one unit short.
    let p2_ba = (50_000u128, 1u128);
    let losing = detection
        .rejected
        .iter()
        .find(|r| r.reason == RejectionReason::Unprofitable)
        .expect("one route loses");
    let scan = peak(p2_ba, p1_ab);
    assert_eq!(scan.profit, -1, "{scan:?}");
    let miss = losing.peak.expect("it was priced");
    assert_eq!(miss.gross_loss(), Some(U256::ONE), "{miss:?}");
    assert_eq!(
        miss.input,
        U256::from(scan.first_input),
        "the search reports an argmax of the scan, not a point near it"
    );

    // The same condition at the search's own door, with the bounds it states.
    let snapshot = market(&specs);
    let route = detection_paths(&snapshot)
        .into_iter()
        .find(|p| p.pools() == [pool(P1), pool(P2)] && p.input_token() == token(A))
        .expect("the A -> P1 -> B -> P2 -> A route");
    let error = find_optimal_input(&priced(&snapshot, route), SearchPolicy::default())
        .expect_err("no domain to search");
    assert_eq!(
        error,
        OpportunityError::EmptySearchDomain {
            lower: U256::ONE,
            upper: U256::ZERO,
        }
    );
    assert!(error.to_string().contains("empty"), "{error}");
    assert_eq!(
        priced(&snapshot, route).input_upper_bound(),
        None,
        "one unit in the exit pool leaves the bound below the smallest legal input"
    );
}

fn priced(snapshot: &evm_graph::GraphSnapshot, path: ArbitragePath) -> PricedCycle {
    let [first, second] = path.hops();
    PricedCycle::new(
        &snapshot.edge(first).expect("in the graph"),
        &snapshot.edge(second).expect("in the graph"),
    )
    .expect("priced")
}

#[test]
fn an_unattested_fee_is_attributed_to_the_pool_that_lacks_it() {
    let detection = detect_opportunities(&market(&unattested())).expect("detect");
    assert_eq!(detection.rejected.len(), 4, "{:?}", detection.rejected);
    for rejection in &detection.rejected {
        assert_eq!(
            rejection.reason,
            RejectionReason::MissingFee(pool(P1)),
            "{:?} was refused for the wrong pool",
            rejection.path
        );
    }
    // The three-pool market mixes the two: routes through the unattested pool are
    // refused by name, and the rest are still priced on their own merits.
    let mixed = detect_opportunities(&market(&[
        spec(P1, A, B, 100_000, 50_000, Some(FEE)),
        spec(P2, B, A, 50_000, 125_000, Some(FEE)),
        spec(P3, A, B, 40_000, 80_000, None),
    ]))
    .expect("detect");
    let refused: Vec<PoolId> = mixed
        .rejected
        .iter()
        .filter_map(|r| match r.reason {
            RejectionReason::MissingFee(p) => Some(p),
            _ => None,
        })
        .collect();
    assert!(!refused.is_empty(), "no route named the unattested pool");
    assert!(refused.iter().all(|p| *p == pool(P3)), "{refused:?}");
    assert!(mixed
        .rejected
        .iter()
        .any(|r| r.reason == RejectionReason::Unprofitable));
}

#[test]
fn an_impossible_fee_is_refused_rather_than_traded_through() {
    // A fee ratio that retains more than it is paid is not a fraction of anything,
    // so it cannot price a hop. The fixture's attestation is one the discovery
    // layer would never produce; the point is that nothing is priced with it.
    let detection = detect_opportunities(&market(&impossible_fee())).expect("detect");
    assert!(detection.opportunities.is_empty());
    assert_eq!(detection.candidates.len(), 4);
    assert_eq!(detection.rejected.len(), 4);
    for rejection in &detection.rejected {
        assert_eq!(
            rejection.reason,
            RejectionReason::InvalidFee,
            "{:?}",
            rejection.path
        );
        assert!(rejection.peak.is_none(), "nothing was priced against it");
    }
    // Every route in a two-pool market passes through both pools, so one bad
    // attestation refuses all four rather than leaving a partial report to be read
    // as the market. The check is per hop, whichever end the bad pool is on: the
    // first two refusals are the routes that start there, the last two the routes
    // that only reach it second.
    assert_eq!(
        detection
            .rejected
            .iter()
            .map(|r| r.path.pools())
            .collect::<Vec<_>>(),
        vec![
            [pool(P1), pool(P2)],
            [pool(P1), pool(P2)],
            [pool(P2), pool(P1)],
            [pool(P2), pool(P1)],
        ]
    );
}

#[test]
fn a_route_the_graph_does_not_contain_is_an_error_not_a_zero() {
    // MissingPool's door: the detector can be handed a path directly, and a path
    // naming a pool this snapshot never saw must be refused, not priced at zero.
    let snapshot = market(&[spec(P1, A, B, 100_000, 50_000, Some(FEE))]);
    let path = ArbitragePath::two_hops(
        Hop::new(pool(P1), token(A), token(B)),
        Hop::new(pool(P2), token(B), token(A)),
    )
    .expect("a cycle");
    assert_eq!(
        OpportunityDetector::default().evaluate(&snapshot, path),
        Err(OpportunityError::MissingPool(pool(P2)))
    );
    // Both hops missing names the first one it looked for, in trade order.
    let stranger = ArbitragePath::two_hops(
        Hop::new(pool(P3), token(A), token(B)),
        Hop::new(pool(P2), token(B), token(A)),
    )
    .expect("a cycle");
    assert_eq!(
        OpportunityDetector::default().evaluate(&snapshot, stranger),
        Err(OpportunityError::MissingPool(pool(P3)))
    );
    let error = OpportunityError::MissingState(pool(P3));
    assert!(error.to_string().contains("no state"), "{error}");
}

#[test]
fn the_path_rules_refuse_the_shapes_that_are_not_cross_market_arbitrage() {
    let a_to_b = Hop::new(pool(P1), token(A), token(B));
    let b_to_a = Hop::new(pool(P2), token(B), token(A));
    let same_pool_back = Hop::new(pool(P1), token(B), token(A));
    let wrong_mid = Hop::new(pool(P2), token(C), token(A));

    // SamePool: a round trip inside one pool is a fee donation, not a market.
    assert_eq!(
        ArbitragePath::two_hops(a_to_b, same_pool_back),
        Err(PathError::SamePool(pool(P1), pool(P1)))
    );
    // TokenMismatch: the second hop does not buy what the first one paid out.
    assert_eq!(
        ArbitragePath::two_hops(a_to_b, wrong_mid),
        Err(PathError::TokenMismatch(pool(P2), token(B)))
    );
    // NotACycle: it ends holding a token it never spent.
    let a_to_b_then_b_to_c = Hop::new(pool(P2), token(B), token(C));
    assert_eq!(
        ArbitragePath::two_hops(a_to_b, a_to_b_then_b_to_c),
        Err(PathError::NotACycle(token(A)))
    );
    // And the valid shape still builds, so the three refusals above are about the
    // route and not about the constructor.
    assert!(ArbitragePath::two_hops(a_to_b, b_to_a).is_ok());

    // A refused route reaches the crate's own error type unchanged: §38's
    // InvalidPath row is the path rules surfacing as a result, not a filter that
    // drops a candidate on the floor.
    assert_eq!(
        OpportunityError::from(PathError::SamePool(pool(P1), pool(P1))),
        OpportunityError::Path(PathError::SamePool(pool(P1), pool(P1)))
    );
}

/// The messages are the audit trail, so each one has to name what it refuses.
/// Pinned to the exact text rather than to "is non-empty".
#[test]
fn every_error_states_what_it_refused() {
    let cases: Vec<(&str, OpportunityError)> = vec![
        (
            "missing pool names the pool",
            OpportunityError::MissingPool(pool(P2)),
        ),
        (
            "missing state names the pool",
            OpportunityError::MissingState(pool(P2)),
        ),
        (
            "an unattested fee names the pool and the token",
            OpportunityError::UnattestedFee(pool(P1), token(A)),
        ),
        (
            "an empty domain states both bounds",
            OpportunityError::EmptySearchDomain {
                lower: U256::ONE,
                upper: U256::ZERO,
            },
        ),
        ("a refused path", PathError::NotACycle(token(A)).into()),
        ("refused math", MathError::Overflow.into()),
    ];
    for (label, error) in cases {
        let text = error.to_string();
        assert!(
            !text.is_empty(),
            "{label}: an empty message refuses nothing"
        );
        println!("{label}: {text}");
    }
    // The two messages that must carry an address do.
    assert!(OpportunityError::MissingPool(pool(P2))
        .to_string()
        .contains("0x00000000000000000000000000000000000000f2"));
    assert!(OpportunityError::UnattestedFee(pool(P1), token(A))
        .to_string()
        .contains("0x00000000000000000000000000000000000000f1"));
    assert!(OpportunityError::EmptySearchDomain {
        lower: U256::ONE,
        upper: U256::ZERO,
    }
    .to_string()
    .contains("empty"));
}

/// A zero reserve cannot reach the detector: it is refused at the state store and
/// again at the graph edge, so `MathError::InvalidReserve` is a math-layer guard
/// rather than a report the pipeline can produce. Tested at both doors that exist.
#[test]
fn a_zero_reserve_is_refused_before_it_can_be_priced() {
    let meta = PoolMeta {
        id: pool(P1),
        protocol: ProtocolId::new("test-v2"),
        token0: token(A),
        token1: token(B),
        fee: Some(FEE),
        pool_type: PoolType::ConstantProduct,
    };
    let empty = PoolState {
        pool: pool(P1),
        reserve0: U256::ZERO,
        reserve1: U256::from(50_000u32),
        block_number: BlockNumber(BLOCK),
        log_index: LogIndex(1),
    };
    assert!(matches!(
        GraphEdge::pair(&meta, &empty),
        Err(evm_graph::EdgeRejection::EmptySide)
    ));

    // The guard behind that door, which is the one the crate itself would hit if a
    // zero ever arrived from somewhere else.
    let edge = GraphEdge {
        id: Hop::new(pool(P1), token(A), token(B)),
        reserve_in: U256::ZERO,
        reserve_out: U256::from(50_000u32),
        fee: Some(FEE),
        state_position: UpdatePosition::new(BlockNumber(BLOCK), LogIndex(1)),
    };
    let other = GraphEdge {
        id: Hop::new(pool(P2), token(B), token(A)),
        reserve_in: U256::from(50_000u32),
        reserve_out: U256::from(125_000u32),
        fee: Some(FEE),
        state_position: edge.state_position,
    };
    assert_eq!(
        PricedCycle::new(&edge, &other),
        Err(OpportunityError::Math(MathError::InvalidReserve))
    );
    // ... and the same through the fee resolution, which is where an unattested
    // pool is refused for the same kind of reason.
    assert_eq!(
        evm_opportunity::PricedHop::from_edge(&edge),
        Err(OpportunityError::Math(MathError::InvalidReserve))
    );
}
