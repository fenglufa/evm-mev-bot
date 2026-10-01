//! Test-only market builder.
//!
//! Fixtures here go through the same door the real pipeline does — register a
//! pool, sync its reserves into the state store, project the result with the
//! graph builder — so a fixture graph cannot be a shape the real path never
//! produces. Nothing in this module constructs a `GraphSnapshot` by hand.

use alloy_primitives::{address, Address, U256};

use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};
use evm_graph::{EdgeId, GraphSnapshot, MarketGraphBuilder};

use crate::detector::Opportunity;
use crate::optimizer::{PricedHop, SearchPolicy, SearchRecord, SearchStrategy};
use crate::path::ArbitragePath;
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

/// The two tokens every hand-built finding in these tests trades between.
const TOKEN_A: Address = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
const TOKEN_B: Address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

/// One finding, assembled field by field.
///
/// Deliberately not built through [`graph`] and the detector: the lifecycle tests
/// reason about *identity* (chain, block, the two pools, the direction) and about
/// the state version a finding was priced at, and neither of those is a property of
/// the AMM arithmetic. Routing them through a real priced graph would mean arranging
/// reserves that make four different pool orders each profitable, which tests M3
/// again rather than the staleness rule. Anything that does test pricing goes
/// through [`graph`] and the detector, as it does in `detector.rs` and `optimizer.rs`.
pub(crate) fn test_opportunity(
    chain: ChainId,
    block: BlockNumber,
    first: PoolId,
    second: PoolId,
    input: U256,
) -> Opportunity {
    let token_a = TokenId::new(chain, TOKEN_A);
    let token_b = TokenId::new(chain, TOKEN_B);
    let path = ArbitragePath::two_hops(
        EdgeId::new(first, token_a, token_b),
        EdgeId::new(second, token_b, token_a),
    )
    .expect("a fixture cycle closes by construction");
    let fee = Fee::new(3, 1_000).expect("0.3% is a fee");
    let hop = |pool: PoolId, reserve_in: u64, reserve_out: u64| PricedHop {
        pool,
        reserve_in: U256::from(reserve_in),
        reserve_out: U256::from(reserve_out),
        fee,
    };
    Opportunity {
        chain_id: chain,
        block_number: block,
        path,
        input_token: token_a,
        input_amount: input,
        output_amount: input + U256::from(1u64),
        gross_profit: U256::from(1u64),
        hops: [hop(first, 1_000, 2_000), hop(second, 2_000, 1_000)],
        search: SearchRecord {
            strategy: SearchStrategy::BoundedTernary,
            rounds: 1,
            evaluations: 1,
            lower_bound: U256::from(1u64),
            upper_bound: input.max(U256::from(1u64)),
            interval_closed: true,
            scan_count: 1,
            policy: SearchPolicy::default(),
        },
    }
}
