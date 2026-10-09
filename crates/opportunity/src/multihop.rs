//! A closed route of any hop count, priced hop by hop in exact integers.
//!
//! M3 prices two pools and M9.3 searches for cycles of two and three pools. This
//! module is where those two meet, and where both stop: a [`MultiHopRoute`] is a
//! route that has been *read out of a graph snapshot* and proven to be a closed
//! trail of distinct pools, and it is the object the M11 optimizer and the M11
//! simulation work against. It is deliberately not a `CycleCandidate` with more
//! fields — §6 of the task forbids widening M9.3's semantics, because a topology
//! finding that carries an amount would let a graph walk claim it priced
//! something it never priced. So the candidate stays as it is and this type is
//! the adapter over it:
//!
//! ```text
//! CycleCandidate (topology, M9.3, frozen)
//!        |
//!        v  MultiHopRoute::from_candidate
//! MultiHopRoute  = edges + the market data those edges name in this snapshot
//! ```
//!
//! # What the route is allowed to know
//!
//! Reserves and fees, because without them there is nothing to quote — and
//! nothing else. No amount belongs on a route: §8 asks that the route and the
//! amount be separate identities, so that an optimizer searching over input
//! sizes can never be read as having changed the market it searches. That is why
//! the fields below are private and no method takes `&mut self`: a
//! [`MultiHopRoute`] is built or it is not built, and quoting it does not touch
//! it.
//!
//! The hop count is not capped here. The bound that matters — the executor's
//! `MAX_LEGS` — belongs to the contract and is enforced when a route becomes a
//! plan, which is where the number lives; a price over five pools is arithmetic
//! and needs no permission, while a *call* over five pools is a revert the
//! contract refuses on its own.
//!
//! # Identity, and why presentation order is not it
//!
//! `A -> B -> C -> A` walked from `B` is the same route entered one seat to the
//! left. [`MultiHopRoute::identity`] is the lexicographically smallest rotation
//! of the edge sequence, which is M9.3's `CanonicalKey` rule restated on the
//! data this crate already holds; the trade-order edges stay visible as
//! [`MultiHopRoute::edges`], because pricing has to follow the direction the
//! reserves were oriented in. Two routes with one identity are one finding.
//!
//! # The arithmetic
//!
//! One hop is [`crate::math::swap_exact_in`] and nothing else, so the search
//! here and a hand quote of the same pool can never drift apart. Composition is
//! a fold: hop *i* spends what hop *i-1* produced, whole, with no rounding
//! between hops beyond the floor each hop's own formula takes. A hop that buys
//! less than one whole unit hands over zero and the round trip ends there — that
//! is a market outcome (an input too small to move), recorded as an output of
//! zero, not an error.
//!
//! A fee of `None` is not a fee of zero. M2 attests a pool's fee or it leaves it
//! unknown, and pricing an unknown fee would mean inventing a number and calling
//! the result profit, so [`Price::Incomplete`] names the pool and token that are
//! unproved and answers nothing else.

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId, Fee, PoolId, TokenId};
use evm_graph::{EdgeId, GraphEdge, GraphSnapshot};

use crate::error::{MathError, RouteError, RouteResult};
use crate::math::swap_exact_in;

/// One edge of a route: the pool direction plus the fee it is priced at.
///
/// `fee` is `Option` for the same reason [`GraphEdge::fee`] is — an unattested
/// fee is a hole in the evidence, not a value — and it is kept per hop rather
/// than folded into one route-level flag so that [`Price::Incomplete`] can name
/// the pool nobody proved instead of pointing at the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RouteHop {
    pub pool: PoolId,
    pub token_in: TokenId,
    pub token_out: TokenId,
    pub reserve_in: U256,
    pub reserve_out: U256,
    pub fee: Option<Fee>,
}

impl RouteHop {
    fn from_edge(edge: &GraphEdge) -> Self {
        Self {
            pool: edge.pool(),
            token_in: edge.token_in(),
            token_out: edge.token_out(),
            reserve_in: edge.reserve_in,
            reserve_out: edge.reserve_out,
            fee: edge.fee,
        }
    }
}

/// The rotation-invariant identity of a route: the smallest rotation of its
/// edges, in trade order from there.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RouteIdentity(Vec<EdgeId>);

impl RouteIdentity {
    fn of(edges: &[EdgeId]) -> Self {
        let len = edges.len();
        let mut best: Vec<EdgeId> = edges.to_vec();
        if len >= 2 {
            for shift in 1..len {
                let rotation: Vec<EdgeId> = (shift..shift + len).map(|i| edges[i % len]).collect();
                if rotation < best {
                    best = rotation;
                }
            }
        }
        Self(best)
    }

