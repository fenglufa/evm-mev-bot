use std::fmt::{self, Display, Formatter};

use alloy_primitives::{Address, B256};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlockNumber(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxIndex(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogIndex(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TxHash(pub B256);

impl Display for TxHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

/// Chain-scoped identity. A bare `Address` is never a global identity: the same
/// address on two chains denotes two different objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TokenId {
    pub chain_id: ChainId,
    pub address: Address,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PoolId {
    pub chain_id: ChainId,
    pub address: Address,
}

impl TokenId {
    pub const fn new(chain_id: ChainId, address: Address) -> Self {
        Self { chain_id, address }
    }
}

impl PoolId {
    pub const fn new(chain_id: ChainId, address: Address) -> Self {
        Self { chain_id, address }
    }
}

/// Identifies the concrete protocol semantics a pool was decoded with.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolId(pub String);

impl ProtocolId {
    pub fn new(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl Display for ProtocolId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    #[test]
    fn same_address_on_different_chains_is_different_identity() {
        let addr = address!("0x0000000000000000000000000000000000000001");
        let a = TokenId::new(ChainId(1), addr);
        let b = TokenId::new(ChainId(91342), addr);
        assert_ne!(a, b);
    }

    #[test]
    fn identity_round_trips_through_json() {
        let id = PoolId::new(
            ChainId(91342),
            address!("0x0000000000000000000000000000000000000001"),
        );
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<PoolId>(&json).unwrap(), id);
    }
}
