use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use evm_core::{
    BlockNumber, ChainId, EvidenceRef, EvidenceSource, LogIndex, PoolId, TokenId, TxHash, TxIndex,
};

/// Where an event was observed. Carried with every decoded event so a state
/// value can always be traced back to one log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogPosition {
    pub chain_id: ChainId,
    pub block_number: BlockNumber,
    pub tx_hash: TxHash,
    pub tx_index: TxIndex,
    pub log_index: LogIndex,
}

impl LogPosition {
    pub fn evidence(&self, signature: &str) -> EvidenceRef {
        EvidenceRef {
            source: EvidenceSource::ChainLog,
            block_number: Some(self.block_number),
            transaction_hash: Some(self.tx_hash),
            log_index: Some(self.log_index.0),
            signature: Some(signature.to_string()),
        }
    }
}

/// `Sync(uint112 reserve0, uint112 reserve1)` — the pair's own statement of its
/// balances. This is the authoritative state source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncEvent {
    pub position: LogPosition,
    pub pool: PoolId,
    pub reserve0: U256,
    pub reserve1: U256,
}

/// `Swap(...)` — flow only. Never a reserve source: amounts in/out describe one
/// trade, not the pair's resulting balances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapEvent {
    pub position: LogPosition,
    pub pool: PoolId,
    pub sender: Option<Address>,
    pub to: Option<Address>,
    pub amount0_in: U256,
    pub amount1_in: U256,
    pub amount0_out: U256,
    pub amount1_out: U256,
}

/// Factory-level pool creation notice, if the chain has one.
///
/// This is a *claim by the emitter*, and nothing more: it names an address that
/// was created, not an address that has been verified. M9.1 §6 makes that the
/// central rule of discovery (`Raw Log -> PairCreated -> CandidatePool`), and
/// `evm_discovery::CandidatePool` is what carries it forward into verification.
/// Decoding this event writes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolCreatedEvent {
    pub position: LogPosition,
    pub factory: Address,
    pub pool: PoolId,
    pub token0: TokenId,
    pub token1: TokenId,
    /// `pairIndex`, the factory's own counter of what it has created.
    pub pair_index: U256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolEvent {
    Sync(SyncEvent),
    Swap(SwapEvent),
    PoolCreated(PoolCreatedEvent),
}

impl ProtocolEvent {
    pub fn position(&self) -> LogPosition {
        match self {
            Self::Sync(e) => e.position,
            Self::Swap(e) => e.position,
            Self::PoolCreated(e) => e.position,
        }
    }

    pub fn pool(&self) -> PoolId {
        match self {
            Self::Sync(e) => e.pool,
            Self::Swap(e) => e.pool,
            Self::PoolCreated(e) => e.pool,
        }
    }
}
