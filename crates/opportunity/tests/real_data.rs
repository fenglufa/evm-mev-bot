//! M3 real-data acceptance: two verified pools on one token pair, one captured
//! block, and the question the whole milestone exists to answer — does this
//! market state contain a two-pool route that gains, and is the number the
//! detector reports the number the chain's own arithmetic produces?
//!
//! Everything here comes from chain 91342 as it actually is. The pools, their
//! tokens, their reserves and above all their **fees** are attested in
//! `data/protocols-m3/v2-sepolia-42000006-cffe7472.json`, and the reserves are
//! the `Sync` logs inside `fixtures/real-m3/block-37191169.json`. No reserve was
//! edited, no pool was invented, and no fee was defaulted: §26 of the task
//! forbids exactly that, and §10 forbids reading a guessed 0.3 % in its place.
//!
//! The route is not theoretical either. The transaction that set both pools'
//! reserves at this block — `0x6132a9da…92ba`, tx 18 of the captured block — is
//! someone on-chain doing `WETH -> pool A -> TTAX -> pool B -> WETH` at these
//! very two pools. The graph this test prices is the state left behind by that
//! trade.

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address, U256};

use evm_chain::RecordedChainAdapter;
use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};
use evm_graph::{GraphSnapshot, MarketGraphBuilder};
use evm_opportunity::{
    detect_opportunities, enumerate_candidates, ArbitragePath, Detection, Opportunity,
    OpportunityDetector, RejectionReason, SearchPolicy, SearchStrategy,
};
use evm_protocol::{PoolAttestation, Registry, V2Adapter};
use evm_replay::ReplayEngine;
use evm_state::InMemoryStateStore;

const CHAIN: ChainId = ChainId(91342);

/// The one block both pools last synced in — their own `Sync` logs, and the only
/// block the detector is allowed to price.
const BLOCK: u64 = 37_191_169;

/// `0xf487d533…6578`, created 37 187 524 by factory `0x5f6e8a56…`.
const POOL_A: Address = address!("0xf487d533cae6cddd0c7e7bbbac084dd04d876578");
/// `0x5bef6275…7440`, created 37 187 526 by a *different* factory,
/// `0x1e594a50…`, on the same token pair. Two pools per pair is what makes a
/// two-hop cycle possible at all; the four M2 pools never had it.
const POOL_B: Address = address!("0x5bef6275607901dcd58160356660151be0637440");

const TOKEN_WETH: Address = address!("0x4200000000000000000000000000000000000006");
const TOKEN_TTAX: Address = address!("0xcffe7472a7a1a6947f56233854ae91a54c862f62");

/// Reserves exactly as each pool's `Sync` log at this block states them
/// (reserve0, reserve1), before any quoting.
const A_R0: u128 = 35_099_900_253_008;
const A_R1: u128 = 45_655_538_604_883_371_699;
const B_R0: u128 = 36_641_298_079_327;
const B_R1: u128 = 43_677_608_078_641_141_054;

/// The fee both pools are attested at, proved per pool by bracketing their own
/// trades between two of their own `Sync` logs.
const FEE: Fee = Fee {
    numerator: 997,
    denominator: 1000,
};

/// The peaks the detector produces on this market, reproduced by two methods
/// that are not the search: an exhaustive scan of consecutive inputs around the
/// answer, and the closed-form ceiling of the unfloored composition (`ceiling`
/// below). The WETH route's profit equals that ceiling exactly, which makes it
/// the global integer optimum and not merely the best the policy found; the
/// TTAX route sits a few thousand units under it.
///
/// Both inputs are one point on a wide flat top — the WETH route pays
/// 29 641 519 810 on every input in `714 844 520 992 ..= 714 845 020 992`, so
/// the input is a property of the policy's tie-breaking, the profit is the fact.
const WETH_ROUTE_INPUT: u128 = 714_844_720_992;
const WETH_ROUTE_OUTPUT: u128 = 744_486_240_802;
const WETH_ROUTE_PROFIT: u128 = 29_641_519_810;
const TTAX_ROUTE_INPUT: u128 = 890_134_426_448_791_298;
const TTAX_ROUTE_OUTPUT: u128 = 927_044_426_434_210_645;
const TTAX_ROUTE_PROFIT: u128 = 36_909_999_985_419_347;

/// The same two routes measured a third way: the smooth maximum of the unfloored
/// composition ([`analytic_ceiling`]), and how far the reported TTAX peak sits
/// below its own ceiling.
const WETH_CEILING: u128 = 29_641_519_810;
const TTAX_CEILING: u128 = 36_909_999_985_422_472;
const TTAX_CEILING_GAP: u128 = 3_125;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn m3_registry() -> Registry {
    Registry::load_dir(&workspace_root().join("data/protocols-m3"))
        .expect("the committed M3 registry loads and validates")
}

