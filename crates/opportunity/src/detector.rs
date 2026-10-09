//! From a market graph to the routes in it that could pay.
//!
//! This is the only entry point the rest of the system needs: a
//! [`GraphSnapshot`] in, a [`Detection`] out. Nothing here touches a node, a
//! state store, or a decoder — the graph already decided which pools exist at
//! which block, and re-deriving that would let the opportunity layer disagree
//! with the market it claims to be pricing.
//!
//! Every candidate ends up in exactly one of three lists, and all three are
//! sorted, so "no opportunity" is itself an auditable result rather than an
//! empty return:
//!
//! ```text
//! opportunities   gross profit > 0, best first
//! rejected        priced, but unattested fee / bad reserves / no peak above zero
//! skipped_pairs   pool pairs that never became a route (same pool twice)
//! ```

use std::collections::BTreeSet;

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId, PoolId, TokenId};
use evm_graph::GraphSnapshot;

use crate::error::{MathError, OpportunityError, PathError, Result, RouteError};
use crate::optimizer::{
    find_optimal_input, OptimizedCycle, PricedCycle, PricedHop, SearchPolicy, SearchRecord,
};
use crate::path::{ArbitragePath, Hop, PathSimulation};

/// Why a candidate route produced no opportunity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RejectionReason {
    /// The pool has no proven fee, and guessing one is how a report invents a
    /// market. `None` is not zero.
    MissingFee(PoolId),
    /// A reserve the quote cannot divide by.
    InvalidReserve,
    /// A fee ratio that is not a fraction of the input.
    InvalidFee,
    /// A zero input amount.
    InvalidAmount,
    /// An intermediate product that does not fit in 256 bits.
    Overflow,
    /// The route is not a two-pool cycle.
    InvalidPath(PathError),
    /// M11's shape refusal, carried whole. Two-hop detection never produces it —
    /// the route it names has more than two pools — but `OpportunityError` is one
    /// enum, so the mapping has to be total rather than pretend the case cannot
    /// arrive.
    InvalidRoute(RouteError),
    /// A hop this graph does not contain.
    MissingHop(PoolId),
    /// The exit pool holds at most one unit of the input token, so no input can
    /// come back larger than it went in.
    EmptyDomain,
    /// The best round trip found does not gain. The peak itself is kept in
    /// [`CandidateRejection::peak`], so the near miss is auditable.
    Unprofitable,
}

impl RejectionReason {
    fn classify(error: &OpportunityError) -> Self {
        match error {
            OpportunityError::UnattestedFee(pool, _) => Self::MissingFee(*pool),
            OpportunityError::Math(MathError::InvalidReserve) => Self::InvalidReserve,
            OpportunityError::Math(MathError::InvalidFee) => Self::InvalidFee,
            OpportunityError::Math(MathError::InvalidAmount) => Self::InvalidAmount,
            OpportunityError::Math(MathError::Overflow) => Self::Overflow,
            OpportunityError::Path(path) => Self::InvalidPath(*path),
            OpportunityError::Route(route) => Self::InvalidRoute(*route),
            OpportunityError::MissingPool(pool) | OpportunityError::MissingState(pool) => {
                Self::MissingHop(*pool)
            }
            OpportunityError::EmptySearchDomain { .. } => Self::EmptyDomain,
        }
    }
}

/// A priced candidate that did not clear zero, and how close it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CandidateRejection {
    pub path: ArbitragePath,
    pub reason: RejectionReason,
    /// The best round trip found for this route, peak input included. `None`
    /// when the route could not be priced at all.
    pub peak: Option<PathSimulation>,
}

/// A pair of pool directions the enumeration looked at and did not turn into a
/// route, with the rule that refused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SkippedPair {
    pub first: Hop,
    pub second: Hop,
    pub reason: PathError,
}

/// One route that gains, at the best input the search found.
///
/// `gross_profit` is the AMM's own token arithmetic at this block: no gas, no
/// simulation, no execution, no token tax, nothing that would have to be true
/// for a bundle to land. It answers "does this market state contain a price
/// discrepancy?", which is the whole of M3 (§41).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Opportunity {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    pub path: ArbitragePath,
    pub input_token: TokenId,
    pub input_amount: U256,
    pub output_amount: U256,
    pub gross_profit: U256,
    /// The two pools this was priced against, in trade order: each one's
    /// reserves in the direction traded and its attested fee. Carried so the
    /// finding is auditable on its own, without the snapshot it came from.
    pub hops: [PricedHop; 2],
    /// What the input search did, carried so the claim stays exactly as strong
    /// as the method that produced it (§23).
    pub search: SearchRecord,
}

