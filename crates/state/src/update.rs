use std::fmt::{self, Display, Formatter};

use alloy_primitives::U256;
use evm_core::{BlockNumber, LogIndex, PoolId, PoolMeta};
use serde::{Deserialize, Serialize};

/// Where in the chain a state update was observed.
///
/// `log_index` is the block-global log index the chain reports, so
/// `(block_number, log_index)` is already a total order over state-changing
/// events: it increases across transactions inside a block, not only within one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UpdatePosition {
    pub block_number: BlockNumber,
    pub log_index: LogIndex,
}

impl UpdatePosition {
    pub fn new(block_number: BlockNumber, log_index: LogIndex) -> Self {
        Self {
            block_number,
            log_index,
        }
    }
}

impl Display for UpdatePosition {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "block {} log {}", self.block_number.0, self.log_index.0)
    }
}

/// The only way anything may enter the store.
///
/// Protocol events are never applied directly: they are first turned into one of
/// these, so replay and live feed the store through the same door.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateUpdate {
    /// A pool whose identity is attested becomes known to the store. Metadata
    /// and reserves are deliberately separate: one is stable, the other changes
    /// on every sync.
    PoolRegistered(PoolMeta),

    /// The authoritative reserve statement of a pool. Only a pool's own state
    /// event can produce this — never a swap.
    PoolSynced {
        pool: PoolId,
        reserve0: U256,
        reserve1: U256,
        position: UpdatePosition,
    },
}

impl StateUpdate {
    pub fn pool(&self) -> PoolId {
        match self {
            Self::PoolRegistered(meta) => meta.id,
            Self::PoolSynced { pool, .. } => *pool,
        }
    }
}