fn registry_for(pools: &[Address]) -> Registry {
    let all = m3_registry();
    let mut chosen = Registry::default();
    for pool in pools {
        let attestation = all
            .get(PoolId::new(CHAIN, *pool))
            .unwrap_or_else(|| panic!("{pool} is attested in data/protocols-m3"));
        chosen.attest(attestation.clone());
    }
    chosen.validate().expect("the subset is still evidenced");
    chosen
}

/// Replay the captured block and build the market graph it leaves behind.
async fn market(pools: &[Address]) -> GraphSnapshot {
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real-m3"), CHAIN)
        .expect("the captured block loads");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry_for(pools)))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    engine
        .replay_block(BlockNumber(BLOCK), &mut report)
        .await
        .expect("the block replays");
    assert_eq!(report.rejected_syncs, 0, "{:?}", report.rejections);
    MarketGraphBuilder::new()
        .build(&engine.snapshot())
        .expect("both pools synced in this block")
}

fn r(value: u128) -> U256 {
    U256::from(value)
}

fn token(a: Address) -> TokenId {
    TokenId::new(CHAIN, a)
}

fn pool(a: Address) -> PoolId {
    PoolId::new(CHAIN, a)
}

/// A pool's own `(reserve0, reserve1)` at [`BLOCK`], straight from its `Sync` log.
fn pool_reserves(pool: PoolId) -> (U256, U256) {
    assert_eq!(pool.chain_id, CHAIN);
    if pool.address == POOL_A {
        (r(A_R0), r(A_R1))
    } else {
        assert_eq!(pool.address, POOL_B);
        (r(B_R0), r(B_R1))
    }
}

/// A quote written here, in the test, so the crate's own math is not the only
/// thing asserting what the crate's own math does. Same formula as the chain's
/// pair — which M1 verified log by log against real `Swap` outputs.
fn quote(reserve_in: U256, reserve_out: U256, x: U256, fee: Fee) -> U256 {
    let n = U256::from(fee.numerator);
    let d = U256::from(fee.denominator);
    let held = x * n;
    held * reserve_out / (reserve_in * d + held)
}

fn round_trip(first: (U256, U256), second: (U256, U256), x: U256) -> U256 {
    let mid = quote(first.0, first.1, x, FEE);
    quote(second.0, second.1, mid, FEE)
}

/// §46: the search may not validate itself. Two independent sweeps of the same
/// route, neither of which is the optimizer:
///
/// * `sweep` walks the **whole derived domain** `1 ..= second.reserve_out - 1` at
///   a fixed stride and returns the best profit seen — if the search's answer
///   were beaten anywhere in the domain, this is where it shows up.
/// * `window` walks every single input for `radius` units either side of the
///   reported one, which makes the reported peak a local maximum rather than an
///   artefact of where the ternary probes happened to land.
fn sweep(first: (U256, U256), second: (U256, U256), points: u32) -> (U256, U256, u32) {
    let upper = second.1 - U256::ONE;
    let stride = (upper / U256::from(points)).max(U256::ONE);
    let mut best = (U256::ZERO, U256::ZERO);
    let mut evaluated = 0u32;
    let mut x = U256::ONE;
    while x <= upper {
        let out = round_trip(first, second, x);
        let gained = out.checked_sub(x).unwrap_or(U256::ZERO);
        let best_gained = best.1.checked_sub(best.0).unwrap_or(U256::ZERO);
        if gained > best_gained {
            best = (x, out);
        }
        evaluated += 1;
        x += stride;
    }
    (best.0, best.1, evaluated)
}

fn window_max(first: (U256, U256), second: (U256, U256), centre: U256, radius: u32) -> (U256, u32) {
    let from = centre.saturating_sub(U256::from(radius)).max(U256::ONE);
    let to = (centre + U256::from(radius)).min(second.1 - U256::ONE);
    let mut best = U256::ZERO;
    let mut evaluated = 0u32;
    let mut x = from;
    while x <= to {
        let out = round_trip(first, second, x);
        let gained = out.checked_sub(x).unwrap_or(U256::ZERO);
        if gained > best {
            best = gained;
        }
        evaluated += 1;
        x += U256::ONE;
    }
    (best, evaluated)
}

