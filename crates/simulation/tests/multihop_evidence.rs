//! M11 §42–§44: the evidence directory, written so a second program can disagree with it.
//!
//! ```text
//! cargo test -p evm-simulation --test multihop_evidence -- --test-threads=1
//! ```
//!
//! §42 names the tree; §43 names what has to be recompute-able — pricing, the optimizer's answer,
//! the simulation summary, the route identity, the plan hash and the calldata hash — and forbids a
//! directory whose only verification is a `println!("PASS")`. So every row here carries two
//! versions of the same figure:
//!
//! * **published** — what the pipeline answered (`price`, `optimize`, REVM, risk, the plan);
//! * **recomputed independently** — what this file's own arithmetic answered, from the fields the
//!   row also publishes: a constant-product fold written here in [`U256`] (`in*num*r_out /
//!   (r_in*den + in*num)`, floor division, hop by hop), a rotation-minimum written here over the
//!   published edge list, a grid rebuilt here from the published domain and policy, a keccak over
//!   the published bytes.
//!
//! The two versions are published side by side with an `agrees` flag, so a reader can see the
//! comparison rather than take it. `crates/execution/tests/multihop_evidence_gate.rs` is the
//! program that does it again from disk, in another crate, and additionally re-runs REVM through
//! the library API rather than through this file's harness. Where the two disagree, the row was
//! never evidence.
//!
//! ## What is not claimed
//!
//! Nothing here is a real-market verdict (§41). The two-hop runs on the market M7 froze; the
//! three-hop runs on that state extended by pools this fixture introduced (declared row by row in
//! `tests/multihop_state`); the four-hop row is priced on a hand-written graph and is *not* run
//! through the EVM at all, because M9.3's search refuses a candidate deeper than three hops and
//! the executor's own `MAX_LEGS` is reached at four (§42's `not_measured`). `real/` carries the
//! three §42 names and answers all three `UNKNOWN`, with the reason for each.
//!
//! §44: no read here reaches a node. Every run on every page is served by
//! [`evm_simulation::state::DumpStateProvider`] built from a committed dump file, whose type name
//! and whose endpoint-free environment are recorded per row as the measurement; `rpc_count` is 0
//! and nothing in M11 added a read to a hot path.
//!
//! Nothing here signs, broadcasts, or holds a key. Writes to `data/evidence/m11/` only, and the
//! gate that reads them writes nothing.

use std::collections::BTreeMap;
use std::path::Path;

use alloy_primitives::{keccak256, Address, B256, U256};
use serde_json::{json, Map, Value};

use evm_core::{BlockNumber, Fee};
use evm_execution::{
    CandidateLane, CapitalDomain, ExecutablePlan, LaneFailure, LaneId, LaneLedger, LaneStanding,
    LaneState, MarketKind, MultihopBinding, MultihopPlanContext,
};
use evm_graph::EdgeId;
use evm_opportunity::{
    optimize, price, Gross, MultiHopRoute, OptimizationPolicy, OptimizedCandidate, Price,
};
use evm_pathfinder::{find_cycles, CycleCandidate, FeeStatus, PathFinderConfig};
use evm_risk::{MarketFacts, MultihopAcceptance, MultihopRiskDecision, MultihopRiskPolicy};
use evm_simulation::executor::{ExecutorOutcome, ExecutorRun};
use evm_simulation::gas::GasPricing;
use evm_simulation::state::DumpStateProvider;
use evm_simulation::SimulatedOpportunity;

mod executor_state;
mod multihop_market;
mod multihop_recorded;
mod multihop_state;

use executor_state::{amount_in, workspace_root, Fixture, BLOCK, CHAIN, POOL_A, WETH};
use multihop_market::{at_amount, edge_on, market_on, u};
use multihop_recorded::{
    candidate_at, file, fixture, plan_of, policy_at, recorded_acceptance, recorded_binding,
    recorded_context, recorded_cycle_candidate, recorded_facts, recorded_policy, request, run_ok,
};
use multihop_state::{cycle_amount_in, pools_in_order, Cycle};

// ---------------------------------------------------------------------------
// §42's tree
// ---------------------------------------------------------------------------

/// The one file format id, so a reader knows which keys are guaranteed.
const SCHEMA: &str = "m11-evidence-v1";
const EVIDENCE: &str = "data/evidence/m11";
const ASSEMBLED_BY: &str = "crates/simulation/tests/multihop_evidence.rs";
const CHECKED_BY: &str = "crates/execution/tests/multihop_evidence_gate.rs";

const PRICING_2HOP: &str = "data/evidence/m11/pricing/recorded_2hop.json";
const PRICING_3HOP: &str = "data/evidence/m11/pricing/declared_3hop.json";
const PRICING_4HOP: &str = "data/evidence/m11/pricing/declared_4hop.json";
const OPTIMIZER_2HOP: &str = "data/evidence/m11/optimizer/recorded_2hop.json";
const OPTIMIZER_3HOP: &str = "data/evidence/m11/optimizer/declared_3hop.json";
const SIMULATION_2HOP: &str = "data/evidence/m11/simulation/recorded_2hop.json";
const SIMULATION_3HOP: &str = "data/evidence/m11/simulation/declared_3hop.json";
const RISK_DECISION: &str = "data/evidence/m11/risk/decision.json";
const LANE_MATRIX: &str = "data/evidence/m11/lanes/lane_matrix.json";
const CONTROLLED_2HOP: &str = "data/evidence/m11/controlled/2hop/chain.json";
const CONTROLLED_3HOP: &str = "data/evidence/m11/controlled/3hop/chain.json";
const REAL_EXECUTION: &str = "data/evidence/m11/real/execution.json";
const REAL_FAILURE: &str = "data/evidence/m11/real/failure.json";
const REAL_RECONCILIATION: &str = "data/evidence/m11/real/reconciliation.json";
const MANIFEST: &str = "data/evidence/m11/manifest.json";
const README: &str = "data/evidence/m11/README.md";

/// M10's real-chain files, named by §42's rows as the nearest evidence that exists for the three
/// questions this milestone leaves `UNKNOWN`. These paths are checked for existence by the gate,
/// which is why each one is a committed file rather than the name a reader would guess.
const M10_REAL_EXECUTION: &str = "data/evidence/m10/real/giwa_execution.json";
const M10_REAL_FAILURE: &str = "data/evidence/m10/real/giwa_failure.json";
const M10_REAL_LADDER: &str = "data/evidence/m10/real/giwa_ladder_steps.json";
const M10_REAL_PRECONDITIONS: &str = "data/evidence/m10/real/preconditions.json";

/// Every file §42's tree asks for, so a typo in one path fails at the place that made it.
const REQUIRED: [&str; 16] = [
    PRICING_2HOP,
    PRICING_3HOP,
    PRICING_4HOP,
    OPTIMIZER_2HOP,
    OPTIMIZER_3HOP,
    SIMULATION_2HOP,
    SIMULATION_3HOP,
    RISK_DECISION,
    LANE_MATRIX,
    CONTROLLED_2HOP,
    CONTROLLED_3HOP,
    REAL_EXECUTION,
    REAL_FAILURE,
    REAL_RECONCILIATION,
    MANIFEST,
    README,
];

// ---------------------------------------------------------------------------
// Printing the numbers
// ---------------------------------------------------------------------------

