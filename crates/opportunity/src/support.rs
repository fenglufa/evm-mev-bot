//! Test-only market builder.
//!
//! Fixtures here go through the same door the real pipeline does — register a
//! pool, sync its reserves into the state store, project the result with the
//! graph builder — so a fixture graph cannot be a shape the real path never
//! produces. Nothing in this module constructs a `GraphSnapshot` by hand.

use alloy_primitives::{Address, U256};

use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};
use evm_graph::{GraphSnapshot, MarketGraphBuilder};
use evm_state::{InMemoryStateStore, StateStore, StateUpdate, UpdatePosition};

/// A pool stated at one block: identity, both tokens, both reserves in the
/// pool's own `token0`/`token1` order, and whatever fee has been proved for it.
pub(crate) struct Spec {
    pub chain: ChainId,
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: u128,
    pub reserve1: u128,
    pub fee: Option<Fee>,
}

pub(crate) fn spec(
    chain: ChainId,
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0: u128,
    reserve1: u128,
    fee: Option<Fee>,
) -> Spec {
    Spec {
        chain,
        pool,
        token0,
        token1,
        reserve0,
        reserve1,
        fee,
    }
}

/// Every pool in `pools` priced at `block`, as one graph.
///
/// Log positions increase across the list, which is what the state store
/// requires and what the graph then binds the block to.
pub(crate) fn graph(pools: &[Spec], block: u64) -> GraphSnapshot {
    let chain = pools
        .first()
        .expect("a fixture graph needs at least one pool")
        .chain;
    let mut store = InMemoryStateStore::new(chain);
    for pool in pools {
        let meta = PoolMeta {
            id: PoolId::new(pool.chain, pool.pool),
            protocol: ProtocolId::new("test-v2"),
            token0: TokenId::new(pool.chain, pool.token0),
            token1: TokenId::new(pool.chain, pool.token1),
            fee: pool.fee,
            pool_type: PoolType::ConstantProduct,
        };
        store
            .apply(StateUpdate::PoolRegistered(meta))
            .expect("register a fixture pool");
    }
    for (index, pool) in pools.iter().enumerate() {
        store
            .apply(StateUpdate::PoolSynced {
                pool: PoolId::new(pool.chain, pool.pool),
                reserve0: U256::from(pool.reserve0),
                reserve1: U256::from(pool.reserve1),
                position: UpdatePosition::new(BlockNumber(block), LogIndex(index as u64 + 1)),
            })
            .expect("sync a fixture pool");
    }
    MarketGraphBuilder::new()
        .build(&store.snapshot())
        .expect("project the fixture graph")
}