/// Floor square root by bisection, with the defining property asserted rather
/// than assumed: `r*r <= x` and `(r+1)*(r+1) > x`.
fn isqrt(x: U256) -> U256 {
    let mut lo = U256::ZERO;
    let mut hi = x;
    while lo < hi {
        let span = hi - lo;
        let mid = lo + span / U256::from(2u8) + U256::ONE;
        match mid.checked_mul(mid) {
            Some(square) if square <= x => lo = mid,
            _ => hi = mid - U256::ONE,
        }
    }
    assert!(lo * lo <= x, "{lo} is not a square root of {x}");
    let next = lo + U256::ONE;
    let undershot = match next.checked_mul(next) {
        Some(square) => square <= x,
        None => false,
    };
    assert!(!undershot, "{lo} is below the square root of {x}");
    lo
}

/// §23: the search is not allowed to certify its own answer, so the certificate
/// here is a closed form that never touches it. Composing the two hops
/// **unfloored** gives a smooth function of the input,
///
/// ```text
/// out(x) - x = P·x / (Q + S·x) - x
/// P = n²·Ro1·Ro2   Q = d²·Ri1·Ri2   S = d·n·Ri2 + n²·Ro1
/// ```
///
/// maximised at `x* = (√(P·Q) - Q) / S` with value `(√P - √Q)² / S`. Each floor
/// the real quote takes only removes value, so no integer input can beat that
/// smooth maximum. `P·Q` does not fit in 256 bits, but
/// `√(P·Q) = n·d·√(Ro1·Ro2·Ri1·Ri2)` does, so one exact integer square root of
/// the four reserves gives the bound.
///
/// Returns `(ceiling, x_star)`, with `ceiling` the integer floor of the smooth
/// maximum — at most one above it, never below. A reported peak that equals it
/// is therefore the global optimum over the integers, not merely the best input
/// the policy happened to reach.
fn analytic_ceiling(first: (U256, U256), second: (U256, U256), fee: Fee) -> (U256, U256) {
    let n = U256::from(fee.numerator);
    let d = U256::from(fee.denominator);
    let p = n * n * first.1 * second.1;
    let q = d * d * first.0 * second.0;
    let slope = d * n * second.0 + n * n * first.1;
    let root = n * d * isqrt(first.1 * second.1 * first.0 * second.0);
    ((p + q - U256::from(2u8) * root) / slope, (root - q) / slope)
}

fn weth_legs() -> ((U256, U256), (U256, U256)) {
    ((r(A_R0), r(A_R1)), ((r(B_R1)), r(B_R0)))
}
fn ttax_legs() -> ((U256, U256), (U256, U256)) {
    ((r(B_R1), r(B_R0)), (r(A_R0), r(A_R1)))
}

/// The four routes a two-pool, two-token graph contains, in the order the
/// detector's own enumeration produces.
fn expected_candidates(snapshot: &GraphSnapshot) -> Vec<ArbitragePath> {
    enumerate_candidates(snapshot).expect("the graph enumerates")
}

fn find(detection: &Detection, input: Address) -> Opportunity {
    detection
        .opportunities
        .iter()
        .find(|o| o.input_token.address == input)
        .copied()
        .unwrap_or_else(|| panic!("a route spends {input}"))
}

// ---------------------------------------------------------------------------

/// The registry file is evidence, not configuration: both pools carry a fee,
/// that fee came from a proof rather than a default, and every attestation
/// names the logs it was read out of.
#[test]
fn both_pools_are_attested_with_a_fee_and_a_shared_pair() {
    let registry = m3_registry();
    assert_eq!(registry.pools.len(), 2, "exactly the two route pools");
    let a = registry.get(pool(POOL_A)).expect("pool A");
    let b = registry.get(pool(POOL_B)).expect("pool B");

    for attestation in [a, b] {
        assert_eq!(attestation.fee, Some(FEE), "fee 997/1000, never a default");
        assert_eq!(attestation.token0, token(TOKEN_WETH));
        assert_eq!(attestation.token1, token(TOKEN_TTAX));
        assert!(
            attestation.evidence.identity.len() >= 6,
            "the fee proof is carried as per-trade evidence: {} identity refs",
            attestation.evidence.identity.len()
        );
        assert_eq!(attestation.evidence.tokens.len(), 4);
        assert_eq!(attestation.evidence.state.len(), 5);
    }
    // The pair is shared, which is what the four M2 pools never had.
    assert_eq!((a.token0, a.token1), (b.token0, b.token1));
    assert_ne!(a.pool, b.pool);
}