fn addr(address: Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

fn dec(value: U256) -> String {
    value.to_string()
}

fn opt_dec(value: Option<U256>) -> Value {
    value
        .map(|v| Value::String(v.to_string()))
        .unwrap_or(Value::Null)
}

fn hash(value: B256) -> String {
    format!("{:#x}", value)
}

fn bytes_hex(data: &[u8]) -> String {
    format!("0x{}", hex::encode(data))
}

fn fee_json(fee: &Fee) -> Value {
    json!({ "numerator": fee.numerator, "denominator": fee.denominator })
}

fn opt_fee(fee: Option<Fee>) -> Value {
    fee.map(|f| fee_json(&f)).unwrap_or(Value::Null)
}

fn gross_json(gross: &Gross) -> Value {
    match gross {
        Gross::Gain(amount) => json!({ "state": "gain", "amount": dec(*amount) }),
        Gross::Even => json!({ "state": "even", "amount": "0" }),
        Gross::Loss(amount) => json!({ "state": "loss", "amount": dec(*amount) }),
    }
}

fn digest_of(text: &str) -> Value {
    json!({
        "bytes": text.len(),
        "keccak256": format!("{:#x}", keccak256(text.as_bytes())),
    })
}

// ---------------------------------------------------------------------------
// The independent arithmetic. Written here, on the published fields only.
// ---------------------------------------------------------------------------

/// One hop as the fold needs it: the two reserves the pool holds on either side of this
/// direction, and the retained fraction the pool is attested at.
struct FoldHop {
    pool: Address,
    reserve_in: U256,
    reserve_out: U256,
    fee: Option<Fee>,
}

fn fold_hops(route: &MultiHopRoute) -> Vec<FoldHop> {
    route
        .edges()
        .iter()
        .map(|hop| FoldHop {
            pool: hop.pool.address,
            reserve_in: hop.reserve_in,
            reserve_out: hop.reserve_out,
            fee: hop.fee,
        })
        .collect()
}

/// One hop of the constant-product curve, in this file's own words:
/// `out = in * numerator * reserve_out / (reserve_in * denominator + in * numerator)`.
///
/// The fee is taken on the input side because that is what the attestation means: a pool that
/// retains 997/1000 of what arrives is a pool that trades against `in * 997`. Division floors,
/// which is what the contract does; a refusal returns the reason rather than a number.
fn fold_one(hop: &FoldHop, amount_in: U256) -> Result<U256, String> {
    let name = format!("{:#x}", hop.pool);
    if hop.reserve_in.is_zero() || hop.reserve_out.is_zero() {
        return Err(format!("{name}: one side of the pool is empty"));
    }
    if amount_in.is_zero() {
        return Err(format!("{name}: a zero input buys nothing"));
    }
    let fee = hop
        .fee
        .ok_or_else(|| format!("{name}: no fee attestation, so nothing to trade against"))?;
    if fee.denominator == 0 || fee.numerator > fee.denominator {
        return Err(format!(
            "{name}: {} over {} is not a fraction of the input",
            fee.numerator, fee.denominator
        ));
    }
    let retained = amount_in
        .checked_mul(U256::from(fee.numerator))
        .ok_or_else(|| format!("{name}: the fee product leaves 256 bits"))?;
    let numerator = retained
        .checked_mul(hop.reserve_out)
        .ok_or_else(|| format!("{name}: the output product leaves 256 bits"))?;
    let denominator = hop
        .reserve_in
        .checked_mul(U256::from(fee.denominator))
        .ok_or_else(|| format!("{name}: the reserve product leaves 256 bits"))?
        .checked_add(retained)
        .ok_or_else(|| format!("{name}: the denominator leaves 256 bits"))?;
    Ok(numerator / denominator)
}

/// The whole route, hop by hop, feeding each output into the next input. A hop that floors to
/// zero ends the walk at zero rather than as an error: that is a market outcome, not a broken
/// calculator.
fn fold_route(hops: &[FoldHop], amount_in: U256) -> Result<Vec<U256>, String> {
    let mut outputs = Vec::with_capacity(hops.len());
    let mut carry = amount_in;
    for hop in hops {
        let out = fold_one(hop, carry)?;
        outputs.push(out);
        carry = out;
        if out.is_zero() {
            for _ in outputs.len()..hops.len() {
                outputs.push(U256::ZERO);
            }
            return Ok(outputs);
        }
    }
    Ok(outputs)
}

/// The rotation-invariant identity, computed here from the edge list rather than read off the
/// route: every rotation of a closed trail compared as a list of [`EdgeId`], smallest wins.
/// §43's route-identity row is this function against the route's own identity.
fn minimum_rotation(edges: &[EdgeId]) -> Vec<EdgeId> {
    let len = edges.len();
    let mut best: Vec<EdgeId> = edges.to_vec();
    for shift in 1..len {
        let rotation: Vec<EdgeId> = (shift..shift + len).map(|i| edges[i % len]).collect();
        if rotation < best {
            best = rotation;
        }
    }
    best
}

/// This file's own ordering of two round trips: more profit wins, `even` beats any loss and
/// loses to any gain, a smaller loss beats a larger one, and a tie goes to the smaller input.
/// The optimizer's rule is the same one; publishing it as a function of the two gross figures
/// is what lets a reader check which point the claimed best won on.
fn ranked_better(candidate: &Gross, incumbent: &Gross) -> bool {
    match (candidate, incumbent) {
        (Gross::Gain(a), Gross::Gain(b)) => a > b,
        (Gross::Gain(_), _) => true,
        (Gross::Even, Gross::Gain(_)) => false,
        (Gross::Even, Gross::Even) => false,
        (Gross::Even, Gross::Loss(_)) => true,
        (Gross::Loss(a), Gross::Loss(b)) => a < b,
        (Gross::Loss(_), _) => false,
    }
}

fn gross_of(input: U256, output: U256) -> Gross {
    match output.cmp(&input) {
        std::cmp::Ordering::Greater => Gross::Gain(output - input),
        std::cmp::Ordering::Equal => Gross::Even,
        std::cmp::Ordering::Less => Gross::Loss(input - output),
    }
}

// ---------------------------------------------------------------------------
// §43 pricing: one route, priced, with the fold beside it
// ---------------------------------------------------------------------------

fn edge_json(edge: &EdgeId) -> Value {
    json!({
        "pool": addr(edge.pool.address),
        "token_in": addr(edge.token_in.address),
        "token_out": addr(edge.token_out.address),
    })
}

fn hop_json(hop: &evm_opportunity::RouteHop) -> Value {
    json!({
        "pool": addr(hop.pool.address),
        "token_in": addr(hop.token_in.address),
        "token_out": addr(hop.token_out.address),
        "reserve_in": dec(hop.reserve_in),
        "reserve_out": dec(hop.reserve_out),
        "fee": opt_fee(hop.fee),
    })
}

fn quote_json(quote: &evm_opportunity::MultiHopQuote) -> Value {
    json!({
        "input": dec(quote.input),
        "output": dec(quote.output),
        "truncated": quote.truncated,
        "hops": quote.hops.iter().map(|hop| json!({
            "pool": addr(hop.pool.address),
            "token_in": addr(hop.token_in.address),
            "token_out": addr(hop.token_out.address),
            "amount_in": dec(hop.amount_in),
            "amount_out": dec(hop.amount_out),
            "fee": fee_json(&hop.fee),
        })).collect::<Vec<_>>(),
    })
}

/// The pricing row: the fields, the pipeline's answer, this file's fold of the same fields, and
/// the identity computed twice.
fn pricing_row(
    route: &MultiHopRoute,
    input: U256,
    state_source: &str,
    market: Value,
    what_this_is: &str,
) -> Value {
    let hops = fold_hops(route);
    let folded = fold_route(&hops, input);
    let priced = price(route, input);
    let published_quote = priced.quote().cloned();

    let identity_edges: Vec<Value> = route.identity().edges().iter().map(edge_json).collect();
    let trade_order: Vec<EdgeId> = route
        .edges()
        .iter()
        .map(|hop| {
            edge_on(
                route.chain_id(),
                hop.pool.address,
                hop.token_in.address,
                hop.token_out.address,
            )
        })
        .collect();
    let rotated = minimum_rotation(&trade_order);
    let identity_agrees = rotated
        .iter()
        .zip(route.identity().edges())
        .all(|(mine, its)| mine == its);

    let (fold_out, fold_error) = match &folded {
        Ok(outputs) => (
            Value::Array(outputs.iter().map(|v| Value::String(dec(*v))).collect()),
            Value::Null,
        ),
        Err(reason) => (Value::Null, Value::String(reason.clone())),
    };

    let quote_agrees = match (&published_quote, &folded) {
        (Some(quote), Ok(outputs)) => {
            quote.output == *outputs.last().expect("a route has at least one hop")
                && quote
                    .hops
                    .iter()
                    .zip(outputs.iter())
                    .all(|(hop, out)| hop.amount_out == *out)
        }
        (None, Err(_)) => true,
        _ => false,
    };

    json!({
        "what_this_is": what_this_is,
        "chain_id": route.chain_id().0,
        "target_block": route.target_block().0,
        "state_source": state_source,
        "market_claim": market,
        "route": {
            "hop_count": route.hop_count(),
            "input_token": addr(route.input_token().address),
            "pools": route.pools().iter().map(|p| addr(p.address)).collect::<Vec<_>>(),
            "tokens": route.tokens().iter().map(|t| addr(t.address)).collect::<Vec<_>>(),
            "is_priceable": route.is_priceable(),
            "input_upper_bound": opt_dec(route.input_upper_bound()),
            "unattested": match route.unattested() {
                Some((pool, token)) => json!({
                    "pool": addr(pool.address),
                    "token": addr(token.address),
                }),
                None => Value::Null,
            },
        },
        "hops": route.edges().iter().map(hop_json).collect::<Vec<_>>(),
        "published": {
            "price_state": priced.state(),
            "input": dec(input),
            "quote": published_quote.as_ref().map(quote_json).unwrap_or(Value::Null),
            "gross": priced.gross().map(|g| gross_json(&g)).unwrap_or(Value::Null),
            "identity": format!("{:?}", route.identity()),
            "identity_edges": identity_edges.clone(),
        },
        "recomputed_independently": {
            "method": "this file's own constant-product fold over the published hops array: \
                       in*numerator*reserve_out / (reserve_in*denominator + in*numerator), floor \
                       division, each output feeding the next input",
            "input": dec(input),
            "hop_outputs": fold_out,
            "output": folded.as_ref().ok().map(|o| dec(*o.last().expect("a route has a hop"))).map(Value::String).unwrap_or(Value::Null),
            "refused_because": fold_error,
            "identity_method": "minimum rotation of the published trade-order edge list, compared \
                                as a Vec<EdgeId> with EdgeId's own Ord",
            "identity_edges": rotated.iter().map(edge_json).collect::<Vec<_>>(),
            "identity_matches": identity_agrees,
        },
        "agrees": {
            "quote_with_the_fold": quote_agrees,
            "identity_with_the_rotation_minimum": identity_agrees,
        },
    })
}

// ---------------------------------------------------------------------------
// §43 optimizer: a search, its domain, and a scan of that domain from the fields
// ---------------------------------------------------------------------------

/// The grid the optimizer says it walked, rebuilt from the published domain and policy alone. A
/// width at or under the exhaustive limit is walked one input at a time; wider is sampled on a
/// uniform grid of `ceil(width / coarse_points)` and then refined over `best ± refine_span`.
///
/// The anchor of that refinement is rebuilt here rather than read off the search's answer: the
/// window sits around the best *grid* point, which is not the same point as the best point of the
/// whole search. Anchoring on the final best would slide the window by up to one span and let this
/// file price inputs the search never saw — an agreement check that quietly compares two different
/// sets.
struct RebuiltGrid {
    inputs: Vec<U256>,
    coarse_points: usize,
    anchor: Option<U256>,
}

fn rebuilt_grid(
    domain_min: U256,
    domain_max: U256,
    policy: &OptimizationPolicy,
    strategy_is_exhaustive: bool,
    hops: &[FoldHop],
) -> RebuiltGrid {
    let width = domain_max - domain_min + U256::ONE;
    if strategy_is_exhaustive {
        let count = usize::try_from(width).expect("a published exhaustive domain fits in a usize");
        return RebuiltGrid {
            inputs: (0..count)
                .map(|step| domain_min + U256::from(step))
                .collect(),
            coarse_points: count,
            anchor: None,
        };
    }
    let coarse = U256::from(policy.coarse_points);
    let step = (width + coarse - U256::ONE) / coarse;
    let mut inputs = Vec::new();
    let mut input = domain_min;
    while (inputs.len() as u64) < policy.coarse_points && input <= domain_max {
        inputs.push(input);
        input += step;
    }
    let coarse_points = inputs.len();

    // This file's own best grid point, ranked by its own fold before any window exists.
    let mut best: Option<(U256, Gross)> = None;
    for candidate in &inputs {
        let Ok(outputs) = fold_route(hops, *candidate) else {
            continue;
        };
        let output = *outputs.last().expect("a route has at least one hop");
        let gross = gross_of(*candidate, output);
        let take = match &best {
            None => true,
            Some((best_input, best_gross)) => {
                ranked_better(&gross, best_gross)
                    || (gross == *best_gross && *candidate < *best_input)
            }
        };
        if take {
            best = Some((*candidate, gross));
        }
    }
    let anchor = best.map(|(input, _)| input);

    // A grid with nothing priced on it has no window to refine around, which is the same rule the
    // search states at multi_optimizer.rs:286.
    if let Some(anchor) = anchor {
        let span = U256::from(policy.refine_span);
        let low = anchor.saturating_sub(span).max(domain_min);
        let high = anchor.saturating_add(span).min(domain_max);
        let window = usize::try_from(high - low + U256::ONE).expect("a refinement window fits");
        for offset in 0..window {
            inputs.push(low + U256::from(offset));
        }
    }
    RebuiltGrid {
        inputs,
        coarse_points,
        anchor,
    }
}

/// One optimizer row: the search as it reported itself, and this file's scan of the same inputs
/// through the same published fields.
fn optimizer_row(
    route: &MultiHopRoute,
    search_min: U256,
    search_max: U256,
    policy: OptimizationPolicy,
    state_source: &str,
    what_this_is: &str,
) -> Value {
    let hops = fold_hops(route);
    let searched = optimize(route, search_min, search_max, policy)
        .unwrap_or_else(|error| panic!("{what_this_is}: the search refused: {error}"));
    let result = &searched.search;
    let exhaustive = result.strategy == evm_opportunity::OptimizationStrategy::Exhaustive;

    // The independent scan over the rebuilt grid. For the exhaustive branch the grid *is* the
    // domain, so this is a brute-force oracle in §43's sense; for the sampled branch it is the
    // same set the pipeline compared, re-priced and re-ranked here.
    let rebuilt = rebuilt_grid(
        result.domain_min,
        result.domain_max,
        &result.policy,
        exhaustive,
        &hops,
    );
    let coarse_points = rebuilt.coarse_points;
    let anchor = rebuilt.anchor;
    let grid = rebuilt.inputs;
    let mut scan_best: Option<(U256, U256, Gross)> = None;
    let mut curve: Vec<Value> = Vec::new();
    let mut refused_points = 0usize;
    for input in &grid {
        match fold_route(&hops, *input) {
            Ok(outputs) => {
                let output = *outputs.last().expect("a route has at least one hop");
                let gross = gross_of(*input, output);
                let take = match &scan_best {
                    None => true,
                    Some((best_input, _, best_gross)) => {
                        ranked_better(&gross, best_gross)
                            || (gross == *best_gross && *input < *best_input)
                    }
                };
                if take {
                    scan_best = Some((*input, output, gross));
                }
                curve.push(json!({
                    "input": dec(*input),
                    "output": dec(output),
                    "gross": gross_json(&gross),
                }));
            }
            Err(_) => refused_points += 1,
        }
    }
    let (scan_input, scan_output, scan_profit) = scan_best.map_or(
        (Value::Null, Value::Null, Value::Null),
        |(input, output, gross)| {
            (
                Value::String(dec(input)),
                Value::String(dec(output)),
                gross_json(&gross),
            )
        },
    );

    json!({
        "what_this_is": what_this_is,
        "chain_id": route.chain_id().0,
        "target_block": route.target_block().0,
        "state_source": state_source,
        "route_identity": format!("{:?}", route.identity()),
        "hops": route.edges().iter().map(hop_json).collect::<Vec<_>>(),
        "published": {
            "search_min": dec(search_min),
            "search_max": dec(search_max),
            "domain_min": dec(result.domain_min),
            "domain_max": dec(result.domain_max),
            "domain_width": dec(result.domain_max - result.domain_min + U256::ONE),
            "strategy": format!("{:?}", result.strategy).to_lowercase(),
            "termination": format!("{:?}", result.termination).to_lowercase(),
            "policy": {
                "coarse_points": result.policy.coarse_points,
                "refine_span": result.policy.refine_span,
                "exhaustive_limit": result.policy.exhaustive_limit,
            },
            "evaluations": result.evaluations,
            "refusals": result.refusals,
            "best_input": dec(result.best_input),
            "best_output": dec(result.best_output),
            "best_profit": dec(result.best_profit),
            "gross": gross_json(&result.gross),
            "covered_the_route_domain": result.covered_the_route_domain(route),
            "covered_the_route_domain_means": "this flag can only be true for an exhaustive walk \
                                               that reached the route's own input ceiling; on a \
                                               sampled search the answer is that the search saw a \
                                               grid, not the domain — it is a statement about \
                                               coverage, not a failure",
            "strategy_this_run_took": format!("{:?}", result.strategy).to_lowercase(),
            "curve_this_file_asked_the_pipeline_for": curve_of(route, &grid),
        },
        "recomputed_independently": {
            "method": "this file re-ranked its own fold of every input in the grid the published \
                       domain and policy describe; the grid is rebuilt from those two fields, not \
                       taken from the search, and the refinement window is anchored on this file's \
                       own best grid point rather than on the search's answer",
            "grid_points_rebuilt": grid.len(),
            "coarse_points_rebuilt": coarse_points,
            "refinement_anchor_this_file_found": anchor.map(dec).map(Value::String).unwrap_or(Value::Null),
            "points_the_fold_refused": refused_points,
            "best_input": scan_input,
            "best_output": scan_output,
            "best_profit": scan_profit,
            "curve": curve,
        },
        "agrees": {
            "best_input": scan_best.as_ref().is_some_and(|(i, _, _)| *i == result.best_input),
            "best_output": scan_best.as_ref().is_some_and(|(_, o, _)| *o == result.best_output),
            "every_rebuilt_point_was_attempted": grid.len() as u64 == result.evaluations + result.refusals,
        },
    })
}

/// The pipeline's own answer at each grid point, so the two curves can be compared point by
/// point rather than only at their maxima.
fn curve_of(route: &MultiHopRoute, grid: &[U256]) -> Vec<Value> {
    grid.iter()
        .map(|input| match price(route, *input) {
            Price::Quoted { quote, gross } => json!({
                "input": dec(*input),
                "output": dec(quote.output),
                "gross": gross_json(&gross),
            }),
            other => json!({
                "input": dec(*input),
                "refused": other.state(),
            }),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// §43 simulation: the request, the run, and the bytes both sides can hash
// ---------------------------------------------------------------------------

fn call_json(call: &evm_protocol::ExecutorCall) -> Value {
    match call {
        evm_protocol::ExecutorCall::Execute {
            legs,
            input_token,
            amount_in,
            min_final_amount,
            recipient,
        } => json!({
            "signature": call.signature(),
            "selector": bytes_hex(&call.selector()),
            "legs": legs.iter().map(|leg| json!({
                "pool": addr(leg.pool),
                "token_in": addr(leg.token_in),
                "token_out": addr(leg.token_out),
                "amount_in": dec(leg.amount_in),
                "amount_out": dec(leg.amount_out),
                "min_amount_out": dec(leg.min_amount_out),
            })).collect::<Vec<_>>(),
            "input_token": addr(*input_token),
            "amount_in": dec(*amount_in),
            "min_final_amount": dec(*min_final_amount),
            "recipient": addr(*recipient),
        }),
        other => json!({ "signature": other.signature(), "other": format!("{other:?}") }),
    }
}

/// The gas model as fields, not as one `{:?}` word. A gate in another crate has to rebuild the
/// run it is re-executing, and a `Debug` string is not a reconstruction — §43's independence is
/// only available to a reader who can put the number back. The `Debug` form stays beside it so the
/// published figures can still be read against the words the run itself printed.
fn pricing_json(pricing: &GasPricing) -> Value {
    match pricing {
        GasPricing::Eip1559 {
            priority_fee_per_gas,
            provenance,
        } => json!({
            "kind": "eip1559",
            "priority_fee_per_gas": priority_fee_per_gas.to_string(),
            "provenance": provenance,
        }),
        GasPricing::Legacy {
            gas_price,
            provenance,
        } => json!({
            "kind": "legacy",
            "gas_price": gas_price.to_string(),
            "provenance": provenance,
        }),
        GasPricing::Unresolved { reason } => json!({
            "kind": "unresolved",
            "reason": reason,
        }),
    }
}

fn run_spec_json(spec: &ExecutorRun) -> Value {
    json!({
        "chain_id": spec.chain_id.0,
        "priced_at_block": spec.priced_at.0,
        "state_source": spec.state_source,
        "executor": addr(spec.executor),
        "operator": addr(spec.operator),
        "recipient": addr(spec.operator),
        "gas_limit": spec.gas_limit,
        "rules": format!("{:?}", spec.rules),
        "pricing": pricing_json(&spec.pricing),
        "pricing_debug": format!("{:?}", spec.pricing),
        "endowment": opt_dec(spec.endowment),
        "call": call_json(&spec.call),
        "calldata": bytes_hex(spec.call.encode().as_ref()),
        "calldata_len": spec.call.encode().len(),
    })
}

fn file_digest(root: &Path, relative: &str) -> Value {
    let bytes = std::fs::read(root.join(relative))
        .unwrap_or_else(|error| panic!("reading {relative}: {error}"));
    json!({
        "bytes": bytes.len(),
        "keccak256": format!("{:#x}", keccak256(&bytes)),
    })
}

/// The state the declared triangle ran on, committed the way M10 committed its own fixture. A gate
/// in another crate can only re-run the EVM if the dump it ran on is on disk under a name the
/// evidence quotes; an in-memory composition nobody can load is not a reproducible state.
const CYCLE_FIXTURE: &str = "fixtures/simulation-m11/fixture-37530593-triangle.json";
const CYCLE_ADDITIONS: &str = "fixtures/simulation-m11/fixture-37530593-triangle-additions.json";

/// One run's state, in the form §43's outside gate loads it. The published file is checked against
/// the bytes this assembly actually ran with, so "this is the state" is a comparison here rather
/// than a claim downstream.
fn state_json(fx: &Fixture, fixture_file: &str, additions_file: &str) -> Value {
    let used = fx.fixture_bytes();
    let path = workspace_root().join(fixture_file);
    let on_disk = std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}\nThe committed fixture is the state these rows ran on. It is written by \
             the ignored setup test:\n    cargo test -p evm-simulation --test multihop_evidence \
             -- --ignored --test-threads=1",
            path.display()
        )
    });
    assert_eq!(
        used, on_disk,
        "{fixture_file} differs from the state this assembly used"
    );
    json!({
        "fixture_file": fixture_file,
        "fixture": {
            "bytes": on_disk.len(),
            "keccak256": format!("{:#x}", keccak256(&on_disk)),
        },
        "additions_file": additions_file,
        "additions_digest": file_digest(workspace_root().as_path(), additions_file),
        "base_recording": executor_state::RECORDED,
        "state_source_string": fx.source,
        "how_to_load": "read fixture_file as an evm_simulation::state::StateDump, hand it to \
                        DumpStateProvider::new with the state_source_string above, and run the \
                        published calldata bytes against it — the run's only other state edit is \
                        the endowment ExecutorRun::setup layers from the published spec, which is \
                        the one edit §58 allows and it is named by the spec, not by this file",
    })
}

