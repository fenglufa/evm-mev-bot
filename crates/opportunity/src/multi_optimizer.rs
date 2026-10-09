//! §13–§18: the amount search over a multi-hop route.
//!
//! M3's ternary search is not reused here, and the reason is the objective
//! function, not the code. Two hops compose to a fractional-linear function whose
//! discrete derivative changes sign once, so the profit sequence is unimodal and
//! narrowing an interval is sound. Compose three or more hops and floor each hop
//! in between and the sequence is a staircase with notches: a local improvement
//! does not imply the peak lies that way. §13 names this as the place M11 is most
//! likely to get wrong, and the way to get it wrong is to keep the old search and
//! the old claim. So this module searches a *bounded set of points* and reports
//! what it did, and never says "global optimum" (§46's rule that an approximate
//! optimum is reported as one).
//!
//! What is guaranteed:
//!
//! * the candidate space is finite by construction — a grid of at most
//!   [`OptimizationPolicy::MAX_COARSE_POINTS`] points plus a refinement window of
//!   at most `2 * refine_span + 1`, or a walk of at most
//!   [`OptimizationPolicy::MAX_EXHAUSTIVE_WIDTH`] inputs when the domain is
//!   smaller than that. There is no `while profit improves` loop (§15), and both
//!   phases terminate by counting and by bounds, not by judging;
//! * the search is a pure function of `(route, domain, policy)` — same route,
//!   same numbers, so a replay reproduces it (§12);
//! * the route is never modified (§17): every point is evaluated through
//!   [`crate::multihop::price`] on the same `&MultiHopRoute`, and the result
//!   carries that route's quote;
//! * the best round trip keeps all three states of [`Gross`]. A loss is not
//!   written as the number zero, which is why [`OptimizationResult::gross`] —
//!   not [`OptimizationResult::best_profit`] — is the field that answers "did it
//!   pay?" (§4).
//!
//! Ties go to the smaller input, in both phases, so an exhaustive run and a
//! brute-force scan agree on `best_input` and not merely on the profit (§16).

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId};

use crate::error::{OpportunityError, Result};
use crate::multihop::{price, Gross, MultiHopQuote, MultiHopRoute, Price};

/// How the peak was looked for. Two strategies exist, and both are exact about
/// what they evaluated; naming one in the result is what keeps a reported number
/// from reading like a proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum OptimizationStrategy {
    /// Every input in the domain, in order. Taken when the domain is no wider
    /// than [`OptimizationPolicy::exhaustive_limit`], which is the only case in
    /// which this module may call its answer the domain's maximum.
    Exhaustive,
    /// A uniform grid across the domain, then every input inside
    /// `best ± refine_span`. Approximate by construction on a wide domain.
    CoarseThenRefine,
}

/// What ended the search: whether every input was compared, or only a window
/// around a sampled grid point. §14 asks for a termination reason, and the two
/// states are the difference between "this is the peak" and "this is the best I
/// looked at". A point the arithmetic refused does not change the answer here —
/// it is counted in [`OptimizationResult::refusals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Termination {
    /// The domain was walked input by input.
    DomainExhausted,
    /// The grid's best point had a refinement window, and that window was walked
    /// in full.
    WindowRefined,
}

/// The knobs of [`optimize`]. Every one is echoed back in the result, so no
/// search limit hides in the code, and every one is clamped to the documented
/// cap, so a caller cannot ask for a search that is not bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OptimizationPolicy {
    /// Grid points across the domain, at most [`Self::MAX_COARSE_POINTS`].
    pub coarse_points: u64,
    /// Inputs on each side of the grid's best point that get compared one by
    /// one, at most [`Self::MAX_REFINE_SPAN`].
    pub refine_span: u64,
    /// Domains no wider than this are walked entirely instead of sampled, at
    /// most [`Self::MAX_EXHAUSTIVE_WIDTH`].
    pub exhaustive_limit: u64,
}

impl OptimizationPolicy {
    /// The caps are part of the promise §15 makes: the candidate space is
    /// finite, and its size is a function of these numbers and nothing else.
    pub const MAX_COARSE_POINTS: u64 = 1_024;
    pub const MAX_REFINE_SPAN: u64 = 1_024;
    pub const MAX_EXHAUSTIVE_WIDTH: u64 = 65_536;

    /// A policy that walks any domain of a few thousand inputs exactly, and
    /// otherwise samples 256 grid points and closes a 64-wide window.
    pub fn default_search() -> Self {
        Self {
            coarse_points: 256,
            refine_span: 64,
            exhaustive_limit: 4_096,
        }
    }

    /// The policy with every knob inside its documented cap. A `coarse_points`
    /// of zero would divide by zero, so it becomes one point — the grid
    /// degenerates to its left end plus a refinement window.
    pub fn clamped(&self) -> Self {
        Self {
            coarse_points: self.coarse_points.clamp(1, Self::MAX_COARSE_POINTS),
            refine_span: self.refine_span.min(Self::MAX_REFINE_SPAN),
            exhaustive_limit: self.exhaustive_limit.min(Self::MAX_EXHAUSTIVE_WIDTH),
        }
    }
}