#[tokio::test]
async fn the_captured_block_yields_one_graph_with_both_pools() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    assert_eq!(snapshot.block_number(), BlockNumber(BLOCK));
    assert_eq!(snapshot.chain_id(), CHAIN);
    assert_eq!(snapshot.pool_count(), 2);
    assert_eq!(snapshot.edge_count(), 4, "two directions through each pool");
    let mut tokens: Vec<_> = snapshot.nodes().copied().collect();
    tokens.sort();
    assert_eq!(tokens, vec![token(TOKEN_WETH), token(TOKEN_TTAX)]);
}

/// The headline: this real market state contains exactly two profitable routes,
/// both directions of the same pair of pools, and the detector reports the
/// amounts the independent oracle computed.
#[tokio::test]
async fn the_detector_reports_the_two_routes_the_oracle_predicted() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let detection = detect_opportunities(&snapshot).expect("detection");

    assert_eq!(detection.chain_id, CHAIN);
    assert_eq!(detection.block_number, BlockNumber(BLOCK));
    assert_eq!(
        detection.candidates,
        expected_candidates(&snapshot),
        "every route the graph contains was priced"
    );
    assert_eq!(detection.candidates.len(), 4);
    // Four ordered edge pairs never became routes, all for the same reason: a
    // pool traded out and back along itself. §7 makes that a fee donation, not
    // a market, and the refusal is reported by name rather than silently.
    assert_eq!(detection.skipped_pairs.len(), 4);
    for skipped in &detection.skipped_pairs {
        assert!(matches!(
            skipped.reason,
            evm_opportunity::PathError::SamePool(_, _)
        ));
        assert_eq!(skipped.first.pool, skipped.second.pool);
    }
    assert_eq!(detection.evaluated_count(), 4);

    assert_eq!(detection.opportunities.len(), 2, "{:?}", detection.rejected);
    assert_eq!(detection.rejected.len(), 2);

    let weth = find(&detection, TOKEN_WETH);
    assert_eq!(weth.input_amount, r(WETH_ROUTE_INPUT));
    assert_eq!(weth.output_amount, r(WETH_ROUTE_OUTPUT));
    assert_eq!(weth.gross_profit, r(WETH_ROUTE_PROFIT));
    assert_eq!(weth.input_token, token(TOKEN_WETH));
    assert_eq!(weth.path.pools(), [pool(POOL_A), pool(POOL_B)]);
    assert_eq!(weth.path.mid_token(), token(TOKEN_TTAX));

    let ttax = find(&detection, TOKEN_TTAX);
    assert_eq!(ttax.input_amount, r(TTAX_ROUTE_INPUT));
    assert_eq!(ttax.output_amount, r(TTAX_ROUTE_OUTPUT));
    assert_eq!(ttax.gross_profit, r(TTAX_ROUTE_PROFIT));
    assert_eq!(ttax.path.pools(), [pool(POOL_B), pool(POOL_A)]);
    assert_eq!(ttax.path.mid_token(), token(TOKEN_WETH));

    // Sorted by profit descending, so the best route leads without a lookup.
    assert_eq!(detection.opportunities[0], ttax);
    assert_eq!(detection.opportunities[1], weth);
    assert_eq!(detection.best(), Some(&ttax));

    // Both hops of each finding carry the reserves and fees they were priced
    // against — the audit does not have to trust the snapshot still matches.
    for opportunity in &detection.opportunities {
        let [first, second] = opportunity.hops;
        assert_eq!(first.fee, FEE);
        assert_eq!(second.fee, FEE);
        for (hop, hop_id) in [
            (first, opportunity.path.first()),
            (second, opportunity.path.second()),
        ] {
            let (reserve0, reserve1) = pool_reserves(hop.pool);
            // Both pairs order token0 = WETH by address, so the direction the
            // trade takes decides which of the pool's two reserves leads.
            let expected = if hop_id.token_in == token(TOKEN_WETH) {
                (reserve0, reserve1)
            } else {
                (reserve1, reserve0)
            };
            assert_eq!(
                (hop.reserve_in, hop.reserve_out),
                expected,
                "hop through {:?} carrying {:?}",
                hop.pool,
                hop_id.token_in
            );
        }
        assert!(opportunity.search.interval_closed);
    }

    // The two routes that lost are rejected with their peak kept, not dropped.
    for rejection in &detection.rejected {
        assert_eq!(rejection.reason, RejectionReason::Unprofitable);
        let peak = rejection.peak.expect("a priced route reports its peak");
        assert!(peak.output <= peak.input);
    }
}