    /// The edges as the identity sorts them — smallest edge first, then trade
    /// order from there.
    pub fn edges(&self) -> &[EdgeId] {
        &self.0
    }

    /// The token the route is entered at once rotations are folded. `None` only
    /// for an identity with no edges, which no constructor produces and no
    /// caller has to assume away.
    pub fn start_token(&self) -> Option<TokenId> {
        self.0.first().map(|edge| edge.token_in)
    }

    pub fn hop_count(&self) -> usize {
        self.0.len()
    }
}

/// A closed trail of distinct pools, read from one snapshot at one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiHopRoute {
    chain_id: ChainId,
    target_block: BlockNumber,
    identity: RouteIdentity,
    input_token: TokenId,
    hops: Vec<RouteHop>,
}

impl MultiHopRoute {
    /// The smallest closed trail this type accepts. One edge is a token traded
    /// against itself through one pool, which M2 already refuses and M9.3
    /// already refuses; repeating the floor here is what lets the type be the
    /// boundary rather than a polite suggestion.
    pub const MIN_HOPS: usize = 2;

    /// Build a route from edge identities, resolving each against `snapshot`.
    ///
    /// Resolution is part of construction: an `EdgeId` that this snapshot does
    /// not carry is a route over a market nobody showed, so it is refused rather
    /// than defaulted. All edges come out of one snapshot, which is what makes
    /// "the same block" a checked fact rather than a caller's promise.
    pub fn new(snapshot: &GraphSnapshot, edges: &[EdgeId]) -> RouteResult<Self> {
        if edges.len() < Self::MIN_HOPS {
            return Err(RouteError::TooFewHops {
                found: edges.len(),
                minimum: Self::MIN_HOPS,
            });
        }
        let mut hops = Vec::with_capacity(edges.len());
        for id in edges {
            let edge = snapshot.edge(*id).ok_or(RouteError::EdgeNotInGraph(*id))?;
            hops.push(RouteHop::from_edge(&edge));
        }
        Self::assemble(snapshot.chain_id(), snapshot.block_number(), edges, hops)
    }

    /// The M9.3 adapter: the candidate's edges, resolved against the graph they
    /// were found in.
    ///
    /// The candidate's own chain and block are carried over rather than re-read,
    /// and the two have to agree — [`evm_pathfinder::CycleCandidate::belongs_to`]
    /// is asked first, so a candidate quoted against a graph from another block
    /// is refused here instead of being priced at the wrong reserves. Nothing
    /// about the candidate changes: this reads `edges` and nothing else.
    pub fn from_candidate(
        snapshot: &GraphSnapshot,
        candidate: &evm_pathfinder::CycleCandidate,
    ) -> RouteResult<Self> {
        if !candidate.belongs_to(snapshot) {
            return Err(RouteError::CandidateNotInGraph {
                chain_id: candidate.chain_id,
                target_block: candidate.target_block,
                hops: candidate.edges.len(),
            });
        }
        Self::new(snapshot, &candidate.edges)
    }