/// The triangle's declared rows beside the base fixture's, in the table form M10's additions file
/// uses: every row carries what it is and why it is scaffolding rather than a market fact.
fn cycle_additions_text(cycle: &Cycle) -> String {
    let value = json!({
        "fixture": CYCLE_FIXTURE,
        "base_fixture": executor_state::FIXTURE,
        "base_additions": executor_state::FIXTURE_ADDITIONS,
        "recorded": executor_state::RECORDED,
        "label": executor_state::FIXTURE_LABEL,
        "accounts": cycle
            .fx
            .accounts
            .iter()
            .chain(cycle.added_accounts.iter())
            .map(|row| serde_json::to_value(row).expect("a declared account serializes"))
            .collect::<Vec<_>>(),
        "words": cycle
            .fx
            .additions
            .iter()
            .chain(cycle.added_words.iter())
            .map(|row| serde_json::to_value(row).expect("a declared word serializes"))
            .collect::<Vec<_>>(),
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&value).expect("the additions serialize")
    )
}

/// §44: what served this run, measured rather than asserted.
fn rpc_count(state: &Value) -> Value {
    let endpoint_present = std::env::var("GIWA_RPC_URL").is_ok();
    json!({
        "rpc_count": 0,
        "basis": {
            "provider_type": std::any::type_name::<DumpStateProvider>(),
            "constructed_from": state["fixture_file"],
            "state_source_string": state["state_source_string"],
            "endpoint_variable": {
                "name": "GIWA_RPC_URL",
                "present_in_the_assembling_process": endpoint_present,
                "why_it_matters": "the only provider in this workspace that holds a URL reads it \
                                  from this variable; the runs above are served by the type named \
                                  in provider_type, which takes a dump file and a label and \
                                  answers from memory"
            },
            "local_reads_served": "every read answered from the dump or was refused; a refusal \
                                  here would have failed the run rather than costing a request",
            "hot_path": "M11 added no read to a hot path: the milestone's production diff is \
                         audited in §49's gate, and this directory is assembled offline",
        },
    })
}

fn observed_json(outcome: &ExecutorOutcome) -> Value {
    json!({
        "status": format!("{:?}", outcome.status),
        "succeeded": outcome.succeeded(),
        "delivered": opt_dec(outcome.delivered),
        "gas_used": outcome.gas_used,
        "gas_limit": outcome.gas_limit,
        "charge": format!("{:?}", outcome.charge),
        "contract_error": outcome.contract_error.clone(),
        "revert_kind": outcome.revert_kind,
        "revert_reason": outcome.revert().map(|data| data.reason().to_string()),
        "market_moved": outcome.market_moved(),
        "logs": outcome.logs.len(),
        "reserve_rows": outcome.reserves.iter().map(|row| json!({
            "pool": addr(row.pool),
            "before": [dec(row.before.reserve0), dec(row.before.reserve1)],
            "after": [dec(row.after.reserve0), dec(row.after.reserve1)],
            "changed": row.changed(),
        })).collect::<Vec<_>>(),
        "balance_rows": outcome.balances.iter().map(|row| json!({
            "token": addr(row.token),
            "holder": addr(row.holder),
            "before": dec(row.before),
            "after": dec(row.after),
            "movement": format!("{:?}", row.movement()),
        })).collect::<Vec<_>>(),
        "changed_slots": outcome.state_changes.slots.len(),
    })
}