/// §46 in the only form that counts on real reserves: the reported peak is
/// beaten nowhere in the domain a sweep can see, and is the maximum over the
/// thousand-odd inputs around it, computed here rather than by the search.
#[tokio::test]
async fn an_independent_scan_does_not_beat_either_reported_peak() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let detection = detect_opportunities(&snapshot).expect("detection");

    for (opportunity, legs) in [
        (find(&detection, TOKEN_WETH), weth_legs()),
        (find(&detection, TOKEN_TTAX), ttax_legs()),
    ] {
        let (first, second) = legs;
        assert_eq!(
            round_trip(first, second, opportunity.input_amount),
            opportunity.output_amount,
            "the test's own quote reproduces the reported output"
        );

        let (best_in, best_out, evaluated) = sweep(first, second, 100_000);
        let best_profit = best_out - best_in;
        assert!(
            opportunity.gross_profit >= best_profit,
            "the search reported {} but a {}-point sweep of the whole domain found \
             {} at input {}",
            opportunity.gross_profit,
            evaluated,
            best_profit,
            best_in,
        );

        let (window_best, points) = window_max(first, second, opportunity.input_amount, 5_000);
        assert_eq!(
            opportunity.gross_profit, window_best,
            "the reported peak is not the maximum over {points} consecutive inputs"
        );

        // The derived domain bound holds on the real numbers too.
        assert!(opportunity.input_amount <= second.1 - U256::ONE);
        assert!(opportunity.input_amount >= U256::ONE);
    }
}

/// The price discrepancy itself, stated without the search: the two pools
/// disagree by more than the round trip costs, on both directions of one pair.
#[tokio::test]
async fn the_two_pools_disagree_by_more_than_their_fees() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    assert_eq!(snapshot.edge_count(), 4);
    let detection = detect_opportunities(&snapshot).expect("detection");
    let weth = find(&detection, TOKEN_WETH);
    let [first, second] = weth.hops;

    // Product of the two ask/bid prices, as an exact ratio: no float anywhere.
    let product_num = first.reserve_out * second.reserve_out;
    let product_den = first.reserve_in * second.reserve_in;
    // A round trip needs product > (d/n)^2 = 1 000 000 / 994 009 to pay its fees.
    assert!(
        product_num * U256::from(994_009u64) > product_den * U256::from(1_000_000u64),
        "the fee threshold would have to be exceeded for any route to pay"
    );
    for rejection in &detection.rejected {
        assert_eq!(rejection.reason, RejectionReason::Unprofitable);
    }
    // The losing direction is the mirror image: same two pools, other side.
    assert_eq!(detection.rejected.len(), 2);
}

/// §10–§14 on real data: take the fee evidence away and the same market stops
/// producing findings. `None` is never read as 0.3 %.
#[tokio::test]
async fn without_attested_fees_the_same_market_produces_nothing() {
    let registry = registry_for(&[POOL_A, POOL_B]);
    let mut unattested = Registry::default();
    for attestation in registry.pools.values() {
        let mut stripped: PoolAttestation = attestation.clone();
        stripped.fee = None;
        unattested.attest(stripped);
    }
    unattested.validate().expect("still fully evidenced");

    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real-m3"), CHAIN)
        .expect("the captured block loads");
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(unattested))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    engine
        .replay_block(BlockNumber(BLOCK), &mut report)
        .await
        .expect("the block replays");
    let snapshot = MarketGraphBuilder::new()
        .build(&engine.snapshot())
        .expect("graph");

    let detection = detect_opportunities(&snapshot).expect("detection");
    assert_eq!(detection.candidates.len(), 4, "the routes still exist");
    assert!(
        detection.opportunities.is_empty(),
        "unattested fee produced {} findings",
        detection.opportunities.len()
    );
    assert_eq!(detection.rejected.len(), 4);
    for rejection in &detection.rejected {
        assert!(
            matches!(rejection.reason, RejectionReason::MissingFee(_)),
            "{:?}",
            rejection.reason
        );
        assert!(rejection.peak.is_none(), "an unpriced route has no peak");
    }
}

/// §55: one finding prints its chain, block, both tokens, both pools, the input,
/// the output, the gross profit, both pools' reserves and both fees.
#[tokio::test]
async fn the_audit_line_carries_every_number_a_reviewer_wants() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let detection = detect_opportunities(&snapshot).expect("detection");
    let line = detection.opportunities[0].to_string();
    for needle in [
        "chain 91342",
        &BLOCK.to_string(),
        &TOKEN_WETH.to_string(),
        &TOKEN_TTAX.to_string(),
        &POOL_A.to_string(),
        &POOL_B.to_string(),
        &TTAX_ROUTE_INPUT.to_string(),
        &TTAX_ROUTE_OUTPUT.to_string(),
        &TTAX_ROUTE_PROFIT.to_string(),
        "997/1000",
    ] {
        assert!(line.contains(needle), "{needle} missing from:\n{line}");
    }
    // Reserves of both pools, in trade direction, appear too.
    for reserve in [B_R1.to_string(), A_R1.to_string()] {
        assert!(line.contains(&reserve), "{reserve} missing from:\n{line}");
    }
    assert!(line.contains("BoundedTernary"));
    assert!(
        line.contains("closed true"),
        "the closing pass is reported:\n{line}"
    );
}

