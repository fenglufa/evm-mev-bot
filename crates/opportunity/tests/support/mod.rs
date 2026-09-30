//! Plumbing shared by the M3 integration tests.
//!
//! Two jobs here. `market()` builds a [`GraphSnapshot`] the same way the real
//! pipeline does — an `InMemoryStateStore` fed register/sync updates, projected
//! by the graph builder — so no test gets to invent a market shape the system
//! never produces. And `quote`/`peak` are a **second implementation** of the AMM
//! arithmetic, written against `u128` while the crate works in `U256`, so an
//! optimizer result can be checked by something that is not the optimizer
//! (§46: the search may not validate itself).
//!
//! The oracle scans the whole derived domain `1 ..= second.reserve_out - 1` and
//! reports the maximum profit together with the *width* of the input plateau that
//! achieves it. A search that lands anywhere on that plateau and reports the same
//! profit has agreed with the scan; a search that reports a higher profit than
//! the scan, or an input outside the domain, has contradicted it, and that is a
//! bug in either of them.

#![allow(dead_code)]

use alloy_primitives::{address, Address, U256};

use evm_core::{
    BlockNumber, ChainId, Fee, LogIndex, PoolId, PoolMeta, PoolType, ProtocolId, TokenId,
};
use evm_graph::{GraphEdge, GraphSnapshot, MarketGraphBuilder};
use evm_state::{InMemoryStateStore, StateStore, StateUpdate, UpdatePosition};

pub const CHAIN: ChainId = ChainId(7);
pub const BLOCK: u64 = 100;

/// The retained fraction the fee evidence reproduces on 94/94 real trades.
pub const FEE: Fee = Fee {
    numerator: 997,
    denominator: 1000,
};
/// A fee that is 0 % of the input, i.e. nothing withheld.
pub const NO_FEE: Fee = Fee {
    numerator: 1,
    denominator: 1,
};

pub const A: Address = address!("0x000000000000000000000000000000000000000a");
pub const B: Address = address!("0x000000000000000000000000000000000000000b");
pub const C: Address = address!("0x000000000000000000000000000000000000000c");
pub const P1: Address = address!("0x00000000000000000000000000000000000000f1");
pub const P2: Address = address!("0x00000000000000000000000000000000000000f2");
pub const P3: Address = address!("0x00000000000000000000000000000000000000f3");
pub const P4: Address = address!("0x00000000000000000000000000000000000000f4");
pub const P5: Address = address!("0x00000000000000000000000000000000000000f5");
pub const P6: Address = address!("0x00000000000000000000000000000000000000f6");

pub fn token(a: Address) -> TokenId {
    TokenId::new(CHAIN, a)
}

pub fn pool(a: Address) -> PoolId {
    PoolId::new(CHAIN, a)
}

/// A pool as an attestation states it: which two tokens, whose order decides
/// which reserve is which, and whatever fee has been proved for it.
pub struct Spec {
    pub pool: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: u128,
    pub reserve1: u128,
    pub fee: Option<Fee>,
}

pub fn spec(
    pool: Address,
    token0: Address,
    token1: Address,
    reserve0: u128,
    reserve1: u128,
    fee: Option<Fee>,
) -> Spec {
    Spec {
        pool,
        token0,
        token1,
        reserve0,
        reserve1,
        fee,
    }
}