/// The simulation row: the spec that ran, the answer the EVM gave, and the two hashes this file
/// re-derives from the published bytes.
fn simulation_row(
    state: &Value,
    priced: &OptimizedCandidate,
    spec: &ExecutorRun,
    simulated: &SimulatedOpportunity,
    what_this_is: &str,
) -> Value {
    let encoded = spec.call.encode();
    let calldata_hash = keccak256(encoded.as_ref());
    let quoted: Vec<U256> = priced
        .search
        .quote
        .hops
        .iter()
        .map(|hop| hop.amount_out)
        .collect();
    let recomputed_identity_hash = keccak256(simulated.canonical_text().as_bytes());
    let agrees = simulated.route_agrees_with_pricing();

    let mut row = json!({});
    row["rpc"] = rpc_count(state);
    row["what_this_is"] = json!(what_this_is);
    row["published"] = json!({
        "state": state.clone(),
        "spec": run_spec_json(spec),
        "observed": observed_json(&simulated.outcome),
        "simulated_opportunity": {
            "status": simulated.status.name(),
            "identity": simulated.identity(),
            "identity_hash": hash(simulated.identity_hash()),
            "canonical_text": simulated.canonical_text(),
            "chain_id": simulated.chain_id.0,
            "simulation_block": simulated.simulation_block.number.0,
            "simulation_block_hash": format!("{:#x}", simulated.simulation_block.hash),
            "input_amount": dec(simulated.input_amount),
            "final_amount": opt_dec(simulated.final_amount),
            "gross": simulated.gross.map(|g| gross_json(&g)),
            "gas_used": simulated.gas_used,
            "gas_charge": format!("{:?}", simulated.gas_charge),
            "min_final_output": dec(simulated.min_final_output),
            "calldata": bytes_hex(simulated.calldata().as_ref()),
            "calldata_hash": hash(simulated.calldata_hash()),
            "route_agrees_with_pricing": match &agrees {
                Ok(()) => "agrees".to_string(),
                Err(refusal) => format!("refuses: {refusal}"),
            },
            "matches_pricing": simulated.matches_pricing(),
            "market_moved": simulated.market_moved(),
        },
        "pricing_quotes_the_same_route": {
            "quoted_hop_outputs": quoted.iter().map(|v| dec(*v)).collect::<Vec<_>>(),
            "delivered": opt_dec(simulated.final_amount),
            "priced_against_delivered": simulated
                .priced_against_delivered()
                .map(|g| gross_json(&g))
                .unwrap_or(Value::Null),
        },
    });
    row["recomputed_independently"] = json!({
        "calldata_hash_of_the_published_bytes": format!("{:#x}", calldata_hash),
        "calldata_hash_agrees": calldata_hash == simulated.calldata_hash(),
        "identity_hash_of_the_published_canonical_text": format!("{:#x}", recomputed_identity_hash),
        "identity_hash_agrees": recomputed_identity_hash == simulated.identity_hash(),
        "method": "keccak256 taken over the published request calldata and over the published \
                   canonical text, in this file rather than by calling the simulation's own \
                   hash methods",
    });
    row["agrees"] = json!({
        "calldata_hash_with_the_published_figure": format!("{:#x}", calldata_hash)
            == hash(simulated.calldata_hash()),
        "request_bytes_with_the_recorded_bytes": encoded.as_ref() == simulated.calldata().as_ref(),
    });
    row
}

// ---------------------------------------------------------------------------
// §43 risk: the figures, the thresholds, and the comparisons they decide
// ---------------------------------------------------------------------------

fn risk_row(
    case: &str,
    simulated: &SimulatedOpportunity,
    policy: &MultihopRiskPolicy,
    facts: &MarketFacts,
    decision: &MultihopRiskDecision,
    figures: Option<&MultihopAcceptance>,
    what_this_is: &str,
) -> Value {
    let simulation_age = facts
        .head
        .map(|head| head.0.saturating_sub(simulated.simulation_block.number.0));
    let state_age = facts.state_version.map(|version| {
        version
            .0
            .saturating_sub(simulated.simulation_block.number.0)
    });
    let delivered = simulated.final_amount.unwrap_or(U256::ZERO);
    let gross_by_arithmetic = delivered.saturating_sub(simulated.input_amount);

    json!({
        "what_this_is": what_this_is,
        "case": case,
        "route_identity": format!("{:?}", simulated.candidate.route.identity()),
        "input_amount": dec(simulated.input_amount),
        "policy": {
            "minimum_gross_profit": dec(policy.minimum_gross_profit),
            "maximum_gas": policy.maximum_gas,
            "maximum_simulation_age": policy.maximum_simulation_age,
            "maximum_state_age": policy.maximum_state_age,
            "executor": addr(policy.executor),
            "chain_id": policy.chain_id.0,
            "provenance": policy.provenance,
        },
        "market_facts": {
            "head": facts.head.map(|h| h.0),
            "state_version": facts.state_version.map(|v| v.0),
            "provenance": facts.provenance,
        },
        "run": {
            "status": simulated.status.name(),
            "final_amount": opt_dec(simulated.final_amount),
            "gas_used": simulated.gas_used,
            "gas_charge": format!("{:?}", simulated.gas_charge),
            "simulation_block": simulated.simulation_block.number.0,
            "min_final_output": dec(simulated.min_final_output),
            // §43's two comparisons that need an identity: `executor_matches` and `chain_matches`
            // compare the run against the policy, so a checker in another crate needs the run's
            // own chain id and executor, not only the policy's. Without these the row's
            // `recomputed_independently` block would have two entries nothing can be recomputed
            // from, and the gate would either skip them or read them off the policy twice.
            "chain_id": simulated.chain_id.0,
            "executor": addr(simulated.run.executor),
        },
        "published": {
            "decision": decision.name(),
            "accepted": decision.accepted(),
            "check": decision.check().map(|c| format!("{c:?}")),
            "reason": decision.reason().map(|r| r.describe()),
            "detail": decision.detail(),
            "figures": figures.map(|f| json!({
                "input_amount": dec(f.input_amount),
                "delivered": dec(f.delivered),
                "gross_profit": dec(f.gross_profit),
                "minimum_gross_profit": dec(f.minimum_gross_profit),
                "min_final_output": dec(f.min_final_output),
                "gas_used": f.gas_used,
                "maximum_gas": f.maximum_gas,
                "gas_charge_wei": f.gas_charge_wei.map(dec),
                "simulation_block": f.simulation_block.0,
                "head": f.head.map(|h| h.0),
                "state_version": f.state_version.map(|v| v.0),
                "detail": f.detail.clone(),
            })),
        },
        "recomputed_independently": {
            "method": "the six comparisons §26–§29 make, applied by this file to the figures \
                       published above",
            "gross_profit": dec(gross_by_arithmetic),
            "clears_the_profit_floor": gross_by_arithmetic >= policy.minimum_gross_profit,
            "within_the_gas_ceiling": simulated.gas_used <= policy.maximum_gas,
            "simulation_age": simulation_age,
            "within_the_simulation_age": simulation_age
                .is_some_and(|a| a <= policy.maximum_simulation_age),
            "state_age": state_age,
            "within_the_state_age": state_age.is_some_and(|a| a <= policy.maximum_state_age),
            "executor_matches": simulated.run.executor == policy.executor,
            "chain_matches": simulated.chain_id == policy.chain_id,
            "guard_is_met_by_the_delivery": simulated
                .final_amount
                .is_some_and(|d| d >= simulated.min_final_output),
        },
    })
}

// ---------------------------------------------------------------------------
// §43 controlled chain: every stage's answer, and the hashes under one binding
// ---------------------------------------------------------------------------

fn plan_json(plan: &ExecutablePlan) -> Value {
    let raw = plan.plan();
    json!({
        "plan_hash": hash(plan.plan_hash()),
        "calldata": bytes_hex(plan.calldata().as_ref()),
        "calldata_hash": hash(plan.calldata_hash()),
        "route_id": plan.route_id(),
        "canonical_text": raw.canonical_text(),
        "fields": {
            "chain_id": raw.chain_id,
            "executor": addr(raw.executor),
            "sender": addr(raw.sender),
            "recipient": addr(raw.recipient),
            "input_token": addr(raw.input_token),
            "input_amount": dec(raw.input_amount),
            "legs": raw.legs.len(),
            "min_final_output": dec(raw.min_final_output),
            "validity": {
                "simulated_at_block": raw.validity.simulated_at_block.0,
                "max_block_age": raw.validity.max_block_age,
                "provenance": raw.validity.provenance,
            },
            "simulation": {
                "correlation_id": raw.simulation.correlation_id,
                "block_number": raw.simulation.block_number.0,
                "block_hash": format!("{:#x}", raw.simulation.block_hash),
                "state_fingerprint": raw.simulation.state_fingerprint,
                "simulation_id": format!("{:#x}", raw.simulation.simulation_id),
                "outcome": raw.simulation.outcome.name(),
                "funding": format!("{:?}", raw.simulation.funding),
                "market": raw.simulation.market.name(),
                "market_evidence": raw.simulation.market.evidence(),
            },
            "profit": {
                "denomination": format!("{:?}", raw.profit.denomination),
                "required_final_balance": dec(raw.profit.required_final_balance),
                "provenance": raw.profit.provenance,
            },
            "leg_rows": raw.legs.iter().enumerate().map(|(index, leg)| json!({
                "index": index,
                "pool": addr(leg.pool),
                "token_in": addr(leg.token_in),
                "token_out": addr(leg.token_out),
                "amount_in": dec(leg.amount_in),
                "amount_out": dec(leg.amount_out),
                "min_amount_out": dec(leg.min_amount_out),
                "derivation": leg.derivation.name(),
            })).collect::<Vec<_>>(),
        },
        "token_transitions": raw.token_transitions().iter().map(|a| addr(*a)).collect::<Vec<_>>(),
    })
}

fn binding_json(binding: &MultihopBinding) -> Value {
    json!({
        "simulation_id": binding.simulation_id,
        "simulation_calldata_hash": hash(binding.simulation_calldata_hash),
        "execution_calldata_hash": hash(binding.execution_calldata_hash),
        "plan_hash": hash(binding.plan_hash),
        "priced_route_id": binding.priced_route_id,
        "simulated_route_id": binding.simulated_route_id,
        "execution_route_id": binding.execution_route_id,
        "bound": binding.bound(),
    })
}