/// §5/§64: the search publishes its own budget use on real data, so the reported
/// optimum can be read as "the whole derived domain was closed" rather than as a
/// number the policy happened to stop at. These counts are the actual ones: if a
/// future change alters them, this test says so instead of the report going stale.
#[tokio::test]
async fn the_real_search_closes_its_interval_inside_the_published_budget() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let detection = detect_opportunities(&snapshot).expect("detection");

    for (opportunity, rounds, evaluations, scan_count, upper) in [
        (find(&detection, TOKEN_WETH), 50u32, 130u32, 30u32, B_R0 - 1),
        (find(&detection, TOKEN_TTAX), 104, 232, 24, A_R1 - 1),
    ] {
        let record = &opportunity.search;
        assert_eq!(record.strategy, SearchStrategy::BoundedTernary);
        assert_eq!(record.policy, SearchPolicy::default());
        assert_eq!(record.lower_bound, U256::ONE);
        assert_eq!(
            record.upper_bound,
            r(upper),
            "the domain ends where the second hop runs out of the token it pays back"
        );
        assert_eq!(
            (record.rounds, record.evaluations, record.scan_count),
            (rounds, evaluations, scan_count)
        );
        assert!(record.interval_closed, "{record:?}");
        assert!(record.rounds < record.policy.max_rounds);
        assert!(record.evaluations <= record.policy.max_rounds * 2 + record.scan_count);
    }
}

/// §31–§33: the same snapshot detected twice produces identical output, in the
/// same order, and §34–§35: detecting does not touch the market.
#[tokio::test]
async fn detection_is_repeatable_and_leaves_the_market_alone() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let before = snapshot.clone();
    let first = detect_opportunities(&snapshot).expect("first pass");
    let second = detect_opportunities(&snapshot).expect("second pass");
    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_string(&first.opportunities).expect("serialise"),
        serde_json::to_string(&second.opportunities).expect("serialise"),
        "the audit form is byte-stable"
    );
    assert_eq!(
        first
            .opportunities
            .iter()
            .map(|o| o.identity())
            .collect::<Vec<_>>()
            .len(),
        2
    );
    let mut ids = first
        .opportunities
        .iter()
        .map(|o| o.identity())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2, "two findings, two distinct identities");
    assert_eq!(snapshot, before, "quoting did not move a reserve");

    // A different policy is still a real search of the same domain: the peak it
    // reports cannot exceed what the default finds, and the record says what it
    // did.
    let narrow = OpportunityDetector::new(SearchPolicy {
        max_rounds: 40,
        scan_width: 8,
    })
    .detect(&snapshot)
    .expect("narrow search");
    assert_eq!(narrow.opportunities.len(), 2);
    for (wide, tight) in first.opportunities.iter().zip(narrow.opportunities.iter()) {
        assert!(wide.gross_profit >= tight.gross_profit);
        assert!(tight.search.rounds <= 40);
    }
}

