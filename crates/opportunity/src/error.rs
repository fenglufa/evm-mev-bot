//! Every way this crate can refuse a calculation, and why it refuses out loud.
//!
//! Market data is external input: a reserve can be zero, a fee can be
//! unattested, a product can exceed 256 bits. None of those are allowed to
//! panic, and none of them are allowed to be answered with a guessed number, so
//! each has a name and the caller decides what to do about it.

use alloy_primitives::U256;
use serde::Serialize;

use evm_core::{PoolId, TokenId};

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

/// Anything this crate is asked to compute and cannot.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpportunityError {
    #[error(transparent)]
    Path(PathError),
    #[error(transparent)]
    Math(MathError),
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
