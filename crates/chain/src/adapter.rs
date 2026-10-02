use std::sync::Arc;

use alloy_primitives::{Address, U256};
use async_trait::async_trait;

use evm_core::{BlockNumber, ChainId};

use crate::error::Result;
use crate::rpc_trace::RpcTraceSink;
use crate::types::{BlockContext, BlockData, CallRequest, ChainBlock, ChainLog, LogFilter};

/// Chain-facing interface the rest of the system is allowed to see.
///
/// Implementations must return records already ordered by on-chain execution
/// position, and must never expose provider types.
#[async_trait]
pub trait ChainAdapter: Send + Sync {
    fn chain_id(&self) -> ChainId;

    /// This adapter again, with every provider call it makes recorded into `sink`,
    /// or `None` when the source has no calls to record.
    ///
    /// M8.2 §6 needs each call attributable to one simulation and §31 forbids a global
    /// collector, so the handle is handed down from whoever starts the simulation
    /// rather than installed once per process. The default is `None` — which is not a
    /// failure to instrument but a statement about the source: a recorded directory
    /// answers from memory and issues no requests at all, so its RPC timeline is empty
    /// by fact, and §9 requires that to be reported as nothing measured rather than as
    /// zero milliseconds. A caller that gets `None` back says so in the evidence
    /// instead of reporting a count it never observed.
    fn with_rpc_trace(&self, _sink: RpcTraceSink) -> Option<Arc<dyn ChainAdapter>> {
        None
    }

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

/// One adapter, held by two owners.
///
/// The live pipeline needs the *same* adapter answering the block reads on the
/// state path and the state reads inside a simulation, and §62's one-provider
/// rule is a lot easier to hold when there is literally one object than when a
/// report claims two clones happen to point at the same backend. `Arc` is also
/// what lets the pipeline be generic over `dyn ChainAdapter` — so the recorded
/// directory and the live node travel through the same ingestion code (§9).
#[async_trait]
impl<T: ChainAdapter + ?Sized> ChainAdapter for std::sync::Arc<T> {
    fn chain_id(&self) -> ChainId {
        (**self).chain_id()
    }

    fn with_rpc_trace(&self, sink: RpcTraceSink) -> Option<Arc<dyn ChainAdapter>> {
        (**self).with_rpc_trace(sink)
    }

    async fn latest_block(&self) -> Result<BlockNumber> {
        (**self).latest_block().await
    }

    async fn get_block(&self, number: BlockNumber) -> Result<ChainBlock> {
        (**self).get_block(number).await
    }

    async fn get_block_data(&self, number: BlockNumber) -> Result<BlockData> {
        (**self).get_block_data(number).await
    }

    async fn get_block_context(&self, number: BlockNumber) -> Result<BlockContext> {
        (**self).get_block_context(number).await
    }

    async fn get_logs(&self, filter: LogFilter) -> Result<Vec<ChainLog>> {
        (**self).get_logs(filter).await
    }

    async fn call(
        &self,
        at: BlockNumber,
        request: &CallRequest,
    ) -> Result<alloy_primitives::Bytes> {
        (**self).call(at, request).await
    }

    async fn get_code(&self, at: BlockNumber, address: Address) -> Result<alloy_primitives::Bytes> {
        (**self).get_code(at, address).await
    }

    async fn get_balance(&self, at: BlockNumber, address: Address) -> Result<U256> {
        (**self).get_balance(at, address).await
    }

    async fn get_storage_at(&self, at: BlockNumber, address: Address, slot: U256) -> Result<U256> {
        (**self).get_storage_at(at, address, slot).await
    }

    async fn get_nonce(&self, at: BlockNumber, address: Address) -> Result<u64> {
        (**self).get_nonce(at, address).await
    }
}