impl Opportunity {
    /// Only a strictly positive gross profit makes one. A break-even route is
    /// not an opportunity wearing a hat.
    pub fn new(
        chain_id: ChainId,
        block_number: BlockNumber,
        best: &OptimizedCycle,
    ) -> Option<Self> {
        Some(Self {
            chain_id,
            block_number,
            path: best.simulation.path,
            input_token: best.simulation.path.input_token(),
            input_amount: best.simulation.input,
            output_amount: best.simulation.output,
            gross_profit: best.simulation.gross_profit()?,
            hops: [best.cycle.first, best.cycle.second],
            search: best.record,
        })
    }

    /// The stable identity of this finding: same chain, same block, same route
    /// (§33).
    pub fn identity(&self) -> (ChainId, BlockNumber, ArbitragePath) {
        (self.chain_id, self.block_number, self.path)
    }
}

/// The audit form (§55): who, where, what goes in, what comes out, and the two
/// markets that produced it — one finding per line group, no re-reading the
/// snapshot to check a number.
impl std::fmt::Display for Opportunity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [first, second] = self.hops;
        let path = self.path;
        write!(
            f,
            concat!(
                "chain {} block {}\n",
                "route {} -> pool {} -> {} -> pool {} -> {}\n",
                "input  {} amount {}\n",
                "output {} amount {}\n",
                "gross profit     {}\n",
                "pool {} reserves in/out {}/{} fee {}/{}\n",
                "pool {} reserves in/out {}/{} fee {}/{}\n",
                "search {:?} over {}..={} rounds {} evaluations {} scanned {} closed {}",
            ),
            self.chain_id.0,
            self.block_number.0,
            path.input_token().address,
            first.pool.address,
            path.mid_token().address,
            second.pool.address,
            path.input_token().address,
            path.input_token().address,
            self.input_amount,
            path.input_token().address,
            self.output_amount,
            self.gross_profit,
            first.pool.address,
            first.reserve_in,
            first.reserve_out,
            first.fee.numerator,
            first.fee.denominator,
            second.pool.address,
            second.reserve_in,
            second.reserve_out,
            second.fee.numerator,
            second.fee.denominator,
            self.search.strategy,
            self.search.lower_bound,
            self.search.upper_bound,
            self.search.rounds,
            self.search.evaluations,
            self.search.scan_count,
            self.search.interval_closed,
        )
    }
}

/// Everything one snapshot produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    /// Routes found in the graph, before pricing, in path-identity order. Kept
    /// so a null result can be reported as "these many candidates were checked",
    /// not as silence.
    pub candidates: Vec<ArbitragePath>,
    pub opportunities: Vec<Opportunity>,
    pub rejected: Vec<CandidateRejection>,
    pub skipped_pairs: Vec<SkippedPair>,
    pub policy: SearchPolicy,
}

impl Detection {
    /// How many candidates reached pricing.
    pub fn evaluated_count(&self) -> usize {
        self.opportunities.len() + self.rejected.len()
    }

    pub fn best(&self) -> Option<&Opportunity> {
        self.opportunities.first()
    }

    /// The most profitable round trip seen, whether or not it gained. This is
    /// what makes "no opportunity found" a reportable measurement (§28).
    pub fn best_peak(&self) -> Option<PathSimulation> {
        let mut best: Option<PathSimulation> = None;
        let seen = self
            .opportunities
            .iter()
            .map(|o| PathSimulation::new(o.path, o.input_amount, o.output_amount))
            .chain(self.rejected.iter().filter_map(|r| r.peak));
        for candidate in seen {
            best = Some(match best {
                None => candidate,
                Some(current) => {
                    let gained = PathSimulation::compare_profit(
                        (candidate.output, candidate.input),
                        (current.output, current.input),
                    )
                    .is_some_and(|ordering| ordering.is_gt());
                    if gained {
                        candidate
                    } else {
                        current
                    }
                }
            });
        }
        best
    }

    pub fn is_empty(&self) -> bool {
        self.opportunities.is_empty()
    }
}

/// Finds two-pool cycles in a graph snapshot and prices them.
#[derive(Clone, Copy, Debug)]
pub struct OpportunityDetector {
    policy: SearchPolicy,
}

