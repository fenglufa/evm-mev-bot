use thiserror::Error;

use crate::update::UpdatePosition;

/// Every rejection is an explicit error, never a panic and never a silent skip:
/// chain data is untrusted input, and state that quietly disagrees with the
/// chain is worse than state that refuses to move.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StateError {
    #[error("store serves chain {store}, the update names chain {update}")]
    ChainMismatch { store: u64, update: u64 },

    #[error("pool {0} has no registered metadata: reserves cannot be applied to an unknown pool")]
    UnregisteredPool(String),

    #[error("pool {pool} synced empty reserves ({reserve0} / {reserve1}) at {position}")]
    InvalidReserves {
        pool: String,
        reserve0: String,
        reserve1: String,
        position: UpdatePosition,
    },

    #[error("pool {pool} update at {attempted} does not follow the stored {previous}")]
    OutOfOrder {
        pool: String,
        previous: UpdatePosition,
        attempted: UpdatePosition,
    },

    #[error("state moved backwards: applied {attempted} after {previous}")]
    Regression {
        previous: UpdatePosition,
        attempted: UpdatePosition,
    },

    #[error("pool {0} is already registered with different metadata")]
    ConflictingRegistration(String),
}

pub type Result<T> = std::result::Result<T, StateError>;