fn candidate_json(candidate: &CycleCandidate) -> Value {
    json!({
        "chain_id": candidate.chain_id.0,
        "target_block": candidate.target_block.0,
        "start_token": addr(candidate.start_token.address),
        "hop_count": candidate.hop_count,
        "edges": candidate.edges.iter().map(edge_json).collect::<Vec<_>>(),
        "canonical_key": format!("{:?}", candidate.canonical_key),
        "fee_status": format!("{:?}", candidate.fee_status),
        "identity_recomputed_from_the_edges": minimum_rotation(&candidate.edges)
            .iter()
            .map(edge_json)
            .collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// §43 lanes: the ledger, driven and published
// ---------------------------------------------------------------------------

fn lane_json(lane: &CandidateLane) -> Value {
    json!({
        "lane_id": lane.lane_id.0,
        "candidate_id": lane.candidate_id,
        "state": lane.state.code(),
        "plan_hash": lane.plan_hash.map(hash),
        "simulation_id": lane.simulation_id.clone(),
        "nonce": lane.nonce_reservation.as_ref().map(|r| json!({
            "signer": addr(r.signer),
            "nonce": r.nonce,
            "stage": format!("{:?}", r.stage).to_lowercase(),
        })),
        "capital": lane.capital_reservation.as_ref().map(|r| json!({
            "domain_id": r.domain_id,
            "amount": dec(r.amount),
        })),
        "history": lane.history.iter().map(|m| json!({
            "from": m.from.code(),
            "to": m.to.code(),
            "note": m.note,
        })).collect::<Vec<_>>(),
    })
}

fn capital_json(domain: &CapitalDomain) -> Value {
    json!({
        "domain_id": domain.domain_id,
        "capacity": dec(domain.capacity),
        "available_capital": dec(domain.available_capital),
        "reserved_capital": dec(domain.reserved_capital),
        "settled_input": dec(domain.settled_input()),
        "available_plus_reserved": dec(domain.available_capital + domain.reserved_capital),
        "invariant_holds": domain.invariant_holds(),
        "sums_to_capacity_recomputed": domain.available_capital + domain.reserved_capital
            == domain.capacity,
    })
}

/// The ledger this file drives: §33's three parallel lanes, one winner, one reservation, one
/// settle, and each of §46's lane controls beside its positive arm.
fn lane_rows() -> Value {
    let capacity = U256::from(1_000u64);
    let input = U256::from(100u64);
    let signer = Address::from_slice(&[0x6bu8; 20]);
    let plan_a = B256::left_padding_from(&[0xa1]);
    let plan_b = B256::left_padding_from(&[0xb2]);
    let plan_c = B256::left_padding_from(&[0xc3]);
    let plan_d = B256::left_padding_from(&[0xd4]);
    let plan_f = B256::left_padding_from(&[0xf5]);

    let mut ledger = LaneLedger::new("m11-evidence-domain", capacity);
    let mut steps: Vec<Value> = Vec::new();

    let open = |ledger: &mut LaneLedger, candidate: &str, steps: &mut Vec<Value>| {
        let lane_id = ledger.open(candidate).expect("a fresh candidate opens");
        steps.push(json!({
            "action": "open",
            "lane_id": lane_id.0,
            "candidate_id": candidate,
            "lane": lane_json(ledger.lane(lane_id).expect("just opened")),
            "capital": capital_json(ledger.capital()),
        }));
        lane_id
    };
    let begin = |ledger: &mut LaneLedger, lane_id: LaneId, steps: &mut Vec<Value>| {
        ledger
            .begin_simulation(lane_id)
            .expect("Created to Simulating");
        steps.push(json!({
            "action": "begin_simulation",
            "lane_id": lane_id.0,
            "lane": lane_json(ledger.lane(lane_id).expect("here")),
        }));
    };
    let record = |ledger: &mut LaneLedger,
                  lane_id: LaneId,
                  sim: &str,
                  plan_hash: B256,
                  steps: &mut Vec<Value>| {
        ledger
            .record_simulation(lane_id, sim.to_string(), plan_hash)
            .expect("the run attaches");
        steps.push(json!({
            "action": "record_simulation",
            "lane_id": lane_id.0,
            "simulation_id": sim,
            "plan_hash": hash(plan_hash),
            "lane": lane_json(ledger.lane(lane_id).expect("here")),
        }));
    };
    let advance = |ledger: &mut LaneLedger,
                   lane_id: LaneId,
                   next: LaneState,
                   note: &str,
                   steps: &mut Vec<Value>| {
        ledger.advance(lane_id, next, note).expect("a legal arrow");
        steps.push(json!({
            "action": "advance",
            "lane_id": lane_id.0,
            "to": next.code(),
            "note": note,
            "lane": lane_json(ledger.lane(lane_id).expect("here")),
        }));
    };
    // Every ranking this ledger is walked through is published as a step, not just the first one.
    // `final.winner` is the *last* selection the ledger made, so a gate that could only see one
    // `choose_winner` step could not recompute the field it is asked to check.
    let choose = |ledger: &mut LaneLedger,
                  standings: &[LaneStanding],
                  arm: &str,
                  steps: &mut Vec<Value>|
     -> LaneId {
        let winner = ledger
            .choose_winner(standings)
            .expect("a ranking over accepted lanes");
        steps.push(json!({
            "action": "choose_winner",
            "arm": arm,
            "standings": standings.iter().map(|s| json!({
                "lane_id": s.lane_id.0,
                "gross_gain": dec(s.gross_gain),
            })).collect::<Vec<_>>(),
            "winner": winner.0,
            "rule": "greater gross gain, then the lower lane id",
        }));
        winner
    };

    let a = open(&mut ledger, "m11-evidence-cand-a", &mut steps);
    let b = open(&mut ledger, "m11-evidence-cand-b", &mut steps);
    let c = open(&mut ledger, "m11-evidence-cand-c", &mut steps);
    for lane_id in [a, b, c] {
        begin(&mut ledger, lane_id, &mut steps);
    }
    steps.push(json!({
        "assertion": "three_lanes_simulate_in_parallel",
        "simulating_lanes": ledger.simulating().iter().map(|l| l.0).collect::<Vec<_>>(),
        "lanes_holding_a_nonce": ledger.nonces().outstanding(),
        "capital": capital_json(ledger.capital()),
    }));
    record(&mut ledger, a, "m11-evidence-sim-a", plan_a, &mut steps);
    record(&mut ledger, b, "m11-evidence-sim-b", plan_b, &mut steps);
    record(&mut ledger, c, "m11-evidence-sim-c", plan_c, &mut steps);
    for lane_id in [a, b, c] {
        advance(
            &mut ledger,
            lane_id,
            LaneState::RiskChecking,
            "risk asked",
            &mut steps,
        );
        advance(
            &mut ledger,
            lane_id,
            LaneState::Ready,
            "risk accepted",
            &mut steps,
        );
    }

    // §36: a total order over the standings, then §34/§35's reservation for the winner alone.
    let standings = vec![
        LaneStanding::new(c, U256::from(30u64)),
        LaneStanding::new(a, U256::from(90u64)),
        LaneStanding::new(b, U256::from(50u64)),
    ];
    let winner = choose(
        &mut ledger,
        &standings,
        "§36's ranking over the three accepted lanes",
        &mut steps,
    );
    assert_eq!(
        winner, a,
        "the standing with the greatest gain is lane-0, whose reservation follows"
    );
    let loser_refusal = ledger
        .reserve_for(b, signer, 0, input)
        .expect_err("a lane that did not win is turned away before it holds anything");
    steps.push(json!({
        "control": "nc36_a_losing_lane_is_turned_away",
        "refusal_code": loser_refusal.code(),
        "refusal": format!("{loser_refusal}"),
        "lane_after": lane_json(ledger.lane(b).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));
    let pair = ledger
        .reserve_for(a, signer, 0, input)
        .expect("the winner reserves");
    steps.push(json!({
        "action": "reserve_for",
        "lane_id": a.0,
        "nonce": json!({ "signer": addr(pair.nonce.signer), "nonce": pair.nonce.nonce, "stage": format!("{:?}", pair.nonce.stage).to_lowercase() }),
        "capital": json!({ "domain_id": pair.capital.domain_id, "amount": dec(pair.capital.amount) }),
        "lane_after": lane_json(ledger.lane(a).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));
    for next in [
        LaneState::Submitting,
        LaneState::Submitted,
        LaneState::Included,
    ] {
        advance(&mut ledger, a, next, "in flight", &mut steps);
    }
    ledger.settle(a).expect("the winner settles");
    steps.push(json!({
        "action": "settle",
        "lane_id": a.0,
        "lane_after": lane_json(ledger.lane(a).expect("here")),
        "capital_after": capital_json(ledger.capital()),
        "nonce_outstanding": ledger.nonces().outstanding(),
    }));

    // NC12: a settled lane spent its number; the next candidate takes the one after it.
    let again = open(&mut ledger, "m11-evidence-cand-d", &mut steps);
    begin(&mut ledger, again, &mut steps);
    record(&mut ledger, again, "m11-evidence-sim-d", plan_d, &mut steps);
    advance(
        &mut ledger,
        again,
        LaneState::RiskChecking,
        "risk asked",
        &mut steps,
    );
    advance(
        &mut ledger,
        again,
        LaneState::Ready,
        "risk accepted",
        &mut steps,
    );
    let second_winner = choose(
        &mut ledger,
        &[LaneStanding::new(again, U256::from(10u64))],
        "§46's NC12 arm: the only Ready lane left is the one after the settled pair",
        &mut steps,
    );
    let spent = ledger
        .reserve_for(second_winner, signer, 0, input)
        .expect_err("0 is spent by the settled lane above");
    steps.push(json!({
        "control": "nc12_a_committed_pair_cannot_be_taken_twice",
        "refusal_code": spent.code(),
        "refusal": format!("{spent}"),
        "lane_after": lane_json(ledger.lane(second_winner).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));
    let next_number = ledger
        .reserve_for(second_winner, signer, 1, input)
        .expect("the number after it is free");
    steps.push(json!({
        "control_positive_arm": "nc12_the_next_nonce_is_granted",
        "lane_id": second_winner.0,
        "nonce": next_number.nonce.nonce,
        "lane_after": lane_json(ledger.lane(second_winner).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));

    // NC15: a reserved lane whose window closes hands both back.
    let expiry = ledger
        .end(
            second_winner,
            LaneFailure::PlanExpired("the window closed".to_string()),
        )
        .expect("an expiry is an end");
    steps.push(json!({
        "control": "nc15_expiry_returns_the_pair",
        "from": expiry.from,
        "to": expiry.to,
        "reason": expiry.reason,
        "disposition": format!("{:?}", expiry.disposition).to_lowercase(),
        "released_nonce": expiry.released_nonce.map(|n| json!({ "signer": addr(n.signer), "nonce": n.nonce })),
        "released_capital": expiry.released_capital.map(|r| dec(r.amount)),
        "lane_after": lane_json(ledger.lane(second_winner).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));

    // NC14: one plan hash binds one lane, and one candidate gets one lane.
    let stranger = open(&mut ledger, "m11-evidence-cand-e", &mut steps);
    begin(&mut ledger, stranger, &mut steps);
    let duplicate_plan = ledger
        .record_simulation(stranger, "m11-evidence-sim-e".to_string(), plan_a)
        .expect_err("one hash binds one lane");
    steps.push(json!({
        "control": "nc14_one_plan_hash_binds_one_lane",
        "refusal_code": duplicate_plan.code(),
        "refusal": format!("{duplicate_plan}"),
        "lane_after": lane_json(ledger.lane(stranger).expect("here")),
    }));
    let duplicate_lane = ledger
        .open("m11-evidence-cand-a")
        .expect_err("one candidate speaks with one lane");
    steps.push(json!({
        "control": "nc14_one_candidate_gets_one_lane",
        "refusal_code": duplicate_lane.code(),
        "refusal": format!("{duplicate_lane}"),
        "lanes_in_the_ledger": ledger.lanes().count(),
    }));

    // NC13: a winner that cannot pay is refused and keeps nothing.
    let poor = open(&mut ledger, "m11-evidence-cand-f", &mut steps);
    begin(&mut ledger, poor, &mut steps);
    record(&mut ledger, poor, "m11-evidence-sim-f", plan_f, &mut steps);
    advance(
        &mut ledger,
        poor,
        LaneState::RiskChecking,
        "risk asked",
        &mut steps,
    );
    advance(
        &mut ledger,
        poor,
        LaneState::Ready,
        "risk accepted",
        &mut steps,
    );
    choose(
        &mut ledger,
        &[LaneStanding::new(poor, U256::from(5u64))],
        "§46's NC13 arm: ranked, then refused because the domain cannot cover the ask",
        &mut steps,
    );
    let over = ledger
        .reserve_for(poor, signer, 2, capacity + U256::ONE)
        .expect_err("more than the domain holds cannot be reserved");
    steps.push(json!({
        "control": "nc13_a_winner_that_cannot_pay_is_refused",
        "refusal_code": over.code(),
        "refusal": format!("{over}"),
        "lane_after": lane_json(ledger.lane(poor).expect("here")),
        "capital_after": capital_json(ledger.capital()),
    }));

    json!({
        "what_this_is": "§31–§36's lane ledger, driven on scripted numbers: nothing here touches \
                         REVM, an endpoint, or a key, which is §28's boundary stated as an \
                         absence rather than a promise.",
        "domain": {
            "domain_id": "m11-evidence-domain",
            "capacity_at_open": dec(capacity),
            "amount_each_lane_claims": dec(input),
        },
        "steps": steps,
        "final": {
            "lanes": ledger.lanes().map(lane_json).collect::<Vec<_>>(),
            "capital": capital_json(ledger.capital()),
            "outstanding_nonces": ledger.nonces().outstanding(),
            "winner": ledger.winner().map(|w| w.0),
        },
        "recomputed_independently": {
            "method": "the capital pool is an addition: available plus reserved either equals the \
                       capacity or the domain has double-spent. Every step above carries the \
                       pair, so the check is per step rather than only at the end.",
            "capacity": dec(capacity),
            "available_plus_reserved_at_the_end": dec(
                ledger.capital().available_capital + ledger.capital().reserved_capital
            ),
            "settled_input": dec(ledger.capital().settled_input()),
            "sums_to_capacity": ledger.capital().available_capital
                + ledger.capital().reserved_capital
                == capacity,
            "nonce_held_by_the_settled_lane": ledger
                .nonces()
                .held_by(signer, 0)
                .map(|l| l.0)
                .map(Value::from)
                .unwrap_or(Value::Null),
        },
    })
}

// ---------------------------------------------------------------------------
// §42's real/ pages: three questions, three UNKNOWNs with reasons
// ---------------------------------------------------------------------------

/// One of §42's three real-chain questions, answered `UNKNOWN`.
///
/// `m10` names the files that already answer the same question for a two-leg plan on the real
/// chain. Each path is published flat so the gate can ask of every one of them whether a reader
/// can actually open it: a pointer to a file that does not exist is worse than no pointer.
fn real_rows(
    question: &str,
    what_would_answer_it: &str,
    not_measured_because: &str,
    m10: &[(&str, &str)],
) -> Value {
    let mut evidence = Map::new();
    evidence.insert("controlled_2hop".into(), json!(CONTROLLED_2HOP));
    evidence.insert("controlled_3hop".into(), json!(CONTROLLED_3HOP));
    for (label, path) in m10 {
        assert!(
            evidence.insert((*label).to_string(), json!(path)).is_none(),
            "real row {question:?} names {label} twice"
        );
    }
    evidence.insert(
        "note".into(),
        json!(
            "M10 already answered these three questions for a two-leg plan on the real chain; \
               M11 adds no new transaction, no new contract and no new signer, so the M10 files \
               are the nearest real evidence and are named rather than restated here. M10's \
               reconciliation row lives inside its execution file, under \
               `reconciliation_32`."
        ),
    );
    json!({
        "what_this_is": question,
        "verdict": "UNKNOWN",
        "why_not_zero": "§41 forbids publishing a real-market verdict from a CONTROLLED_FIXTURE \
                         run, and 0 is a verdict. An unasked question has no answer in this \
                         directory.",
        "not_measured_because": not_measured_because,
        "what_would_answer_it": what_would_answer_it,
        "evidence_that_does_exist_for_the_mechanism": Value::Object(evidence),
    })
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn render(relative: &str, rows: &[(String, Value)]) -> String {
    let mut map = Map::new();
    for (key, row) in rows {
        assert!(
            map.insert(key.clone(), row.clone()).is_none(),
            "{relative}: row {key} written twice"
        );
    }
    let tail = relative.strip_prefix(EVIDENCE).unwrap_or(relative);
    let directory = match tail.rfind('/') {
        Some(index) if index > 0 && index + 1 < tail.len() => {
            tail[1..index].trim_start_matches('/').to_string()
        }
        _ => ".".to_string(),
    };
    let file = json!({
        "schema": SCHEMA,
        "milestone": "M11",
        "evidence_root": EVIDENCE,
        "directory": directory,
        "file": relative,
        "assembled_by": ASSEMBLED_BY,
        "assemble_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                             evm-simulation --test multihop_evidence -- --test-threads=1",
        "check_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                          evm-execution --test multihop_evidence_gate -- --test-threads=1",
        "checked_by": CHECKED_BY,
        "rows": map,
    });
    let text = serde_json::to_string_pretty(&file).expect("serializable");
    format!("{text}\n")
}

/// §42's whole tree, built in memory. Returns each file's bytes and the row keys inside it, so
/// the manifest can publish a count that is a fact about the rows rather than about the text.
async fn assemble() -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let fx = fixture();
    let cycle = Cycle::build();

    // ---- the recorded two-hop, walked end to end (§37) ----
    let two_candidate = recorded_cycle_candidate(&fx.dump);
    let two_route = multihop_recorded::recorded_route_from_candidate(&fx.dump, &two_candidate);
    let two_priced = candidate_at(&fx.dump, amount_in());
    let two_spec = request(&fx, &two_priced);
    let two_outcome = run_ok(&fx, &two_spec).await;
    let two_sim = file(&two_priced, &two_spec, two_outcome);
    let two_granted = recorded_acceptance(&two_sim);
    let (two_plan, two_binding) = plan_of(
        &two_sim,
        &two_granted,
        &recorded_context(&fx),
        &recorded_binding(),
    )
    .expect("the accepted recorded run binds to a plan");

    // ---- the declared triangle, walked the same way (§38) ----
    let cycle_graph = market_on(CHAIN, BLOCK, &cycle.reserves());
    let three_candidate = candidate_over(&cycle_graph, &pools_in_order(), WETH);
    let three_route = MultiHopRoute::new(&cycle_graph, three_candidate.edges.as_slice())
        .expect("the triangle is a route on the graph it was found in");
    let three_priced = at_amount(&three_route, cycle_amount_in());
    let three_spec = request(&cycle.fx, &three_priced);
    let three_outcome = run_ok(&cycle.fx, &three_spec).await;
    let three_sim = file(&three_priced, &three_spec, three_outcome);
    let three_granted = match recorded_policy(three_sim.gas_used).evaluate(
        &three_sim,
        &recorded_facts(BlockNumber(BLOCK), BlockNumber(BLOCK)),
    ) {
        MultihopRiskDecision::Accept(figures) => figures,
        other => panic!("the triangle run was not accepted: {other}"),
    };
    let three_context = MultihopPlanContext {
        market: MarketKind::ControlledFixture {
            proves: format!(
                "three legs ({}), each against a pool whose runtime code is the recorded pair's \
                 byte for byte; the topology is declared row by row in \
                 crates/simulation/tests/multihop_state",
                pools_in_order()
                    .iter()
                    .map(|p| format!("{:#x}", p))
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
        },
        correlation_id: "m11-evidence-3hop".to_string(),
        ..recorded_context(&cycle.fx)
    };
    let (three_plan, three_binding) = plan_of(
        &three_sim,
        &three_granted,
        &three_context,
        &recorded_binding(),
    )
    .expect("the accepted triangle run binds to a plan");

    // ---- the four-hop: priced, never run (M9.3 refuses a fourth-hop candidate) ----
    let four_route = multihop_market::square_route();
    let four_input = u(1_000);

    // ---- the two states every run below was served from, committed and digest-checked ----
    let two_state = state_json(
        &fx,
        executor_state::FIXTURE,
        executor_state::FIXTURE_ADDITIONS,
    );
    let cycle_state = state_json(&cycle.fx, CYCLE_FIXTURE, CYCLE_ADDITIONS);

    // ---- rows ----
    let recorded_market_claim = json!({
        "kind": "REAL_MARKET_POOLS_ON_A_CONTROLLED_FIXTURE_STATE",
        "attested_by": format!(
            "chain {}, block {BLOCK}: both pools, both tokens and every reserve come from the \
             recording M7 froze; the executor deployment, the operator's balance and the \
             approvals are declared rows",
            CHAIN.0
        ),
        "forbidden_reading": "a quote on these pools is not a claim that the market pays it \
                              (§29/§41)",
    });
    let declared_market_claim = json!({
        "kind": "CONTROLLED_FIXTURE",
        "proves": "the pipeline's shape at three legs, against real pair bytecode",
        "does_not_prove": "that any such triangle exists at these depths on any chain (§41)",
    });
    let synthetic_market_claim = json!({
        "kind": "DECLARED_SYNTHETIC_GRAPH",
        "pools": multihop_market::P1,
        "proves": "that the pricing composes over four hops and reports its own length",
        "does_not_prove": "a market, and nothing here is run through the EVM at four legs",
    });

    let mut published: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();

    published.insert(
        PRICING_2HOP.to_string(),
        vec![(
            format!("recorded-2hop-chain{}-block{BLOCK}", CHAIN.0),
            pricing_row(
                &two_route,
                amount_in(),
                &fx.source,
                recorded_market_claim.clone(),
                "the recorded pair, priced at M11's stake by price() and by this file's fold",
            ),
        )],
    );
    published.insert(
        PRICING_3HOP.to_string(),
        vec![(
            format!(
                "declared-3hop-chain{}-block{}-{}",
                CHAIN.0,
                BLOCK,
                pools_in_order()
                    .iter()
                    .map(|p| format!("{p:#x}"))
                    .collect::<Vec<_>>()
                    .join("-")
            ),
            pricing_row(
                &three_route,
                cycle_amount_in(),
                &cycle.fx.source,
                declared_market_claim.clone(),
                "the declared triangle, priced at this fixture's stake",
            ),
        )],
    );
    published.insert(
        PRICING_4HOP.to_string(),
        vec![(
            "declared-4hop-synthetic-p1-p4".to_string(),
            pricing_row(
                &four_route,
                four_input,
                "declared synthetic graph (crates/simulation/tests/multihop_market/mod.rs)",
                synthetic_market_claim.clone(),
                "four hops on a hand-written graph: pricing only, never run through the EVM",
            ),
        )],
    );

    published.insert(
        OPTIMIZER_2HOP.to_string(),
        vec![
            (
                "recorded-2hop-single-point".to_string(),
                optimizer_row(
                    &two_route,
                    amount_in(),
                    amount_in(),
                    OptimizationPolicy::default_search(),
                    &fx.source,
                    "the domain of one input every downstream stage reads its numbers from",
                ),
            ),
            (
                format!(
                    "recorded-2hop-exhaustive-window-{}",
                    amount_in().saturating_sub(U256::from(63u64))
                ),
                optimizer_row(
                    &two_route,
                    amount_in() - U256::from(63u64),
                    amount_in(),
                    OptimizationPolicy::default_search(),
                    &fx.source,
                    "a 64-input window walked entirely, with this file's brute-force scan beside \
                     it",
                ),
            ),
            (
                "recorded-2hop-coarse-refine-wide".to_string(),
                optimizer_row(
                    &two_route,
                    U256::ONE,
                    U256::from(20_000_000u64),
                    OptimizationPolicy::default_search(),
                    &fx.source,
                    "a domain wider than the exhaustive limit, so the sampled branch is the one \
                     being checked",
                ),
            ),
        ],
    );
    published.insert(
        OPTIMIZER_3HOP.to_string(),
        vec![
            (
                "declared-3hop-single-point".to_string(),
                optimizer_row(
                    &three_route,
                    cycle_amount_in(),
                    cycle_amount_in(),
                    OptimizationPolicy::default_search(),
                    &cycle.fx.source,
                    "the triangle's one-input domain",
                ),
            ),
            (
                "declared-3hop-exhaustive-window".to_string(),
                optimizer_row(
                    &three_route,
                    cycle_amount_in(),
                    cycle_amount_in() + U256::from(31u64),
                    OptimizationPolicy::default_search(),
                    &cycle.fx.source,
                    "a 32-input window above the fixture's stake, walked entirely",
                ),
            ),
        ],
    );

    published.insert(
        SIMULATION_2HOP.to_string(),
        vec![(
            format!("recorded-2hop-sim-{}", hash(two_sim.identity_hash())),
            simulation_row(
                &two_state,
                &two_priced,
                &two_spec,
                &two_sim,
                "the recorded pair, executed by REVM over M10's committed bytecode",
            ),
        )],
    );
    published.insert(
        SIMULATION_3HOP.to_string(),
        vec![(
            format!("declared-3hop-sim-{}", hash(three_sim.identity_hash())),
            simulation_row(
                &cycle_state,
                &three_priced,
                &three_spec,
                &three_sim,
                "the declared triangle, three legs executed in one call",
            ),
        )],
    );

    let facts = recorded_facts(BlockNumber(BLOCK), BlockNumber(BLOCK));
    let two_decision = MultihopRiskDecision::Accept(two_granted.clone());
    let three_decision = MultihopRiskDecision::Accept(three_granted.clone());
    let refused = policy_at(two_sim.gas_used, |policy| {
        policy.minimum_gross_profit = U256::MAX;
    })
    .evaluate(&two_sim, &facts);
    published.insert(
        RISK_DECISION.to_string(),
        vec![
            (
                format!("accept-recorded-2hop-{}", hash(two_sim.identity_hash())),
                risk_row(
                    "recorded-2hop",
                    &two_sim,
                    &recorded_policy(two_sim.gas_used),
                    &facts,
                    &two_decision,
                    Some(&two_granted),
                    "§26–§29's acceptance over the recorded run, with the six comparisons it made",
                ),
            ),
            (
                format!("accept-declared-3hop-{}", hash(three_sim.identity_hash())),
                risk_row(
                    "declared-3hop",
                    &three_sim,
                    &recorded_policy(three_sim.gas_used),
                    &facts,
                    &three_decision,
                    Some(&three_granted),
                    "the same policy over the triangle",
                ),
            ),
            (
                format!(
                    "reject-floor-above-the-delivery-{}",
                    hash(two_sim.identity_hash())
                ),
                risk_row(
                    "recorded-2hop-floor-above-the-delivery",
                    &two_sim,
                    &policy_at(two_sim.gas_used, |policy| {
                        policy.minimum_gross_profit = U256::MAX;
                    }),
                    &facts,
                    &refused,
                    None,
                    "§46's NC5 arm: the same run, the same facts, a floor no delivery reaches",
                ),
            ),
        ],
    );

    published.insert(
        LANE_MATRIX.to_string(),
        vec![("lane-ledger-§31-§36".to_string(), lane_rows())],
    );

    published.insert(
        CONTROLLED_2HOP.to_string(),
        vec![(
            format!(
                "chain-{}-block-{BLOCK}-recorded-pair-{}",
                CHAIN.0,
                hash(two_plan.plan_hash())
            ),
            chain_row(
                "two-hop",
                &two_candidate,
                &two_route,
                &two_priced,
                &fx,
                &two_state,
                &two_spec,
                &two_sim,
                &two_granted,
                &two_plan,
                &two_binding,
            ),
        )],
    );
    published.insert(
        CONTROLLED_3HOP.to_string(),
        vec![
            (
                format!(
                    "chain-{}-block-{BLOCK}-declared-triangle-{}",
                    CHAIN.0,
                    hash(three_plan.plan_hash())
                ),
                chain_row(
                    "three-hop",
                    &three_candidate,
                    &three_route,
                    &three_priced,
                    &cycle.fx,
                    &cycle_state,
                    &three_spec,
                    &three_sim,
                    &three_granted,
                    &three_plan,
                    &three_binding,
                ),
            ),
            (
                "three-hop-rollback-at-the-third-pool".to_string(),
                rollback_row(&cycle, &cycle_state).await,
            ),
        ],
    );

    published.insert(
        REAL_EXECUTION.to_string(),
        vec![(
            "real-execution".to_string(),
            real_rows(
                "did a M11 multi-hop plan execute on the real chain?",
                "one signed, broadcast, receipted transaction of a plan built by this pipeline, \
                 with its receipt and its balance rows",
                "no broadcast was authorized for M11, M11 adds no new contract to deploy, and the \
                 one real three-pool cycle in the recording lives in pools whose runtime code has \
                 no swap dispatcher arm \
                 (fixtures/simulation-m11/probe-37224031-three-leg-slots.json), so there is no \
                 real three-hop this pipeline could drive",
                &[
                    ("m10_real_execution", M10_REAL_EXECUTION),
                    ("m10_real_ladder_steps", M10_REAL_LADDER),
                    ("m10_real_preconditions", M10_REAL_PRECONDITIONS),
                ],
            ),
        )],
    );
    published.insert(
        REAL_FAILURE.to_string(),
        vec![(
            "real-failure".to_string(),
            real_rows(
                "what does the real chain answer to a plan that cannot pay?",
                "a real transaction whose revert is read out of a receipt",
                "the same authorization boundary as above; the failure arms this directory does \
                 have are REVM's, on real bytecode, and they are named in \
                 data/evidence/m11/controlled/3hop/chain.json",
                &[
                    ("m10_real_failure", M10_REAL_FAILURE),
                    ("m10_real_ladder_steps", M10_REAL_LADDER),
                ],
            ),
        )],
    );
    published.insert(
        REAL_RECONCILIATION.to_string(),
        vec![(
            "real-reconciliation".to_string(),
            real_rows(
                "does the simulated delivery reconcile against the on-chain balance delta?",
                "two balance readings of the recipient across one real inclusion, with the \
                 simulated figure beside the delta and the difference explained",
                "there is no real inclusion to reconcile against in M11. M10 reconciled its \
                 two-leg plan on chain 91342; that file is named above rather than copied",
                &[
                    ("m10_real_execution", M10_REAL_EXECUTION),
                    ("m10_real_failure", M10_REAL_FAILURE),
                ],
            ),
        )],
    );

    // ---- serialize, twice, and compare the bytes ----
    let first = serialize(&published);
    let second = serialize(&published);
    assert_eq!(
        first, second,
        "two assemblies of one directory disagreed — the row text is not a function of the fields"
    );
    let row_keys = published
        .iter()
        .map(|(file, rows)| {
            (
                file.clone(),
                rows.iter().map(|(key, _)| key.clone()).collect(),
            )
        })
        .collect();
    (first, row_keys)
}

fn serialize(published: &BTreeMap<String, Vec<(String, Value)>>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (file, rows) in published {
        out.insert(file.clone(), render(file, rows));
    }
    out
}

/// The cycle the search finds in `graph` that walks exactly `pools`, entered at `start`.
///
/// Selection is by pool set, start token and fee status rather than by position: the search also
/// reports the same cycle walked the other way, so a row picked by index would change meaning
/// when the graph grew.
fn candidate_over(
    graph: &evm_graph::GraphSnapshot,
    pools: &[Address],
    start: Address,
) -> CycleCandidate {
    let wanted: Vec<evm_core::PoolId> = pools
        .iter()
        .map(|p| evm_core::PoolId::new(CHAIN, *p))
        .collect();
    let found = find_cycles(graph, &PathFinderConfig::new(pools.len()))
        .unwrap_or_else(|error| panic!("the search refused {pools:?}: {error}"));
    let matches: Vec<CycleCandidate> = found
        .into_iter()
        .filter(|candidate| {
            candidate.hop_count == wanted.len()
                && candidate.pools() == wanted
                && candidate.start_token.address == start
                && candidate.fee_status == FeeStatus::Complete
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one candidate walks {pools:?} at {start:#x}"
    );
    matches.into_iter().next().expect("the one match")
}

/// The whole §37/§38 chain as one row: stage by stage, each stage's answer and the recomputation
/// of the figure that stage is claimed to have produced.
// One row is the assembly of every stage's output, so its inputs are the stages; a context struct
// would move the same eleven names into a second declaration.
#[allow(clippy::too_many_arguments)]
fn chain_row(
    depth: &str,
    candidate: &CycleCandidate,
    route: &MultiHopRoute,
    priced: &OptimizedCandidate,
    fx: &Fixture,
    state: &Value,
    spec: &ExecutorRun,
    simulated: &SimulatedOpportunity,
    granted: &MultihopAcceptance,
    plan: &ExecutablePlan,
    binding: &MultihopBinding,
) -> Value {
    let encoded = plan.calldata().clone();
    let recomputed_calldata_hash = keccak256(encoded.as_ref());
    let recomputed_plan_hash = keccak256(plan.plan().canonical_text().as_bytes());
    let identity = minimum_rotation(&candidate.edges);
    let quote_output = priced.search.quote.output;

    json!({
        "what_this_is": format!("§37/§38's {depth} case, walked from the cycle the search found \
                                 to the bytes an executor would run"),
        "stages": {
            "search": candidate_json(candidate),
            "pricing": pricing_row(route, priced.search.best_input, &fx.source, json!({
                "kind": "see pricing/ for this route's market claim",
            }), "the route the candidate names, priced at the optimizer's own best input"),
            "optimizer": json!({
                "best_input": dec(priced.search.best_input),
                "best_output": dec(priced.search.best_output),
                "evaluations": priced.search.evaluations,
                "strategy": format!("{:?}", priced.search.strategy).to_lowercase(),
                "domain": [dec(priced.search.domain_min), dec(priced.search.domain_max)],
            }),
            "simulation": simulation_row(state, priced, spec, simulated, &format!(
                "the {depth} run, filed under the candidate it claims to be the run of"
            )),
            "risk": json!({
                "figures": json!({
                    "input_amount": dec(granted.input_amount),
                    "delivered": dec(granted.delivered),
                    "gross_profit": dec(granted.gross_profit),
                    "minimum_gross_profit": dec(granted.minimum_gross_profit),
                    "min_final_output": dec(granted.min_final_output),
                    "gas_used": granted.gas_used,
                    "maximum_gas": granted.maximum_gas,
                    "gas_charge_wei": granted.gas_charge_wei.map(dec),
                }),
                "gross_by_arithmetic": dec(granted.delivered.saturating_sub(granted.input_amount)),
                "clears_the_floor": granted.gross_profit >= granted.minimum_gross_profit,
            }),
            "plan": plan_json(plan),
            "binding": binding_json(binding),
        },
        "the_five_claims": {
            "route_identity": format!("{:?}", route.identity()),
            "route_identity_recomputed": identity.iter().map(edge_json).collect::<Vec<_>>(),
            "quote": quote_json(&priced.search.quote),
            "plan_hash": hash(plan.plan_hash()),
            "plan_hash_recomputed": format!("{recomputed_plan_hash:#x}"),
            "calldata_hash": hash(plan.calldata_hash()),
            "calldata_hash_recomputed": format!("{recomputed_calldata_hash:#x}"),
            "simulation_identity_hash": hash(simulated.identity_hash()),
        },
        "the_contract_answered": {
            "delivered": opt_dec(simulated.final_amount),
            "gas_used": simulated.gas_used,
            "status": simulated.status.name(),
            "reserve_rows_moved": simulated.outcome.reserves.iter().filter(|r| r.changed()).count(),
            "min_final_output_met": simulated.final_amount.is_some_and(|d| d >= simulated.min_final_output),
            "quoted_output": dec(quote_output),
        },
        "not_claimed": json!([
            "a real-market verdict (§41)",
            "that this plan was signed, broadcast or included",
            "that the pricing's output is what the market would pay a live taker"
        ]),
    })
}

/// §39's rollback row: the third pool's own invariant refusing, and every word staying put.
async fn rollback_row(cycle: &Cycle, state: &Value) -> Value {
    let legs = cycle.route.legs_inflated(2, 2);
    let call = cycle.call(legs, cycle.route.weth_out_leg_c);
    let spec = cycle.fx.spec(call);
    let calldata = bytes_hex(spec.call.encode().as_ref());
    let outcome = cycle
        .fx
        .try_run_spec(&spec)
        .await
        .expect("§39's call is answered by the EVM; it is the contract that refuses it");
    json!({
        "what_this_is": "§39: the third leg asks double its priced output, and the call leaves \
                         nothing behind",
        "state": state.clone(),
        "spec": run_spec_json(&spec),
        "calldata": calldata,
        "status": format!("{:?}", outcome.status),
        "revert_reason": outcome.revert().map(|d| d.reason().to_string()),
        "contract_error": outcome.contract_error.clone(),
        "delivered": opt_dec(outcome.delivered),
        "reserve_rows": outcome.reserves.iter().map(|r| json!({
            "pool": addr(r.pool),
            "before": [dec(r.before.reserve0), dec(r.before.reserve1)],
            "after": [dec(r.after.reserve0), dec(r.after.reserve1)],
            "changed": r.changed(),
        })).collect::<Vec<_>>(),
        "balance_rows_unchanged": outcome.balances.iter().filter(|b| !b.changed()).count(),
        "balance_rows": outcome.balances.len(),
        "market_moved": outcome.market_moved(),
        "changed_slots_in_the_three_pools": {
            "pools": [addr(multihop_state::POOL_BC), addr(multihop_state::POOL_CA), addr(POOL_A)],
            "counts": [
                outcome.changed_slots_in(multihop_state::POOL_BC).len(),
                outcome.changed_slots_in(multihop_state::POOL_CA).len(),
                outcome.changed_slots_in(POOL_A).len(),
            ],
            "means": "the counts are paired with the pool each was counted on, so a reader who \
                      knows only these three addresses can ask the same question; the word \
                      order is a rendering, not an index",
        },
        "verdict": "the run is refused by the third pool's constant-product check, and no word \
                    this fixture watches differs from what it was before the call",
    })
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m11_evidence_directory_is_assembled() {
    let root = workspace_root();
    let (files, row_keys) = assemble().await;
    for (relative, text) in &files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(&path, text).expect("the file writes");
        println!("wrote {}", path.display());
    }

    // README first: the manifest digests it, and a file cannot digest its own bytes.
    let readme = readme_text();
    let readme_path = root.join(README);
    std::fs::write(&readme_path, &readme).expect("the README writes");
    println!("wrote {}", readme_path.display());

    let mut digests = Map::new();
    let mut total_bytes = 0usize;
    for (relative, text) in &files {
        let keys = row_keys
            .get(relative)
            .unwrap_or_else(|| panic!("{relative} has no row keys"));
        digests.insert(
            relative.clone(),
            json!({
                "rows": keys.len(),
                "row_keys": keys,
                "digest": digest_of(text),
            }),
        );
        total_bytes += text.len();
    }
    digests.insert(README.to_string(), json!({ "digest": digest_of(&readme) }));
    total_bytes += readme.len();

    let manifest = json!({
        "schema": SCHEMA,
        "milestone": "M11",
        "evidence_root": EVIDENCE,
        "directory": ".",
        "file": MANIFEST,
        "assembled_by": ASSEMBLED_BY,
        "assemble_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                             evm-simulation --test multihop_evidence -- --test-threads=1",
        "check_command": "CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                          evm-execution --test multihop_evidence_gate -- --test-threads=1",
        "checked_by": CHECKED_BY,
        "tree": REQUIRED.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        "file_digests": Value::Object(digests),
        "files_written": files.len() + 2,
        "total_bytes": total_bytes,
        "total_bytes_scope": "the bytes of every file this manifest carries a digest for, which \
                              is the tree minus this file: a manifest cannot count the bytes it is \
                              about to contain, the same reason `self_digest` is not a digest",
        "self_digest": "not included: a file cannot hash the bytes it is about to contain",
        "determinism": {
            "assemblies_in_this_run": 2,
            "byte_identical": true,
            "method": "the directory was serialized twice from the same assembled rows and the \
                       two byte maps were compared before anything reached disk",
            "cross_process": "tests/multihop_determinism.rs walks the recorded chain through two \
                              independent loads of the fixture and compares the five figures; \
                              the gate re-runs each row's arithmetic in another crate's process",
        },
        "rpc_count": 0,
        "rpc_count_basis": {
            "every_run_in_this_directory_is_served_by": std::any::type_name::<DumpStateProvider>(),
            "endpoint_variable_present": std::env::var("GIWA_RPC_URL").is_ok(),
            "the_one_file_that_would_have_an_rpc": "crates/simulation/tests/multihop_capture.rs \
                                                    is #[ignore]d and is not part of this \
                                                    assembly; it publishes its own measured cost \
                                                    in fixtures/simulation-m11/capture-37224031-\
                                                    triangle-notes.json",
        },
        "verdict": "UNKNOWN",
        "verdict_means": "the controlled chains are measured and recomputable; the real chain's \
                          answer to §42's three questions was not asked by M11 and is not \
                          reported as zero (§41)",
        "not_measured": json!([
            {
                "item": "a four-hop route executed through the EVM",
                "why": "M9.3's pathfinder refuses a candidate deeper than three hops \
                        (crates/pathfinder/src/config.rs: PathFinderConfig::MAX_MAX_HOPS, \
                        enforced in crates/pathfinder/src/candidate.rs: CycleCandidate::assemble), \
                        so no four-hop CycleCandidate exists to price end to end",
                "what_is_measured_instead": "data/evidence/m11/pricing/declared_4hop.json, on a \
                                             hand-written graph",
                "downstream_arms_that_do_accept_four": "the adapter builds a four-leg request \
                        (crates/simulation/tests/multihop_adapter.rs) and risk records one \
                        (crates/simulation/tests/multihop_risk.rs)",
            },
            {
                "item": "a real three-hop cycle that the executor can drive",
                "why": "the one real triangle in the recording lives in pools whose runtime code \
                        carries no swap dispatcher arm, measured in \
                        crates/simulation/tests/triangle_probe.rs and recorded in \
                        fixtures/simulation-m11/probe-37224031-three-leg-slots.json",
            },
            {
                "item": "real execution, real failure and real reconciliation of an M11 plan",
                "why": "no broadcast was authorized for M11 and §2 forbids a new executor \
                        contract; see data/evidence/m11/real/",
            },
        ]),
        "not_claimed": json!([
            "a real-market profit figure",
            "that any CONTROLLED_FIXTURE row here is evidence about a live market (§41)",
            "a transaction hash, a signature, or a private key"
        ]),
    });
    let manifest_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest).expect("manifest")
    );
    let manifest_path = root.join(MANIFEST);
    std::fs::write(&manifest_path, &manifest_text).expect("the manifest writes");
    println!("wrote {}", manifest_path.display());

    for required in REQUIRED {
        assert!(root.join(required).exists(), "{required} was not written");
    }
}

/// §43's demand, asked of the files rather than of the runs: every row that publishes a hash
/// must have that hash recomputable from bytes the same row publishes.
///
/// Rows come in two shapes — a simulation page's row *is* the simulation row, and a controlled
/// chain page carries the simulation under `stages.simulation` — so the row is located rather
/// than assumed, and a row with neither is skipped by name rather than read as if it had one.
#[test]
fn published_rows_hash_their_own_published_bytes() {
    let root = workspace_root();
    let mut checked = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for relative in [
        SIMULATION_2HOP,
        SIMULATION_3HOP,
        CONTROLLED_2HOP,
        CONTROLLED_3HOP,
    ] {
        let text = std::fs::read_to_string(root.join(relative)).unwrap_or_else(|_| {
            panic!("{relative} is written by m11_evidence_directory_is_assembled")
        });
        let file: Value = serde_json::from_str(&text).expect("valid json");
        let rows = file["rows"].as_object().expect("a rows object");
        for (key, row) in rows {
            let located = row.get("published").cloned().or_else(|| {
                row.get("stages")
                    .and_then(|s| s.get("simulation"))
                    .and_then(|s| s.get("published"))
                    .cloned()
            });
            let Some(published) = located else {
                skipped.push(format!("{relative}#{key}"));
                continue;
            };
            let opportunity = &published["simulated_opportunity"];
            let calldata = opportunity["calldata"]
                .as_str()
                .unwrap_or_else(|| panic!("{key}: no published calldata"));
            let published_hash = opportunity["calldata_hash"]
                .as_str()
                .unwrap_or_else(|| panic!("{key}: no published calldata hash"));
            let bytes =
                hex::decode(calldata.strip_prefix("0x").unwrap_or(calldata)).expect("hex bytes");
            assert_eq!(
                format!("{:#x}", keccak256(&bytes)),
                published_hash,
                "{key}: the published calldata hash is not the hash of the published bytes"
            );
            checked += 1;

            let canonical = opportunity["canonical_text"]
                .as_str()
                .unwrap_or_else(|| panic!("{key}: no published canonical text"));
            assert_eq!(
                format!("{:#x}", keccak256(canonical.as_bytes())),
                opportunity["identity_hash"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{key}: no published identity hash")),
                "{key}: the published simulation identity hash is not the hash of the published \
                 canonical text"
            );
            checked += 1;

            let plan = &row["stages"]["plan"];
            if let (Some(canonical), Some(plan_hash)) =
                (plan["canonical_text"].as_str(), plan["plan_hash"].as_str())
            {
                assert_eq!(
                    format!("{:#x}", keccak256(canonical.as_bytes())),
                    plan_hash,
                    "{key}: the published plan hash is not the hash of the published canonical text"
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 6,
        "only {checked} hash bindings were checked, which is fewer than the pages carry"
    );
    assert_eq!(
        skipped,
        vec![
            "data/evidence/m11/controlled/3hop/chain.json#three-hop-rollback-at-the-third-pool"
                .to_string()
        ],
        "the only row without a simulation section is §39's rollback, which publishes reserves \
         rather than hashes"
    );
}

/// Writes the triangle's committed state files. Reached only by `--ignored`, which is how M10's
/// fixture came to exist too: the gates read the state a run was served from, one setup test
/// writes it. The bytes are produced twice and must agree, or the file on disk would be a receipt
/// of one moment rather than a state anybody can load.
#[test]
#[ignore]
fn m11_cycle_state_files_are_written() {
    let root = workspace_root();
    let first = Cycle::build();
    let second = Cycle::build();
    let files = [
        (
            CYCLE_FIXTURE,
            first.fx.fixture_bytes(),
            second.fx.fixture_bytes(),
        ),
        (
            CYCLE_ADDITIONS,
            cycle_additions_text(&first).into_bytes(),
            cycle_additions_text(&second).into_bytes(),
        ),
    ];
    for (relative, bytes, again) in files {
        assert_eq!(
            bytes, again,
            "{relative} is not reproducible from the fixture's own code"
        );
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(&path, &bytes).expect("the file writes");
        println!("wrote {}: {} bytes", path.display(), bytes.len());
    }
    let loaded = evm_simulation::state::StateDump::from_file(&root.join(CYCLE_FIXTURE))
        .expect("the written state reloads as a dump");
    assert_eq!(
        loaded.accounts.len(),
        first.fx.dump.accounts.len(),
        "the written state has a different account census than the one that ran"
    );
    assert_eq!(
        keccak256(std::fs::read(root.join(CYCLE_FIXTURE)).expect("readable")),
        keccak256(first.fx.fixture_bytes()),
        "the file on disk is not the bytes this run used"
    );
}

fn readme_text() -> String {
    let mut text = String::new();
    text.push_str("# M11 evidence — multi-hop pricing, optimisation, simulation, risk, lanes\n\n");
    text.push_str(
        "§42's tree, assembled by `crates/simulation/tests/multihop_evidence.rs` and checked by \
         `crates/execution/tests/multihop_evidence_gate.rs`. The gate reads these files and \
         writes nothing.\n\n",
    );
    text.push_str("## What is here\n\n```text\n");
    for required in REQUIRED {
        // Spelled from the workspace root: a reader who copies one of these paths opens the file.
        text.push_str(&format!("{required}\n"));
    }
    text.push_str("```\n\n");
    text.push_str(
        "Every row carries the pipeline's figure and this file's own recomputation of it beside \
         each other: pricing as a constant-product fold over the published reserves and fees, \
         route identity as a minimum rotation over the published edge list, the optimizer's best \
         as a re-ranked scan over a grid rebuilt from the published domain and policy, the \
         simulation's hashes as keccak over the published bytes, the risk decision as the six \
         comparisons §26–§29 make, the lane ledger as a capital addition.\n\n",
    );
    text.push_str("## Commands\n\n```text\n");
    text.push_str(
        "assemble:  CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                   evm-simulation --test multihop_evidence -- --test-threads=1\n",
    );
    text.push_str(
        "check:     CC=clang CXX=clang++ CXXFLAGS=\"-include cstdint\" cargo test -p \
                   evm-execution --test multihop_evidence_gate -- --test-threads=1\n",
    );
    text.push_str("```\n\n");
    text.push_str(
        "## What this directory is not\n\n\
         No row here is a real-market verdict (§41). The three-hop's pools, the four-hop's whole \
         graph, the executor deployment and the funding rows are declared; the recorded pair's \
         reserves are real and its execution is simulated. `real/` answers `UNKNOWN` to all three \
         of §42's questions, with the reason each one has. `rpc_count` is 0: every run here is \
         served from a committed dump file by a provider that holds no endpoint, and the ignored \
         capture test that does pay an RPC price publishes its own measured cost elsewhere.\n\n",
    );
    text.push_str("Nothing here signs, broadcasts, or holds a key.\n");
    text
}
