use alloy_primitives::U256;
use serde::{Deserialize, Serialize};

use crate::fee::Fee;
use crate::identity::{BlockNumber, LogIndex, PoolId, ProtocolId, TokenId};

/// Static, slowly-changing identity of a pool.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PoolMeta {
    pub id: PoolId,
    pub protocol: ProtocolId,
    pub token0: TokenId,
    pub token1: TokenId,
    pub fee: Option<Fee>,
    pub pool_type: PoolType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoolType {
    ConstantProduct,
    Unknown,
}

/// Dynamic state, observed at an exact on-chain position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PoolState {
    pub pool: crate::identity::PoolId,
    pub reserve0: U256,
    pub reserve1: U256,
    pub block_number: BlockNumber,
    pub log_index: LogIndex,
}

impl PoolState {
    /// A pool with an empty side has no priced pair and must never reach the graph.
    pub fn has_valid_reserves(&self) -> bool {
        !self.reserve0.is_zero() && !self.reserve1.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;
    use crate::identity::{ChainId, TxIndex};

    fn pool() -> PoolId {
        PoolId::new(
            ChainId(91342),
            address!("0x00000000000000000000000000000000000000aa"),
        )
    }

    #[test]
    fn zero_reserve_is_invalid() {
        let state = PoolState {
            pool: pool(),
            reserve0: U256::ZERO,
            reserve1: U256::from(7u32),
            block_number: BlockNumber(1),
            log_index: LogIndex(0),
        };
        assert!(!state.has_valid_reserves());
    }

    #[test]
    fn u256_reserves_survive_json_round_trip_without_precision_loss() {
        let state = PoolState {
            pool: pool(),
            reserve0: U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, 7]),
            reserve1: U256::from(1u32),
            block_number: BlockNumber(1),
            log_index: LogIndex(0),
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<PoolState>(&json).unwrap(), state);
    }

    #[test]
    fn pool_state_ordering_is_onchain_position_not_hash() {
        let mut keys = [
            (BlockNumber(2), TxIndex(0), LogIndex(0)),
            (BlockNumber(1), TxIndex(2), LogIndex(0)),
            (BlockNumber(1), TxIndex(1), LogIndex(9)),
            (BlockNumber(1), TxIndex(1), LogIndex(3)),
        ];
        keys.sort();
        assert_eq!(
            keys,
            [
                (BlockNumber(1), TxIndex(1), LogIndex(3)),
                (BlockNumber(1), TxIndex(1), LogIndex(9)),
                (BlockNumber(1), TxIndex(2), LogIndex(0)),
                (BlockNumber(2), TxIndex(0), LogIndex(0)),
            ]
        );
    }
}