impl Default for OptimizationPolicy {
    fn default() -> Self {
        Self::default_search()
    }
}

/// The best input the search found, with the search behind it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OptimizationResult {
    /// The input that paid best, in the route's input token.
    pub best_input: U256,
    /// What came back out at that input.
    pub best_output: U256,
    /// [`Gross::gain`] of the best round trip, so a `0` here means "this did not
    /// pay" and is *not* a statement that it broke even. Read [`Self::gross`]
    /// for the three-way answer (§4).
    pub best_profit: U256,
    /// The best round trip's market result, in its three states.
    pub gross: Gross,
    /// The full quote at [`Self::best_input`]: every hop that ran, so a later
    /// layer can build legs from the numbers that produced this claim (§11).
    pub quote: MultiHopQuote,
    /// Inputs priced successfully.
    pub evaluations: u64,
    /// Inputs the arithmetic refused — an intermediate product that left 256
    /// bits. A missing fee is not counted here; it stops the search, see
    /// [`OpportunityError::UnattestedFee`].
    pub refusals: u64,
    /// The domain actually searched: the caller's window, clipped to what the
    /// route can price at all, i.e. `1 ..= exit reserve - 1`.
    pub domain_min: U256,
    pub domain_max: U256,
    pub strategy: OptimizationStrategy,
    pub policy: OptimizationPolicy,
    pub termination: Termination,
}

impl OptimizationResult {
    /// Did the search reach the route's own ceiling? An exhaustive run that
    /// covered up to [`MultiHopRoute::input_upper_bound`] has seen every input
    /// the route can be asked about, which is the strongest statement this
    /// module is allowed to make.
    pub fn covered_the_route_domain(&self, route: &MultiHopRoute) -> bool {
        self.strategy == OptimizationStrategy::Exhaustive
            && route
                .input_upper_bound()
                .is_some_and(|ceiling| self.domain_max >= ceiling)
    }
}

/// A route plus the amount this module thinks is best for it. §18: this is not
/// an opportunity — nothing has been simulated, no gas has been priced, no risk
/// has spoken — and the name is the guard against reading it as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizedCandidate {
    pub chain_id: ChainId,
    pub target_block: BlockNumber,
    pub route: MultiHopRoute,
    pub search: OptimizationResult,
}

impl OptimizedCandidate {
    /// Bind a search result to the route it was run on. The pairing is not
    /// decorative: [`MultiHopRoute`] carries the block its reserves belong to,
    /// so a candidate cannot be re-dated downstream.
    pub fn new(route: MultiHopRoute, search: OptimizationResult) -> Self {
        Self {
            chain_id: route.chain_id(),
            target_block: route.target_block(),
            route,
            search,
        }
    }

    pub fn hop_count(&self) -> usize {
        self.route.hop_count()
    }

    pub fn gross(&self) -> Gross {
        self.search.gross
    }
}

/// The domain the search will actually cover: the caller's window, clipped to
/// `1 ..= route.input_upper_bound()`. `None` when the two do not meet, which is
/// the same empty-domain answer M3 gives for an exit pool holding one unit.
pub fn search_domain(
    route: &MultiHopRoute,
    search_min: U256,
    search_max: U256,
) -> Option<(U256, U256)> {
    let ceiling = route.input_upper_bound()?;
    let low = if search_min.is_zero() {
        U256::ONE
    } else {
        search_min
    };
    let high = search_max.min(ceiling);
    (low <= high).then_some((low, high))
}