impl Default for OpportunityDetector {
    fn default() -> Self {
        Self::new(SearchPolicy::default())
    }
}

impl OpportunityDetector {
    pub fn new(policy: SearchPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> SearchPolicy {
        self.policy
    }

    /// Enumerate and price every two-pool cycle in one block's market.
    pub fn detect(&self, snapshot: &GraphSnapshot) -> Result<Detection> {
        let (candidates, skipped_pairs) = scan(snapshot)?;
        let mut opportunities = Vec::new();
        let mut rejected = Vec::new();
        for path in &candidates {
            match self.evaluate(snapshot, *path) {
                Ok(best) => {
                    match Opportunity::new(snapshot.chain_id(), snapshot.block_number(), &best) {
                        Some(opportunity) => opportunities.push(opportunity),
                        None => rejected.push(CandidateRejection {
                            path: *path,
                            reason: RejectionReason::Unprofitable,
                            peak: Some(best.simulation),
                        }),
                    }
                }
                Err(error) => rejected.push(CandidateRejection {
                    path: *path,
                    reason: RejectionReason::classify(&error),
                    peak: None,
                }),
            }
        }
        // Profit descending, then route identity, so a tie can never be settled
        // by the order the collections happened to iterate in (§31, §32).
        opportunities.sort_by(|a, b| {
            b.gross_profit
                .cmp(&a.gross_profit)
                .then_with(|| a.path.cmp(&b.path))
        });
        rejected.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));
        Ok(Detection {
            chain_id: snapshot.chain_id(),
            block_number: snapshot.block_number(),
            candidates,
            opportunities,
            rejected,
            skipped_pairs,
            policy: self.policy,
        })
    }

    /// Price one named route against this market: find both hops in the graph,
    /// resolve their fees, and search for the best input.
    ///
    /// A route this graph does not contain is an error rather than a zero, so a
    /// stale or hand-written path can never be reported as "no profit".
    pub fn evaluate(
        &self,
        snapshot: &GraphSnapshot,
        path: ArbitragePath,
    ) -> Result<OptimizedCycle> {
        let [first, second] = path.hops();
        let first_edge = snapshot
            .edge(first)
            .ok_or(OpportunityError::MissingPool(first.pool))?;
        let second_edge = snapshot
            .edge(second)
            .ok_or(OpportunityError::MissingPool(second.pool))?;
        let cycle = PricedCycle::new(&first_edge, &second_edge)?;
        find_optimal_input(&cycle, self.policy)
    }
}

/// Every two-pool cycle this market contains, in a deterministic order.
///
/// The shape is `A -> pool1 -> B -> pool2 -> A` for every token pair and every
/// *ordered* pair of pools offering it (§18: the pair alone is not the market —
/// three pools on one pair give six directed routes, and reversing a route
/// changes which token is spent). Deduplicated by full hop identity, so one
/// route is listed once no matter which node the traversal reached it from.
pub fn enumerate_candidates(snapshot: &GraphSnapshot) -> Result<Vec<ArbitragePath>> {
    Ok(scan(snapshot)?.0)
}

