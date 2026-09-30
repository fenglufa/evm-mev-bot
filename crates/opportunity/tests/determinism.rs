//! §31–§33 and §56: the same market has to give the same report, in the same
//! order, in the same bytes — and the order has to be a documented one rather
//! than whatever the collections happened to yield.
//!
//! Three separate claims, tested separately:
//!
//! - **Repeatability.** Detecting twice over one snapshot is one result, and a
//!   snapshot rebuilt from the same state is the same result again — including the
//!   exact input a plateau was reported at, not only its profit.
//! - **Order independence.** The task's §18 loop walks the graph from every node,
//!   so one route is reachable from several starting tokens. Which start found it
//!   first must not survive into the report — so the same market registered in a
//!   different order has to produce a byte-identical one.
//! - **A stated sort key.** Opportunities come out by gross profit descending,
//!   then by route identity; rejections by route identity then reason; skipped
//!   pairs by hop identity. The market here is deliberately a mirror — the {A,B}
//!   pair priced again on {A,C} with the same numbers — so two routes tie on
//!   profit exactly. Without that tie the identity tie-break would be assumed
//!   rather than exercised.
//!
//! Every expected profit is the exhaustive `u128` scan's (`support::peak`, fed
//! from the pools' own attestations), never the search's. The sort is therefore
//! asserted over measurements: if the search and the scan disagreed, this file
//! would fail on the profit before it ever got to the order.
//!
//! Serialization is checked on the parts (`opportunities`, `rejected`,
//! `skipped_pairs`), because `Detection` itself stays a debug view rather than a
//! wire format — §56 asks for stable output for what gets reported, not for a
//! `Serialize` impl on every type in the crate.

mod support;

use alloy_primitives::{Address, U256};

use evm_core::{BlockNumber, PoolId, TokenId};
use evm_opportunity::{
    detect_opportunities, enumerate_candidates, ArbitragePath, CandidateRejection, Detection,
    Opportunity, RejectionReason, SkippedPair,
};

use support::{market, peak, pool, side, spec, token, A, B, BLOCK, C, CHAIN, FEE, P1, P2, P3, P4};

/// Pool 1 asks 2 A per B; pool 2 bids 2.5 A per B. The pair that pays.
fn pool1() -> support::Spec {
    spec(P1, A, B, 100_000, 50_000, Some(FEE))
}

fn pool2() -> support::Spec {
    spec(P2, B, A, 50_000, 125_000, Some(FEE))
}

/// The same two pools with the second token renamed: identical numbers on {A,C}
/// as on {A,B} means an identical best profit, which means the report has a real
/// tie to break instead of a coincidence of addresses.
fn pool3() -> support::Spec {
    spec(P3, A, C, 100_000, 50_000, Some(FEE))
}

fn pool4() -> support::Spec {
    spec(P4, C, A, 50_000, 125_000, Some(FEE))
}

fn mirrored() -> Vec<support::Spec> {
    vec![pool1(), pool2(), pool3(), pool4()]
}

/// One pool's attestation, looked up by address. Used to feed the oracle the same
/// reserves the graph was built from without going through the graph.
fn attestation(addr: Address) -> support::Spec {
    [pool1(), pool2(), pool3(), pool4()]
        .into_iter()
        .find(|pool_spec| pool_spec.pool == addr)
        .expect("this market holds exactly these four pools")
}

/// One hop as the attestation states it: `(reserve_in, reserve_out)` for the
/// direction that enters `addr` holding `entering`.
fn hop(addr: Address, entering: Address) -> (u128, u128) {
    side(&attestation(addr), entering)
}

/// The exhaustive peak of the route that spends `input`, buys `mid` in `first`,
/// and sells it back in `second`.
fn scan(input: Address, mid: Address, first: Address, second: Address) -> i128 {
    peak(hop(first, input), hop(second, mid)).profit
}