/// §28: when the evidence does not support a finding, the answer is an auditable
/// null, not a smaller claim. The four pools M2 verified are that case, and the
/// reason is structural: no token pair among them has two pools.
#[tokio::test]
async fn the_m2_attested_set_is_an_auditable_null() {
    let chain = RecordedChainAdapter::load(&workspace_root().join("fixtures/real"), CHAIN)
        .expect("the M2 corpus loads");
    let registry = Registry::load_dir(&workspace_root().join("data/protocols"))
        .expect("the committed M2 registry loads");
    assert_eq!(registry.pools.len(), 4, "M2's four pools, untouched");
    assert!(
        registry.pools.values().all(|a| a.fee.is_none()),
        "M2 attested no fee, and M3 must not borrow one"
    );
    let mut engine = ReplayEngine::new(
        chain,
        vec![Box::new(V2Adapter::new(registry))],
        InMemoryStateStore::new(CHAIN),
    );
    let mut report = Default::default();
    for block in [
        5_455_035u64,
        5_457_650,
        10_544_346,
        31_390_683,
        37_257_255,
        37_258_093,
    ] {
        engine
            .replay_block(BlockNumber(block), &mut report)
            .await
            .expect("M2 block replays");
    }
    let snapshot = MarketGraphBuilder::new()
        .build(&engine.snapshot())
        .expect("M2 graph");
    assert_eq!(snapshot.block_number(), BlockNumber(37_258_093));

    let detection = detect_opportunities(&snapshot).expect("detection");
    // §28's six auditable fields, each measured here rather than narrated:
    // block range 5 455 035..=37 258 093, 6 snapshots replayed, 1 graph at block
    // 37 258 093, 0 candidate paths, 0 evaluated, no best candidate.
    assert_eq!(report.blocks, 6);
    assert_eq!(detection.block_number, BlockNumber(37_258_093));
    assert_eq!(detection.candidates.len(), 0, "no pair has two pools");
    assert_eq!(detection.evaluated_count(), 0);
    assert_eq!(detection.opportunities.len(), 0);
    assert_eq!(detection.rejected.len(), 0);
    assert!(detection.best_peak().is_none(), "nothing was priced at all");
    assert!(detection.is_empty());
    // The pools and edges are real; the missing ingredient is a second pool on
    // one pair, which the census over all 1 023 created pools says the M2 set
    // never had.
    assert_eq!(snapshot.pool_count(), 2);
    assert_eq!(snapshot.edge_count(), 4);
}

#[tokio::test]
async fn an_analytic_ceiling_proves_the_weth_route_is_the_global_optimum() {
    let snapshot = market(&[POOL_A, POOL_B]).await;
    let detection = detect_opportunities(&snapshot).expect("detection");

    let weth = find(&detection, TOKEN_WETH);
    let (weth_ceiling, weth_argmax) = analytic_ceiling(weth_legs().0, weth_legs().1, FEE);
    assert_eq!(
        weth_ceiling,
        r(WETH_CEILING),
        "the closed form of the real reserves, recomputed here"
    );
    assert_eq!(
        weth.gross_profit, weth_ceiling,
        "the search's profit is exactly the ceiling, so it is the best input \
         in the whole domain and not just the best the policy reached"
    );

    let ttax = find(&detection, TOKEN_TTAX);
    let (ttax_ceiling, ttax_argmax) = analytic_ceiling(ttax_legs().0, ttax_legs().1, FEE);
    assert_eq!(ttax_ceiling, r(TTAX_CEILING));
    assert!(ttax.gross_profit <= ttax_ceiling);
    assert_eq!(
        ttax_ceiling - ttax.gross_profit,
        r(TTAX_CEILING_GAP),
        "the reported peak is this far under its own ceiling: a measured \
         shortfall, not a claim of optimality"
    );

    // Both argmaxes are inside the domain the search is allowed to use, and
    // quoting them with the test's own floored math cannot beat the report —
    // the smooth optimum and the discrete one agree to within a handful of
    // units of the input, which is what a staircase is expected to cost.
    for (legs, argmax, profit) in [
        (weth_legs(), weth_argmax, weth.gross_profit),
        (ttax_legs(), ttax_argmax, ttax.gross_profit),
    ] {
        assert!(
            argmax >= U256::ONE && argmax <= legs.1 .1 - U256::ONE,
            "{argmax} is outside the derived domain"
        );
        let at_argmax = round_trip(legs.0, legs.1, argmax);
        let gained = at_argmax.checked_sub(argmax).unwrap_or(U256::ZERO);
        assert!(
            gained <= profit,
            "the analytic argmax {argmax} pays {gained}, more than the reported \
             {profit}: the ceiling formula and the quote disagree"
        );
    }
}