    /// Validate a resolved hop list and freeze it. Both constructors come
    /// through here, so §7's rules live in one place rather than in whichever
    /// door the caller used.
    fn assemble(
        chain_id: ChainId,
        target_block: BlockNumber,
        ids: &[EdgeId],
        hops: Vec<RouteHop>,
    ) -> RouteResult<Self> {
        let too_few = || RouteError::TooFewHops {
            found: hops.len(),
            minimum: Self::MIN_HOPS,
        };
        let (Some(first), Some(last)) = (hops.first(), hops.last()) else {
            return Err(too_few());
        };
        let start = first.token_in;
        let closing = hops.len() - 1;
        let mut pools = vec![first.pool];
        // The tokens the route has held, in order. Continuity means every hop's
        // input is either the start or the previous hop's output, so a repeated
        // token can only show up as a repeated *output* — which is why the rule
        // below looks at `token_out` and not at both ends.
        let mut held = vec![start];
        for (index, hop) in hops.iter().enumerate() {
            if hop.token_in == hop.token_out {
                return Err(RouteError::SelfLoop {
                    index,
                    token: hop.token_in,
                });
            }
            if index > 0 {
                if pools.contains(&hop.pool) {
                    return Err(RouteError::RepeatedPool {
                        index,
                        pool: hop.pool,
                    });
                }
                pools.push(hop.pool);
                let previous = &hops[index - 1];
                if previous.token_out != hop.token_in {
                    return Err(RouteError::BrokenContinuity {
                        index,
                        expected: previous.token_out,
                        found: hop.token_in,
                    });
                }
            }
            if hop.token_out == start {
                // Returning to the input token is legal exactly once, on the last
                // hop. Arriving early means the route is two cycles walked as one,
                // and the shorter one is the finding (M9.3 applies the same rule).
                if index != closing {
                    return Err(RouteError::ClosedEarly {
                        start,
                        hops: hops.len(),
                    });
                }
            } else if held.contains(&hop.token_out) {
                return Err(RouteError::RepeatedToken {
                    index,
                    token: hop.token_out,
                });
            }
            held.push(hop.token_out);
        }
        if last.token_out != start {
            return Err(RouteError::NotACycle {
                start,
                ended_on: last.token_out,
            });
        }
        Ok(Self {
            chain_id,
            target_block,
            identity: RouteIdentity::of(ids),
            input_token: start,
            hops,
        })
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    /// The block the snapshot is taken at. Part of the route's identity as a
    /// statement about a market: the same edges at another block are other
    /// reserves, and §23 asks a simulation to be unable to lose this number.
    pub fn target_block(&self) -> BlockNumber {
        self.target_block
    }

    pub fn identity(&self) -> &RouteIdentity {
        &self.identity
    }

    /// The hops in trade order, first to last. A slice, not a `Vec`: there is no
    /// way for a caller to push a hop onto a route that exists.
    pub fn edges(&self) -> &[RouteHop] {
        &self.hops
    }

    pub fn hop_count(&self) -> usize {
        self.hops.len()
    }

    /// The token spent at the start and received again at the end.
    pub fn input_token(&self) -> TokenId {
        self.input_token
    }

    pub fn pools(&self) -> Vec<PoolId> {
        self.hops.iter().map(|hop| hop.pool).collect()
    }

    pub fn tokens(&self) -> Vec<TokenId> {
        let mut tokens: Vec<TokenId> = self.hops.iter().map(|hop| hop.token_in).collect();
        if let Some(last) = self.hops.last() {
            tokens.push(last.token_out);
        }
        tokens
    }

    /// Every fee on the route has been attested, which is what makes it
    /// priceable. `false` is not "the fees are zero" — it names the missing
    /// evidence.
    pub fn is_priceable(&self) -> bool {
        self.hops.iter().all(|hop| hop.fee.is_some())
    }

    /// The first hop whose fee nobody proved, in trade order.
    pub fn unattested(&self) -> Option<(PoolId, TokenId)> {
        self.hops
            .iter()
            .find(|hop| hop.fee.is_none())
            .map(|hop| (hop.pool, hop.token_in))
    }

    /// The derived ceiling of the input domain: `last.reserve_out - 1`.
    ///
    /// The same derivation M3 makes for two hops, and it holds for any number of
    /// them: what comes back out of the last pool is strictly less than that
    /// pool's reserve of the input token, so an input at or above that reserve is
    /// guaranteed to lose. It is a bound on the *domain*, not on the peak — the
    /// peak sits inside it, which is what [`crate::multi_optimizer`] searches
    /// for. `None` when the exit pool holds at most one unit, i.e. no domain at
    /// all.
    pub fn input_upper_bound(&self) -> Option<U256> {
        let reserve_out = self.hops.last()?.reserve_out;
        let upper = reserve_out.checked_sub(U256::ONE)?;
        (upper >= U256::ONE).then_some(upper)
    }
}

/// One hop's share of a quote: what it was given and what it handed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HopQuote {
    pub pool: PoolId,
    pub token_in: TokenId,
    pub token_out: TokenId,
    pub amount_in: U256,
    pub amount_out: U256,
    /// The fee this hop was priced at, recorded per hop so an audit of a number
    /// can see which pool's attestation produced it (§9 forbids a single
    /// hard-coded ratio, and this is the field that proves it did not happen).
    pub fee: Fee,
}

/// What the route answers for one input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MultiHopQuote {
    pub input: U256,
    pub output: U256,
    /// Every hop that ran, in order. A round trip that came back with 95 of the
    /// input token is only auditable as `A -100-> B -97-> C -95-> A` if those
    /// intermediate numbers are kept (§11).
    pub hops: Vec<HopQuote>,
    /// `true` when a hop bought less than one whole unit and the round trip
    /// stopped there: the remaining hops are not in `hops`, and the output is
    /// zero because nothing reached them.
    pub truncated: bool,
}