/// Every directed route in this market with its peak, keyed the way a reader names
/// it: the token spent, then the pools in trade order.
fn peaks() -> Vec<((TokenId, [PoolId; 2]), i128)> {
    vec![
        ((token(A), [pool(P1), pool(P2)]), scan(A, B, P1, P2)),
        ((token(B), [pool(P1), pool(P2)]), scan(B, A, P1, P2)),
        ((token(A), [pool(P2), pool(P1)]), scan(A, B, P2, P1)),
        ((token(B), [pool(P2), pool(P1)]), scan(B, A, P2, P1)),
        ((token(A), [pool(P3), pool(P4)]), scan(A, C, P3, P4)),
        ((token(C), [pool(P3), pool(P4)]), scan(C, A, P3, P4)),
        ((token(A), [pool(P4), pool(P3)]), scan(A, C, P4, P3)),
        ((token(C), [pool(P4), pool(P3)]), scan(C, A, P4, P3)),
    ]
}

/// A route reduced to what a reader identifies it by: the token spent, then the
/// pools in trade order.
fn route(path: &ArbitragePath) -> (TokenId, [PoolId; 2]) {
    (path.input_token(), path.pools())
}

fn report(detection: &Detection) -> Vec<(u128, TokenId, [PoolId; 2])> {
    detection
        .opportunities
        .iter()
        .map(|o| (o.gross_profit.to::<u128>(), o.input_token, o.path.pools()))
        .collect()
}

/// The scan's verdict on one route, for the assertions that compare a finding with
/// the measurement rather than with another finding.
fn peak_of(table: &[((TokenId, [PoolId; 2]), i128)], key: &(TokenId, [PoolId; 2])) -> i128 {
    table
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("the scan never covered {key:?}"))
        .1
}

// ---------------------------------------------------------------------------
// Repeatability
// ---------------------------------------------------------------------------