/// Search `search_min ..= search_max` (as clipped by [`search_domain`]) for the
/// input that pays best on `route`, and report the winner with the search that
/// produced it.
///
/// An unattested fee on the route stops the whole thing with
/// [`OpportunityError::UnattestedFee`]: the missing number is not input-dependent,
/// so no sampled point could have been priced either, and a search that returned
/// "no peak" would be a search that invented a market.
pub fn optimize(
    route: &MultiHopRoute,
    search_min: U256,
    search_max: U256,
    policy: OptimizationPolicy,
) -> Result<OptimizedCandidate> {
    let policy = policy.clamped();
    let empty = || OpportunityError::EmptySearchDomain {
        lower: search_min,
        upper: search_max,
    };
    let (domain_min, domain_max) =
        search_domain(route, search_min, search_max).ok_or_else(empty)?;
    // `domain_min >= 1`, so the width cannot reach past `U256::MAX` and the two
    // checked operations below are guarding arithmetic, not a judgement.
    let width = domain_max
        .checked_sub(domain_min)
        .and_then(|span| span.checked_add(U256::ONE))
        .ok_or_else(empty)?;

    let mut search = Search {
        route,
        evaluations: 0,
        refusals: 0,
        best: None,
    };

    // The exhaustive branch first: a domain small enough to walk is walked, and
    // that is the only branch allowed to call its answer the maximum. A width
    // that does not fit `u64` is by definition larger than the limit, so the
    // conversion failing means "sample", not "give up".
    let walkable = u64::try_from(width)
        .ok()
        .filter(|count| *count <= policy.exhaustive_limit);
    let strategy = match walkable {
        Some(count) => {
            for step in 0..count {
                let input = domain_min.checked_add(U256::from(step)).ok_or_else(empty)?;
                search.consider(input)?;
            }
            OptimizationStrategy::Exhaustive
        }
        None => {
            // `ceil(width / coarse_points)`, so the grid reaches the far end of
            // the domain however wide it is.
            let step = width
                .checked_add(U256::from(policy.coarse_points))
                .and_then(|padded| padded.checked_sub(U256::ONE))
                .and_then(|padded| padded.checked_div(U256::from(policy.coarse_points)))
                .ok_or_else(empty)?;
            let mut input = domain_min;
            let mut sampled = 0u64;
            while sampled < policy.coarse_points && input <= domain_max {
                search.consider(input)?;
                sampled += 1;
                match input.checked_add(step) {
                    Some(next) => input = next,
                    None => break,
                }
            }
            // A grid with nothing priced on it has no window to refine around.
            // The refusal is stated at the end, once, from the counts.
            if let Some(grid_best) = search.best.as_ref().map(|point| point.input) {
                let span = U256::from(policy.refine_span);
                let window_low = grid_best.saturating_sub(span).max(domain_min);
                let window_high = grid_best.saturating_add(span).min(domain_max);
                let window_width = window_high
                    .checked_sub(window_low)
                    .and_then(|delta| delta.checked_add(U256::ONE))
                    .ok_or_else(empty)?;
                let steps = u64::try_from(window_width).map_err(|_| empty())?;
                for step in 0..steps {
                    let input = window_low.checked_add(U256::from(step)).ok_or_else(empty)?;
                    search.consider(input)?;
                }
            }
            OptimizationStrategy::CoarseThenRefine
        }
    };

    // `Search::consider` only leaves `best` empty when it counted a refusal for
    // every input it was given: an unattested fee already returned, and a priced
    // input would have been taken as the first candidate. So an empty best here is
    // the arithmetic's answer, not a missing window — §48 says it stays a refusal
    // with that name rather than becoming a domain of zero width.
    let best = search
        .best
        .ok_or(OpportunityError::Math(crate::error::MathError::Overflow))?;
    Ok(OptimizedCandidate::new(
        route.clone(),
        OptimizationResult {
            best_input: best.input,
            best_output: best.output,
            best_profit: best.gross.gain().unwrap_or(U256::ZERO),
            gross: best.gross,
            quote: best.quote,
            evaluations: search.evaluations,
            refusals: search.refusals,
            domain_min,
            domain_max,
            strategy,
            termination: match strategy {
                OptimizationStrategy::Exhaustive => Termination::DomainExhausted,
                OptimizationStrategy::CoarseThenRefine => Termination::WindowRefined,
            },
            policy,
        },
    ))
}

/// One evaluated input.
#[derive(Clone, Debug)]
struct Evaluated {
    input: U256,
    output: U256,
    gross: Gross,
    quote: MultiHopQuote,
}

/// The accumulator both phases share, so "better" is decided in exactly one
/// place: more profit wins, and a tie goes to the smaller input.
struct Search<'a> {
    route: &'a MultiHopRoute,
    evaluations: u64,
    refusals: u64,
    best: Option<Evaluated>,
}

impl Search<'_> {
    /// Price one input and take it if it wins. A missing fee ends the search
    /// with an error; an overflow is counted and skipped, because it is
    /// input-dependent — it says the domain reaches beyond a 256-bit product,
    /// not that the market is unpriceable.
    fn consider(&mut self, input: U256) -> Result<()> {
        match price(self.route, input) {
            Price::Quoted { quote, gross } => {
                self.evaluations += 1;
                let challenger = Evaluated {
                    input,
                    output: quote.output,
                    gross,
                    quote,
                };
                let take = match &self.best {
                    None => true,
                    Some(current) => {
                        beats(gross, current.gross)
                            || (gross == current.gross && challenger.input < current.input)
                    }
                };
                if take {
                    self.best = Some(challenger);
                }
                Ok(())
            }
            Price::Incomplete {
                pool,
                token_in,
                hop_index: _,
            } => Err(OpportunityError::UnattestedFee(pool, token_in)),
            Price::Unpriceable(_) => {
                self.refusals += 1;
                Ok(())
            }
        }
    }
}

/// Is `winner` a better round trip than `runner`? Written as a match rather than
/// a subtraction because `U256` has no negative: a smaller loss does beat a
/// larger one, and that is the only ordering that keeps a search from preferring
/// a catastrophe.
fn beats(winner: Gross, runner: Gross) -> bool {
    match (winner, runner) {
        (Gross::Gain(a), Gross::Gain(b)) => a > b,
        (Gross::Gain(_), _) => true,
        (Gross::Even, Gross::Gain(_)) | (Gross::Even, Gross::Even) => false,
        (Gross::Even, Gross::Loss(_)) => true,
        (Gross::Loss(a), Gross::Loss(b)) => a < b,
        (Gross::Loss(_), _) => false,
    }
}
