use async_trait::async_trait;

use evm_core::{BlockNumber, ChainId};

use crate::error::Result;
use crate::types::{BlockData, CallRequest, ChainBlock, ChainLog, LogFilter};

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

    async fn get_logs(&self, filter: LogFilter) -> Result<Vec<ChainLog>>;

    /// Read-only state access. Allowed for initialization, validation and
    /// recovery — never as the normal source of pool reserves.
    async fn call(&self, at: BlockNumber, request: &CallRequest)
        -> Result<alloy_primitives::Bytes>;
}