#[test]
fn pricing_the_same_market_twice_is_one_report_not_two() {
    let snapshot = market(&mirrored());
    let first = detect_opportunities(&snapshot).expect("detect");
    let second = detect_opportunities(&snapshot).expect("detect");
    assert_eq!(first, second, "one snapshot, one report");
    // Equality reaches every field, including the search record: rounds,
    // evaluations and the exact input the peak was reported at. A plateau resolved
    // by whatever order the closing pass happened to walk would show up here as a
    // different `input_amount`.
    assert_eq!(first.opportunities, second.opportunities);
    assert_eq!(first.rejected, second.rejected);
    assert_eq!(first.skipped_pairs, second.skipped_pairs);
    assert_eq!(first.candidates, second.candidates);

    // A snapshot rebuilt from the same state is the same market.
    let rebuilt = detect_opportunities(&market(&mirrored())).expect("detect");
    assert_eq!(first, rebuilt, "rebuilding the state changes nothing");
    let inputs: Vec<U256> = first.opportunities.iter().map(|o| o.input_amount).collect();
    assert_eq!(
        inputs,
        rebuilt
            .opportunities
            .iter()
            .map(|o| o.input_amount)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        first
            .opportunities
            .iter()
            .map(|o| o.search.rounds)
            .collect::<Vec<_>>(),
        rebuilt
            .opportunities
            .iter()
            .map(|o| o.search.rounds)
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Registration order
// ---------------------------------------------------------------------------

#[test]
fn the_report_does_not_inherit_the_order_the_pools_were_registered_in() {
    let canonical = mirrored();
    let reversed = vec![pool4(), pool3(), pool2(), pool1()];
    let interleaved = vec![pool2(), pool4(), pool1(), pool3()];

    // The fixture numbers log positions by list index, so these three markets
    // differ in exactly the state the graph sees. The report must not see it.
    let mut reports = Vec::new();
    for specs in [&canonical, &reversed, &interleaved] {
        reports.push(detect_opportunities(&market(specs)).expect("detect"));
    }
    for (index, detection) in reports.iter().enumerate().skip(1) {
        assert_eq!(
            &reports[0], detection,
            "build {index} disagrees with the canonical one"
        );
    }
    let json: Vec<String> = reports
        .iter()
        .map(|d| {
            [
                serde_json::to_string(&d.opportunities).expect("serialize"),
                serde_json::to_string(&d.rejected).expect("serialize"),
                serde_json::to_string(&d.skipped_pairs).expect("serialize"),
            ]
            .join("|")
        })
        .collect();
    assert!(
        json.windows(2).all(|w| w[0] == w[1]),
        "the serialized report differs by build: {json:?}"
    );

    let candidates = enumerate_candidates(&market(&mirrored())).expect("candidates");
    assert_eq!(
        candidates,
        enumerate_candidates(&market(&reversed)).expect("candidates")
    );
    assert_eq!(
        candidates,
        enumerate_candidates(&market(&interleaved)).expect("candidates")
    );
    // And the enumeration really did deduplicate: one entry per route identity,
    // however many starting tokens reach it.
    let identities: std::collections::BTreeSet<_> =
        candidates.iter().map(|p| p.identity()).collect();
    assert_eq!(identities.len(), candidates.len());
}

// ---------------------------------------------------------------------------
// The documented sort key
// ---------------------------------------------------------------------------

#[test]
fn the_mirrored_market_really_does_tie_on_profit() {
    // Everything the ordering tests below claim rests on this measurement: two
    // routes pay 660 and two pay 293, and the four that go the expensive way round
    // lose. Read off the scan, not off the search.
    let table = peaks();
    let mut winners: Vec<i128> = table
        .iter()
        .filter(|(_, p)| *p > 0)
        .map(|(_, p)| *p)
        .collect();
    winners.sort_unstable();
    assert_eq!(winners, vec![293, 293, 660, 660], "{table:?}");
    let losers: Vec<i128> = table
        .iter()
        .filter(|(_, p)| *p <= 0)
        .map(|(_, p)| *p)
        .collect();
    assert_eq!(losers.len(), 4, "{table:?}");
    assert!(losers.iter().all(|p| *p == -1), "{losers:?}");
}

#[test]
fn opportunities_come_out_profit_first_and_route_identity_breaks_the_tie() {
    let detection = detect_opportunities(&market(&mirrored())).expect("detect");
    let table = peaks();
    // Two disjoint pairs, four directed routes per pair; the cheap way round pays
    // on both pairs, the expensive way round loses on both.
    assert_eq!(detection.candidates.len(), 8);
    assert_eq!(detection.opportunities.len(), 4);
    assert_eq!(detection.rejected.len(), 4);
    assert_eq!(detection.evaluated_count(), 8);

    // Each finding is the exhaustive maximum of its own route, so the order below
    // is an order over measured profits rather than over the search's claims.
    for opportunity in &detection.opportunities {
        let measured = peak_of(&table, &route(&opportunity.path));
        assert!(measured > 0, "{:?} is not a winner", opportunity.path);
        assert_eq!(
            opportunity.gross_profit,
            U256::from(measured as u128),
            "{:?} vs exhaustive {measured}",
            opportunity.path
        );
    }

    assert_eq!(
        report(&detection),
        [
            (660, token(A), [pool(P1), pool(P2)]),
            (660, token(A), [pool(P3), pool(P4)]),
            (293, token(B), [pool(P2), pool(P1)]),
            (293, token(C), [pool(P4), pool(P3)]),
        ],
        "profit descending, then route identity ascending"
    );

    // The tie is real, not asserted: without it this test would pass with a
    // comparator that ignored route identity entirely.
    let profits: Vec<U256> = detection
        .opportunities
        .iter()
        .map(|o| o.gross_profit)
        .collect();
    assert_eq!(profits[0], profits[1], "the two routes that spend A tie");
    assert_eq!(
        profits[2], profits[3],
        "and so do the two that spend the mid token"
    );
    assert!(profits[0] > profits[2], "the tie groups are distinct");
    for pair in profits.windows(2) {
        assert!(pair[0] >= pair[1], "descending: {profits:?}");
    }
    for pair in detection.opportunities.windows(2) {
        if pair[0].gross_profit == pair[1].gross_profit {
            assert!(
                pair[0].path < pair[1].path,
                "equal profit has to fall back on identity: {:?} vs {:?}",
                pair[0].path,
                pair[1].path
            );
        }
    }

    // Identity is a property of the finding, not of the run (§33): chain, block,
    // route — and the input token is the route's own, never a separate guess.
    for opportunity in &detection.opportunities {
        assert_eq!(
            opportunity.identity(),
            (CHAIN, BlockNumber(BLOCK), opportunity.path)
        );
        assert_eq!(opportunity.input_token, opportunity.path.input_token());
        assert_eq!(opportunity.chain_id, CHAIN);
        assert_eq!(opportunity.block_number, BlockNumber(BLOCK));
    }
}

#[test]
fn rejections_and_skips_have_their_own_documented_order() {
    let detection = detect_opportunities(&market(&mirrored())).expect("detect");
    let table = peaks();

    // Candidates: ascending by route identity, which is what the sorted scan
    // promises and what the graph's traversal order must not disturb.
    let identities: Vec<_> = detection.candidates.iter().map(|p| p.identity()).collect();
    let mut sorted = identities.clone();
    sorted.sort();
    assert_eq!(identities, sorted, "candidates in identity order");

    // Rejections: ascending by route identity, then by reason.
    let keys: Vec<_> = detection
        .rejected
        .iter()
        .map(|r: &CandidateRejection| (r.path, r.reason))
        .collect();
    let mut sorted_keys = keys.clone();
    sorted_keys.sort();
    assert_eq!(keys, sorted_keys, "rejections in (route, reason) order");
    assert_eq!(
        detection
            .rejected
            .iter()
            .map(|r| route(&r.path))
            .collect::<Vec<_>>(),
        [
            (token(B), [pool(P1), pool(P2)]),
            (token(A), [pool(P2), pool(P1)]),
            (token(C), [pool(P3), pool(P4)]),
            (token(A), [pool(P4), pool(P3)]),
        ],
        "the four expensive-way-round routes, in identity order"
    );
    // Each rejection is a route the scan also finds unprofitable, and it failed on
    // the market's verdict rather than on a technical fault.
    for rejection in &detection.rejected {
        let measured = peak_of(&table, &route(&rejection.path));
        assert!(measured <= 0, "{:?} profits {measured}", rejection.path);
        assert_eq!(
            rejection.reason,
            RejectionReason::Unprofitable,
            "{:?} failed for a technical reason",
            rejection.path
        );
        let peak_simulation = rejection.peak.expect("the near miss stays auditable");
        assert!(
            peak_simulation.gross_profit().is_none(),
            "{peak_simulation:?}"
        );
    }

    // Skipped pairs: the same-pool round trips, ascending by hop identity.
    let skip_keys: Vec<_> = detection
        .skipped_pairs
        .iter()
        .map(|s: &SkippedPair| (s.first, s.second, s.reason))
        .collect();
    let mut sorted_skips = skip_keys.clone();
    sorted_skips.sort();
    assert_eq!(skip_keys, sorted_skips, "skips in hop-identity order");
    assert_eq!(
        detection.skipped_pairs.len(),
        8,
        "four pools, one self-trip in each direction"
    );
    assert!(detection
        .skipped_pairs
        .iter()
        .all(|s| s.first.pool == s.second.pool));
}

// ---------------------------------------------------------------------------
// §56: the serialized form
// ---------------------------------------------------------------------------

#[test]
fn the_json_report_is_stable_and_carries_the_market_behind_every_number() {
    let detection = detect_opportunities(&market(&mirrored())).expect("detect");
    let json = serde_json::to_string(&detection.opportunities).expect("serialize");
    assert_eq!(
        json,
        serde_json::to_string(&detection.opportunities).expect("serialize again"),
        "serializing twice gives one string"
    );
    // The permutation of the same market has to give the same bytes — the part a
    // `Debug` equality would not catch.
    let shuffled =
        detect_opportunities(&market(&[pool3(), pool1(), pool4(), pool2()])).expect("detect");
    assert_eq!(
        json,
        serde_json::to_string(&shuffled.opportunities).expect("serialize")
    );

    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    let best = &value.as_array().expect("a list")[0];
    // §55's audit fields, all present in the one object.
    for field in [
        "chain_id",
        "block_number",
        "path",
        "input_token",
        "input_amount",
        "output_amount",
        "gross_profit",
        "hops",
        "search",
    ] {
        assert!(best.get(field).is_some(), "missing {field} in {best}");
    }
    let hops = best["hops"].as_array().expect("two hops");
    assert_eq!(hops.len(), 2);
    for hop in hops {
        for field in ["pool", "reserve_in", "reserve_out", "fee"] {
            assert!(hop.get(field).is_some(), "a hop without {field}: {hop}");
        }
        assert!(hop["fee"]["numerator"].is_number());
        assert!(hop["fee"]["denominator"].is_number());
    }
    let record = &best["search"];
    for field in [
        "strategy",
        "rounds",
        "evaluations",
        "lower_bound",
        "upper_bound",
        "interval_closed",
        "scan_count",
        "policy",
    ] {
        assert!(record.get(field).is_some(), "a record without {field}");
    }
    assert_eq!(record["interval_closed"], true);

    // The amounts a reader outside this process parses are the struct's own
    // numbers, and they state the same profit twice.
    let opportunity = &detection.opportunities[0];
    for field in ["input_amount", "output_amount", "gross_profit"] {
        let from_json: U256 = serde_json::from_value(best[field].clone()).expect(field);
        assert_eq!(from_json, amount(opportunity, field), "{field}");
    }
    assert_eq!(
        opportunity.output_amount - opportunity.input_amount,
        opportunity.gross_profit
    );
    // The domain travels with the finding, so the search's claim is checkable
    // without the graph it came from: `second.reserve_out - 1`, derived.
    let upper: U256 = serde_json::from_value(record["upper_bound"].clone()).expect("bound");
    assert_eq!(upper, opportunity.search.upper_bound);
    assert_eq!(upper, U256::from(124_999u32), "125 000 - 1");
    let lower: U256 = serde_json::from_value(record["lower_bound"].clone()).expect("bound");
    assert_eq!(lower, U256::ONE);
}

fn amount(opportunity: &Opportunity, field: &str) -> U256 {
    match field {
        "input_amount" => opportunity.input_amount,
        "output_amount" => opportunity.output_amount,
        _ => opportunity.gross_profit,
    }
}

#[test]
fn the_audit_form_prints_the_same_market_every_time() {
    // §55 is a human-readable line as well as a struct: the same finding rendered
    // twice is one string, a finding from a re-registered market is the same
    // string, and both pools' reserves and fees are in it.
    let detection = detect_opportunities(&market(&mirrored())).expect("detect");
    let opportunity = detection.best().expect("a best");
    let rendered = format!("{opportunity}");
    assert_eq!(rendered, format!("{opportunity}"));

    let reshuffled =
        detect_opportunities(&market(&[pool4(), pool2(), pool3(), pool1()])).expect("detect");
    assert_eq!(
        rendered,
        format!(
            "{}",
            reshuffled.best().expect("a best after re-registering")
        )
    );

    let [first, second] = opportunity.hops;
    assert!(rendered.contains(&first.reserve_in.to_string()));
    assert!(rendered.contains(&first.reserve_out.to_string()));
    assert!(rendered.contains(&second.reserve_in.to_string()));
    assert!(rendered.contains(&first.fee.numerator.to_string()));
    assert!(rendered.contains(&second.fee.denominator.to_string()));
    assert!(rendered.contains("gross profit"));
    assert!(rendered.contains(&opportunity.gross_profit.to_string()));
    assert!(rendered.contains(&opportunity.input_amount.to_string()));
}