impl MultiHopQuote {
    /// The route's fees, in trade order, read off the quote rather than
    /// restated: §43's independent recompute needs to see that two different
    /// pools were priced at their own two fees.
    pub fn fees(&self) -> Vec<Fee> {
        self.hops.iter().map(|hop| hop.fee).collect()
    }

    pub fn amount_out_of_hop(&self, index: usize) -> Option<U256> {
        self.hops.get(index).map(|hop| hop.amount_out)
    }
}

/// The round trip's market result in the input token's own units.
///
/// Three states because U256 cannot spell a negative, and §4's first principle
/// is that a loss must not be written as the number zero — that number already
/// means "exactly even", and a search that confuses them optimises the wrong
/// thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Gross {
    Gain(U256),
    Even,
    Loss(U256),
}

impl Gross {
    pub fn between(input: U256, output: U256) -> Self {
        match output.cmp(&input) {
            std::cmp::Ordering::Greater => Self::Gain(output - input),
            std::cmp::Ordering::Equal => Self::Even,
            std::cmp::Ordering::Less => Self::Loss(input - output),
        }
    }

    /// The amount by which the round trip pays, if it pays at all. This is the
    /// only form of profit M11 pricing may state; gas, slippage and realized
    /// results are other levels' numbers (§4).
    pub fn gain(&self) -> Option<U256> {
        match self {
            Self::Gain(amount) => Some(*amount),
            _ => None,
        }
    }

    pub fn is_gain(&self) -> bool {
        matches!(self, Self::Gain(_))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Gain(_) => "gain",
            Self::Even => "even",
            Self::Loss(_) => "loss",
        }
    }
}

/// A route's answer to one input, or the reason it has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Price {
    /// The round trip's numbers.
    Quoted { quote: MultiHopQuote, gross: Gross },
    /// A fee on the route has never been attested. Neither a quote nor a
    /// refusal: the route is a real shape in the graph and the missing evidence
    /// is named (§9).
    Incomplete {
        pool: PoolId,
        token_in: TokenId,
        hop_index: usize,
    },
    /// The arithmetic cannot be stated: a zero reserve, an amount that overflows
    /// the fee product, or a zero input.
    Unpriceable(MathError),
}

impl Price {
    pub fn quote(&self) -> Option<&MultiHopQuote> {
        match self {
            Self::Quoted { quote, .. } => Some(quote),
            _ => None,
        }
    }

    pub fn gross(&self) -> Option<Gross> {
        match self {
            Self::Quoted { gross, .. } => Some(*gross),
            _ => None,
        }
    }

    /// A short, stable name for the three states, so an evidence row and a test
    /// can speak about the same outcome without matching on the payload.
    pub fn state(&self) -> &'static str {
        match self {
            Self::Quoted { .. } => "quoted",
            Self::Incomplete { .. } => "incomplete",
            Self::Unpriceable(_) => "unpriceable",
        }
    }
}

/// Quote `input` of the route's input token all the way round.
///
/// Pure: the inputs are the route's reserves and fees and the amount, so the
/// same route at the same input produces the same quote in the same process and
/// in another one (§12). No clock, no node, no floating point.
pub fn price(route: &MultiHopRoute, input: U256) -> Price {
    if input.is_zero() {
        return Price::Unpriceable(MathError::InvalidAmount);
    }
    let mut hops = Vec::with_capacity(route.hop_count());
    let mut amount = input;
    for (index, hop) in route.edges().iter().enumerate() {
        let Some(fee) = hop.fee else {
            return Price::Incomplete {
                pool: hop.pool,
                token_in: hop.token_in,
                hop_index: index,
            };
        };
        match swap_exact_in(hop.reserve_in, hop.reserve_out, fee, amount) {
            Ok(out) => {
                hops.push(HopQuote {
                    pool: hop.pool,
                    token_in: hop.token_in,
                    token_out: hop.token_out,
                    amount_in: amount,
                    amount_out: out,
                    fee,
                });
                if out.is_zero() {
                    // Nothing to feed the next hop with, and the round trip is
                    // over: zero out, not an error (§11's smallest candidates
                    // have to stay in the enumeration).
                    return Price::Quoted {
                        quote: MultiHopQuote {
                            input,
                            output: U256::ZERO,
                            hops,
                            truncated: index + 1 < route.hop_count(),
                        },
                        gross: Gross::between(input, U256::ZERO),
                    };
                }
                amount = out;
            }
            Err(error) => return Price::Unpriceable(error),
        }
    }
    let output = amount;
    Price::Quoted {
        quote: MultiHopQuote {
            input,
            output,
            hops,
            truncated: false,
        },
        gross: Gross::between(input, output),
    }
}