/// One pass over the graph's routes: the candidate paths, and the pool pairs
/// refused by the path rules.
///
/// Both lists come out sorted by their own documented key — candidates by path
/// identity, refusals likewise — so no caller depends on which order the graph's
/// `BTreeSet` happened to yield. The traversal order only decides *when* a route
/// is first seen; it never survives into the result.
fn scan(snapshot: &GraphSnapshot) -> Result<(Vec<ArbitragePath>, Vec<SkippedPair>)> {
    let mut candidates: BTreeSet<ArbitragePath> = BTreeSet::new();
    let mut skipped: BTreeSet<SkippedPair> = BTreeSet::new();
    // Nodes, neighbours and routes all come out of `BTreeSet`/`BTreeMap` order,
    // so this traversal visits the same routes in the same sequence on every run
    // over the same snapshot.
    for token_in in snapshot.nodes() {
        for token_mid in snapshot.neighbors(*token_in) {
            for first in snapshot.routes(*token_in, *token_mid) {
                for second in snapshot.routes(*token_mid, *token_in) {
                    match ArbitragePath::two_hops(first.id, second.id) {
                        Ok(path) => {
                            candidates.insert(path);
                        }
                        Err(reason) => {
                            skipped.insert(SkippedPair {
                                first: first.id,
                                second: second.id,
                                reason,
                            });
                        }
                    };
                }
            }
        }
    }
    Ok((
        candidates.into_iter().collect(),
        skipped.into_iter().collect(),
    ))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address};
    use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};

    use crate::support::{graph, spec};

    use super::*;

    const CHAIN: ChainId = ChainId(7);
    const BLOCK: u64 = 100;
    const FEE: Fee = Fee {
        numerator: 997,
        denominator: 1000,
    };
    const A: Address = address!("0x000000000000000000000000000000000000000a");
    const B: Address = address!("0x000000000000000000000000000000000000000b");
    const C: Address = address!("0x000000000000000000000000000000000000000c");
    const P1: Address = address!("0x00000000000000000000000000000000000000f1");
    const P2: Address = address!("0x00000000000000000000000000000000000000f2");
    const P3: Address = address!("0x00000000000000000000000000000000000000f3");
    const P4: Address = address!("0x00000000000000000000000000000000000000f4");

    fn token(a: Address) -> TokenId {
        TokenId::new(CHAIN, a)
    }

    fn pool(a: Address) -> PoolId {
        PoolId::new(CHAIN, a)
    }

    #[test]
    fn an_unattested_fee_is_attributed_to_the_pool_that_lacks_it() {
        let error = OpportunityError::UnattestedFee(pool(B), token(A));
        assert_eq!(
            RejectionReason::classify(&error),
            RejectionReason::MissingFee(pool(B))
        );
    }

    #[test]
    fn a_route_absent_from_the_graph_is_an_error_not_a_zero() {
        let snapshot = graph(&[spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE))], BLOCK);
        let path = ArbitragePath::two_hops(
            Hop::new(pool(P1), token(A), token(B)),
            // Pool 2 is not in this graph at all.
            Hop::new(pool(P2), token(B), token(A)),
        )
        .expect("cycle");
        assert_eq!(
            OpportunityDetector::default().evaluate(&snapshot, path),
            Err(OpportunityError::MissingPool(pool(P2)))
        );
    }

    #[test]
    fn a_graph_with_one_pool_offers_no_route_and_says_why() {
        let snapshot = graph(&[spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE))], BLOCK);
        assert!(enumerate_candidates(&snapshot).expect("scan").is_empty());
        let detection = OpportunityDetector::default()
            .detect(&snapshot)
            .expect("detect");
        assert!(detection.opportunities.is_empty());
        assert!(detection.candidates.is_empty());
        assert_eq!(detection.evaluated_count(), 0);
        // The pool's own round trip shows up as a refusal, not as silence.
        assert_eq!(detection.skipped_pairs.len(), 2);
        assert!(detection
            .skipped_pairs
            .iter()
            .all(|skipped| skipped.reason == PathError::SamePool(pool(P1), pool(P1))));
    }

    /// A route reduced to what identifies it as a trade: the token spent, and
    /// the pools visited in order. Keeps the assertions below readable.
    fn route(path: &ArbitragePath) -> (TokenId, [PoolId; 2]) {
        (path.input_token(), path.pools())
    }

    #[test]
    fn two_pools_on_one_pair_offer_four_directed_routes_at_one_block() {
        let snapshot = graph(
            &[
                spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE)),
                spec(CHAIN, P2, B, A, 50_000, 125_000, Some(FEE)),
            ],
            BLOCK,
        );
        let detection = OpportunityDetector::default()
            .detect(&snapshot)
            .expect("detect");
        assert_eq!(detection.chain_id, snapshot.chain_id());
        assert_eq!(detection.block_number, BlockNumber(BLOCK));
        assert_eq!(detection.policy, SearchPolicy::default());
        // §18's loop is `for each token A / for each edge A -> B / for each edge
        // B -> A`, so two pools on one pair give four routes, not two. Each
        // ordered pool pair is priced from both ends, and the two versions are
        // different trades: one spends A, the other spends B. That is also why
        // dedup is on full hop identity — the rotations of one cycle are not
        // duplicates of it, they are the same cycle a different holder can take.
        // ... and the list itself is sorted by path identity, not by the order
        // the traversal found them.
        assert_eq!(
            detection.candidates.iter().map(route).collect::<Vec<_>>(),
            [
                (token(A), [pool(P1), pool(P2)]),
                (token(B), [pool(P1), pool(P2)]),
                (token(A), [pool(P2), pool(P1)]),
                (token(B), [pool(P2), pool(P1)]),
            ],
            "identity order, one entry per directed route"
        );
        assert_eq!(
            detection.skipped_pairs.len(),
            4,
            "each pool's own round trip, in both directions"
        );
        // The two routes that go the cheap way round gain; the two that go the
        // expensive way round lose. Same pair of pools, opposite verdicts.
        assert_eq!(detection.opportunities.len(), 2);
        assert_eq!(detection.rejected.len(), 2);
    }

    #[test]
    fn results_are_ordered_by_profit_then_route_and_reproduce_exactly() {
        let snapshot = graph(
            &[
                spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE)),
                spec(CHAIN, P2, B, A, 50_000, 125_000, Some(FEE)),
                spec(CHAIN, P3, A, C, 100_000, 50_000, Some(FEE)),
                spec(CHAIN, P4, C, A, 50_000, 200_000, Some(FEE)),
            ],
            BLOCK,
        );
        let detector = OpportunityDetector::default();
        let first = detector.detect(&snapshot).expect("detect");
        let second = detector.detect(&snapshot).expect("detect");
        assert_eq!(first, second, "the same graph gives the same report");
        // Two pairs of pools, four directed routes per pair, and each pair's two
        // cheap-way routes gain while its two expensive-way routes lose.
        assert_eq!(first.candidates.len(), 8);
        assert_eq!(first.opportunities.len(), 4);
        assert_eq!(first.rejected.len(), 4);
        assert_eq!(first.evaluated_count(), 8);
        for pair in first.opportunities.windows(2) {
            assert!(
                pair[0].gross_profit >= pair[1].gross_profit,
                "descending by profit"
            );
        }
        for opportunity in &first.opportunities {
            assert!(opportunity.gross_profit > U256::ZERO);
            assert_eq!(opportunity.input_token, opportunity.path.input_token());
            assert_eq!(opportunity.block_number, BlockNumber(BLOCK));
        }
        // {A,C} is quoted 2.0:1 against {A,B}'s 1.25:1, so both of its routes
        // have to outrank both of theirs.
        assert_eq!(
            first
                .opportunities
                .iter()
                .map(|o| route(&o.path))
                .collect::<Vec<_>>(),
            [
                (token(A), [pool(P3), pool(P4)]),
                (token(C), [pool(P4), pool(P3)]),
                (token(A), [pool(P1), pool(P2)]),
                (token(B), [pool(P2), pool(P1)]),
            ]
        );
        // The peak of this cycle is a plateau: an independent exact-integer scan
        // (see `tests/optimizer_cross_check.rs`) puts the 8441-unit top at inputs
        // 20372..=20815. Which input a run reports is the tie-break, so the
        // assertion is on the money and on the plateau, never on one point of it.
        let best = first.best().expect("a best");
        assert_eq!(best.gross_profit, U256::from(8_441u32));
        assert_eq!(
            best.output_amount - best.input_amount,
            best.gross_profit,
            "output and input must state the same profit"
        );
        assert!(
            (U256::from(20_372u32)..=U256::from(20_815u32)).contains(&best.input_amount),
            "the reported input is one of the exhaustive argmaxes: {}",
            best.input_amount
        );
    }

    #[test]
    fn best_peak_reports_the_near_miss_when_nothing_clears_zero() {
        // Pool 2 quotes B at 2.01 A, pool 1 at 2 A: a 0.5 % spread, against a
        // round trip that costs 0.6 %. Real price difference, no profit.
        let snapshot = graph(
            &[
                spec(CHAIN, P1, A, B, 100_000, 50_000, Some(FEE)),
                spec(CHAIN, P2, B, A, 50_000, 100_500, Some(FEE)),
            ],
            BLOCK,
        );
        let detection = OpportunityDetector::default()
            .detect(&snapshot)
            .expect("detect");
        assert!(detection.is_empty());
        assert_eq!(detection.rejected.len(), 4);
        assert!(
            detection
                .rejected
                .iter()
                .all(|r| r.reason == RejectionReason::Unprofitable),
            "nothing here failed for a technical reason"
        );
        let peak = detection.best_peak().expect("a peak was measured");
        assert!(peak.gross_profit().is_none(), "it must not read as profit");
        // ... and it is a near miss, not a collapse: a four-figure round trip
        // came back one unit short at its best input.
        assert!(peak.input > U256::from(100u32), "{peak:?}");
        assert_eq!(peak.gross_loss(), Some(U256::from(1u32)));
        for rejection in &detection.rejected {
            let candidate = rejection.peak.expect("priced");
            assert!(candidate.gross_profit().is_none(), "{candidate:?}");
        }
    }
}