/// Where the fee brackets' pre-trade reserves actually come from, checked
/// against the captured block rather than asserted in prose.
///
/// §26 makes the fee an attested fact, and each bracket is measured against the
/// reserves a trade *started* from. A V2 pair emits `Sync` before it emits
/// `Swap`: the reserves are written, the outputs are paid, then `Sync`, then
/// `Swap`. So the `Sync` sitting inside a swap transaction carries that very
/// trade's **post**-trade pair, and the pre-trade pair can only come from this
/// pool's previous `Sync` log — the seeding event for pool B, the previous
/// block's log for pool A. Both directions are pinned here from the fixture's
/// own words: `Sync` immediately before `Swap`, the `Swap` amounts, and the
/// arithmetic that connects the registry's pre-trade pair to the reserves the
/// graph is built from. Reverse the causal claim and the last two assertions
/// fail.
#[test]
fn the_sync_inside_a_swap_transaction_carries_post_trade_reserves() {
    const SYNC_TOPIC: &str = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";
    const SWAP_TOPIC: &str = "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822";

    fn word(data: &str, index: usize) -> u128 {
        let start = 2 + 64 * index;
        u128::from_str_radix(&data[start..start + 64], 16).expect("a 32-byte word")
    }

    // (pool, the pre-trade pair its fee bracket quotes against,
    //  (amount0In, amount1In, amount0Out, amount1Out) from the Swap log,
    //  the post-trade pair the same transaction's Sync log carries)
    let cases = [
        (
            POOL_A,
            (32_270_543_229_562u128, 49_646_431_799_285_044_030),
            (2_829_357_023_446, 0, 0, 3_990_893_194_401_672_331),
            (A_R0, A_R1),
        ),
        (
            POOL_B,
            (40_000_000_000_000u128, 40_000_000_000_000_000_000),
            (0, 3_677_608_078_641_141_054, 3_358_701_920_673, 0),
            (B_R0, B_R1),
        ),
    ];

    let fixture = workspace_root().join("fixtures/real-m3/block-37191169.json");
    let raw = std::fs::read_to_string(&fixture).expect("the captured block is committed");
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("the captured block is valid JSON");
    let logs = value["receipts"]
        .as_array()
        .expect("receipts")
        .iter()
        .flat_map(|receipt| {
            receipt["logs"]
                .as_array()
                .expect("a receipt carries a logs array")
        })
        .collect::<Vec<_>>();

    for (pool, (pre0, pre1), (a0_in, a1_in, a0_out, a1_out), (post0, post1)) in cases {
        let mut syncs = Vec::new();
        let mut swaps = Vec::new();
        for log in &logs {
            let address = log["address"]
                .as_str()
                .expect("a log has an address")
                .parse::<Address>()
                .expect("the log address is a hex address");
            if address != pool {
                continue;
            }
            let topic0 = log["topics"].as_array().expect("a log has topics")[0]
                .as_str()
                .expect("topic 0 is a string")
                .to_ascii_lowercase();
            let entry = (
                log["tx_hash"].as_str().expect("tx hash").to_string(),
                log["tx_index"].as_u64().expect("tx index"),
                log["log_index"].as_u64().expect("log index"),
                log["data"].as_str().expect("data").to_string(),
            );
            match topic0.as_str() {
                SYNC_TOPIC => syncs.push(entry),
                SWAP_TOPIC => swaps.push(entry),
                _ => {}
            }
        }
        assert_eq!(syncs.len(), 1, "{pool} syncs in this block");
        assert_eq!(swaps.len(), 1, "{pool} swaps in this block");
        let (sync_tx, sync_tx_index, sync_log, sync_data) = &syncs[0];
        let (swap_tx, swap_tx_index, swap_log, swap_data) = &swaps[0];

        // Same transaction, and the Sync is the log straight before the Swap.
        assert_eq!(sync_tx, swap_tx, "one transaction");
        assert_eq!(sync_tx_index, swap_tx_index, "one transaction index");
        assert_eq!(
            *sync_log + 1,
            *swap_log,
            "{pool}'s Sync must be emitted before its Swap event"
        );

        // The Swap's own amounts, as the bracket records them.
        assert_eq!(
            (
                word(swap_data, 0),
                word(swap_data, 1),
                word(swap_data, 2),
                word(swap_data, 3)
            ),
            (a0_in, a1_in, a0_out, a1_out),
            "{pool}'s Swap log amounts"
        );

        // The same-transaction Sync holds the post-trade pair — not the pre-trade
        // pair the bracket is measured against — and the gap between the two is
        // exactly this trade's in and out.
        assert_eq!(
            (word(sync_data, 0), word(sync_data, 1)),
            (post0, post1),
            "{pool}'s Sync log carries the reserves the graph is built from"
        );
        assert_eq!(
            (pre0 + a0_in - a0_out, pre1 + a1_in - a1_out),
            (post0, post1),
            "{pool}'s Sync log is its pre-trade reserves adjusted by this Swap, \
             which is what makes it post-trade rather than pre-trade"
        );
        assert_ne!(
            (pre0, pre1),
            (word(sync_data, 0), word(sync_data, 1)),
            "{pool}'s bracket quotes against {pre0}/{pre1}, so the Sync inside \
             this transaction cannot be that pair"
        );
        assert_eq!(
            pool_reserves(PoolId::new(CHAIN, pool)),
            (r(post0), r(post1)),
            "the Sync log read here is the reserve pair the rest of this file attests"
        );
    }
}