/// Register every pool, then sync it, and project the result at `block`.
pub fn market_at(block: u64, pools: &[Spec]) -> GraphSnapshot {
    let mut store = InMemoryStateStore::new(CHAIN);
    for pool in pools {
        let meta = PoolMeta {
            id: evm_core::PoolId::new(CHAIN, pool.pool),
            protocol: ProtocolId::new("test-v2"),
            token0: token(pool.token0),
            token1: token(pool.token1),
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
                pool: evm_core::PoolId::new(CHAIN, pool.pool),
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

pub fn market(pools: &[Spec]) -> GraphSnapshot {
    market_at(BLOCK, pools)
}

/// The one side of a fixture pool that a route entering on `entering` trades
/// through, as `(reserve_in, reserve_out)`. Derived from the attestation's own
/// `token0`/`token1` order, so a test that enumerates routes cannot re-orient a
/// price by hand and then check the crate against its own mistake.
pub fn side(pool: &Spec, entering: Address) -> (u128, u128) {
    if entering == pool.token0 {
        (pool.reserve0, pool.reserve1)
    } else {
        assert_eq!(
            entering, pool.token1,
            "the route enters a pool on one of its two tokens"
        );
        (pool.reserve1, pool.reserve0)
    }
}

/// The token a route ends up holding after entering `pool` on `entering`.
pub fn other(pool: &Spec, entering: Address) -> Address {
    if entering == pool.token0 {
        pool.token1
    } else {
        pool.token0
    }
}

/// Both directions of one fixture pool, straight from its attestation. Used by
/// the tests that price a named route rather than scanning a whole market.
pub fn edges(
    pool_addr: Address,
    token0: Address,
    token1: Address,
    r0: u128,
    r1: u128,
) -> [GraphEdge; 2] {
    let meta = PoolMeta {
        id: pool(pool_addr),
        protocol: ProtocolId::new("test-v2"),
        token0: token(token0),
        token1: token(token1),
        fee: Some(FEE),
        pool_type: PoolType::ConstantProduct,
    };
    let state = evm_core::PoolState {
        pool: pool(pool_addr),
        reserve0: U256::from(r0),
        reserve1: U256::from(r1),
        block_number: BlockNumber(BLOCK),
        log_index: LogIndex(1),
    };
    GraphEdge::pair(&meta, &state).expect("a fixture pool has two sides")
}

// ---------------------------------------------------------------------------
// The independent oracle. `u128`, written from the formula in §11 of the task,
// with no reference to anything in the crate under test.
// ---------------------------------------------------------------------------

/// One hop: `out = (x * n * Ro) / (Ri * d + x * n)`, integer division.
pub fn quote(reserve_in: u128, reserve_out: u128, x: u128) -> u128 {
    quote_at(reserve_in, reserve_out, x, FEE)
}

/// The same quote at whatever fee the market has been attested at.
pub fn quote_at(reserve_in: u128, reserve_out: u128, x: u128, fee: Fee) -> u128 {
    let retained = x * fee.numerator as u128;
    (retained * reserve_out) / (reserve_in * fee.denominator as u128 + retained)
}

/// A two-hop round trip at one input: what comes back in the original token.
pub fn round_trip(first: (u128, u128), second: (u128, u128), x: u128) -> u128 {
    round_trip_at(first, second, x, FEE)
}

pub fn round_trip_at(first: (u128, u128), second: (u128, u128), x: u128, fee: Fee) -> u128 {
    let middle = quote_at(first.0, first.1, x, fee);
    if middle == 0 {
        return 0;
    }
    quote_at(second.0, second.1, middle, fee)
}

/// The best round trip over the whole derived domain.
#[derive(Debug)]
pub struct Peak {
    /// `output - input` at the best inputs; negative means nothing profits.
    pub profit: i128,
    /// Smallest and largest input reaching `profit`, and how many inputs do.
    /// Maximisers need not be contiguous — the floored curve is a staircase with
    /// dips between its top steps — so this is a span plus a count, not a run.
    pub first_input: u128,
    pub last_input: u128,
    pub plateau: u128,
    /// `second.reserve_out - 1`, the domain the scan covered.
    pub domain_end: u128,
}

impl Peak {
    pub fn contains(&self, input: u128) -> bool {
        input >= self.first_input && input <= self.last_input
    }
}

/// Exhaustively evaluate every input in `1 ..= second.1 - 1`, at the attested
/// 997/1000.
pub fn peak(first: (u128, u128), second: (u128, u128)) -> Peak {
    peak_at(first, second, FEE)
}

/// The same exhaustive scan at any fee, used by the controls that price a
/// fixture with the fee removed.
pub fn peak_at(first: (u128, u128), second: (u128, u128), fee: Fee) -> Peak {
    let domain_end = second.1 - 1;
    let mut best: Option<i128> = None;
    let mut first_input = 0u128;
    let mut last_input = 0u128;
    let mut plateau = 0u128;
    for x in 1..=domain_end {
        let profit = round_trip_at(first, second, x, fee) as i128 - x as i128;
        match best {
            None => {
                best = Some(profit);
                first_input = x;
                last_input = x;
                plateau = 1;
            }
            Some(current) => {
                if profit > current {
                    best = Some(profit);
                    first_input = x;
                    last_input = x;
                    plateau = 1;
                } else if profit == current {
                    last_input = x;
                    plateau += 1;
                }
            }
        }
    }
    Peak {
        profit: best.expect("a domain of at least one input"),
        first_input,
        last_input,
        plateau,
        domain_end,
    }
}

/// The price product of a route: `(Ro1 * Ro2) / (Ri1 * Ri2)` as an exact ratio.
/// A route can profit at some input if and only if this beats
/// [`fee_threshold_squared`], which is the algebra behind §44's "real price
/// difference": it is not a screening heuristic, it is the condition, and the
/// exhaustive scan in [`peak`] agrees with it on every fixture here.
pub fn price_product(first: (u128, u128), second: (u128, u128)) -> (u128, u128) {
    (first.1 * second.1, first.0 * second.0)
}

/// `(fee_denominator / fee_numerator)²` as a ratio: the price product a route
/// has to beat before fees, which for 997/1000 is 1 000 000 / 994 009.
pub fn fee_threshold_squared() -> (u128, u128) {
    (
        FEE.denominator as u128 * FEE.denominator as u128,
        FEE.numerator as u128 * FEE.numerator as u128,
    )
}

/// `a/b > c/d` without dividing.
pub fn ratio_gt(a: (u128, u128), c: (u128, u128)) -> bool {
    a.0 * c.1 > c.0 * a.1
}
