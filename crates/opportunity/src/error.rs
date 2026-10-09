//! Every way this crate can refuse a calculation, and why it refuses out loud.
//!
//! Market data is external input: a reserve can be zero, a fee can be
//! unattested, a product can exceed 256 bits. None of those are allowed to
//! panic, and none of them are allowed to be answered with a guessed number, so
//! each has a name and the caller decides what to do about it.

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{BlockNumber, ChainId, PoolId, TokenId};
use evm_graph::EdgeId;

/// Arithmetically ill-defined or unrepresentable swap input.
///
/// Ordered by variant so a rejection list is sortable, and `Copy` so it can be
/// reported per candidate without cloning anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, thiserror::Error)]
pub enum MathError {
    #[error("a reserve of zero prices nothing: division by it is undefined")]
    InvalidReserve,
    #[error("an input amount of zero buys nothing")]
    InvalidAmount,
    #[error(
        "the fee ratio is not a usable fraction (zero denominator, or more retained than paid)"
    )]
    InvalidFee,
    #[error("the intermediate product does not fit in 256 bits")]
    Overflow,
}

/// A path that is not a two-pool cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, thiserror::Error)]
pub enum PathError {
    #[error("v0.1 prices exactly 2-hop cycles, this path has {0} hops")]
    HopCount(usize),
    #[error("{0:?} and {1:?} are the same pool: a round trip through one pool is not a cross-market arbitrage")]
    SamePool(PoolId, PoolId),
    #[error("hop {0:?} pays out {1:?} but the next hop does not buy it back")]
    TokenMismatch(PoolId, TokenId),
    #[error("the path does not return to its input token {0:?}, so it is not a cycle")]
    NotACycle(TokenId),
}

/// A trail of edges that is not a route.
///
/// [`PathError`] is M3's refusal of a two-hop shape and [`MathError`] is
/// arithmetic's refusal; this is the third kind — the shape of a trail of *any*
/// length. It is its own enum rather than more `OpportunityError` variants
/// because M3's rejection taxonomy ([`crate::RejectionReason`]) is a closed
/// statement about two-pool pricing, and a multi-hop trail that does not close
/// is not a new way for M3 to fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, thiserror::Error)]
pub enum RouteError {
    #[error(
        "a round trip needs at least {minimum} pools, this one names {found}: one pool traded \
         against itself is not a cross-market finding"
    )]
    TooFewHops { found: usize, minimum: usize },
    #[error(
        "edge {0:?} is not in this graph snapshot, so the route is over a market nobody showed"
    )]
    EdgeNotInGraph(EdgeId),
    #[error(
        "the candidate claims chain {chain_id:?} at block {target_block:?} with {hops} hops and \
         this snapshot is not that one — a route quoted against another block's reserves is a \
         different market"
    )]
    CandidateNotInGraph {
        chain_id: ChainId,
        target_block: BlockNumber,
        hops: usize,
    },
    #[error("hop {index} trades {token:?} against itself, which no attested pool can do")]
    SelfLoop { index: usize, token: TokenId },
    #[error(
        "hop {index} pays out {token:?} again: a route that holds one token twice before closing \
         is two cycles walked as one, and the shorter one is the finding"
    )]
    RepeatedToken { index: usize, token: TokenId },
    #[error(
        "hop {index} spends {pool:?} twice: a round trip through one pool is not cross-market"
    )]
    RepeatedPool { index: usize, pool: PoolId },
    #[error(
        "hop {index} buys {found:?} but the hop before it paid out {expected:?}, so the trail is \
         not walkable"
    )]
    BrokenContinuity {
        index: usize,
        expected: TokenId,
        found: TokenId,
    },
    #[error("the route spent {start:?} and came back with {ended_on:?}, so it is not a cycle")]
    NotACycle { start: TokenId, ended_on: TokenId },
    #[error(
        "the route returns to {start:?} before its last hop ({hops} hops), so it is a closed \
         trail plus a tail"
    )]
    ClosedEarly { start: TokenId, hops: usize },
}

/// Anything this crate is asked to compute and cannot.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpportunityError {
    #[error(transparent)]
    Path(PathError),
    #[error(transparent)]
    Math(MathError),
    /// M11's shape refusal, carried as one variant so M3's taxonomy stays the
    /// closed list of two-pool failures it already is.
    #[error(transparent)]
    Route(RouteError),
    #[error("pool {0:?} is in the path but not in this graph snapshot")]
    MissingPool(PoolId),
    #[error("pool {0:?} has no state in this graph snapshot")]
    MissingState(PoolId),
    #[error(
        "pool {0:?} carries no attested fee, and a fee of `None` is not a fee of zero \
         ({1:?} is priced against it)"
    )]
    UnattestedFee(PoolId, TokenId),
    #[error(
        "the search domain is empty: lower bound {lower:?} already exceeds upper bound {upper:?}"
    )]
    EmptySearchDomain { lower: U256, upper: U256 },
}

pub type Result<T> = std::result::Result<T, OpportunityError>;

/// What the M11 route constructor answers with. [`OpportunityError`]'s `From`
/// impl lets a route failure travel into a `Result` unchanged, so a caller that
/// prices and a caller that only builds a route can each name what they got.
pub type RouteResult<T> = std::result::Result<T, RouteError>;

impl From<PathError> for OpportunityError {
    fn from(error: PathError) -> Self {
        Self::Path(error)
    }
}

impl From<MathError> for OpportunityError {
    fn from(error: MathError) -> Self {
        Self::Math(error)
    }
}

impl From<RouteError> for OpportunityError {
    fn from(error: RouteError) -> Self {
        Self::Route(error)
    }
}
