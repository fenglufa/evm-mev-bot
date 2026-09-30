use alloy_primitives::{Address, U256};
use async_trait::async_trait;

use evm_core::{BlockNumber, ChainId};

use crate::error::Result;
use crate::types::{BlockContext, BlockData, CallRequest, ChainBlock, ChainLog, LogFilter};

/// Chain-facing interface the rest of the system is allowed to see.
///
/// Implementations must return records already ordered by on-chain execution
/// position, and must never expose provider types.
#[async_trait]
pub trait ChainAdapter: Send + Sync {
    fn chain_id(&self) -> ChainId;

    async fn latest_block(&self) -> Result<BlockNumber>;

    async fn get_block(&self, number: BlockNumber) -> Result<ChainBlock>;

    /// Header + transactions + receipts of one block, ordered.
    async fn get_block_data(&self, number: BlockNumber) -> Result<BlockData>;

    /// Header fields an execution has to be measured against. Every state read
    /// a simulation performs is pinned to the same height this came from.
    async fn get_block_context(&self, number: BlockNumber) -> Result<BlockContext>;

    async fn get_logs(&self, filter: LogFilter) -> Result<Vec<ChainLog>>;

    /// Read-only state access. Allowed for initialization, validation and
    /// recovery — never as the normal source of pool reserves.
    async fn call(&self, at: BlockNumber, request: &CallRequest)
        -> Result<alloy_primitives::Bytes>;

    /// Deployed bytecode at `at`. Empty bytes are a legitimate answer (an
    /// address with no code); a provider that cannot serve history must error.
    async fn get_code(&self, at: BlockNumber, address: Address) -> Result<alloy_primitives::Bytes>;

    async fn get_balance(&self, at: BlockNumber, address: Address) -> Result<U256>;

    /// One storage word at `at`. `slot` is the raw 256-bit key.
    async fn get_storage_at(&self, at: BlockNumber, address: Address, slot: U256) -> Result<U256>;

    /// Account nonce at `at`. A simulation has to assign the sender's nonces the
    /// way the chain would, and that starts from what the chain says they are.
    async fn get_nonce(&self, at: BlockNumber, address: Address) -> Result<u64>;
}
