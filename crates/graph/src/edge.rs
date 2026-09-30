use alloy_primitives::U256;
use serde::{Deserialize, Serialize};

use evm_core::{Fee, PoolId, TokenId};
use evm_state::UpdatePosition;

/// A directed market route. `token_in -> token_out` is not enough to identify
/// one: `A -> B` through two different pools are two markets, and a search has
/// to be able to name them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EdgeId {
    pub pool: PoolId,
    pub token_in: TokenId,
    pub token_out: TokenId,
}

impl EdgeId {
    pub const fn new(pool: PoolId, token_in: TokenId, token_out: TokenId) -> Self {
        Self {
            pool,
            token_in,
            token_out,
        }
    }
}

/// One direction of one pool, at one exact block.
///
/// The reserves are the pool's own `token0`/`token1` reserves, re-oriented so
/// that `reserve_in` belongs to `token_in`. That re-orientation is the whole job
/// of this type, and getting it backwards would silently invert every price, so
/// `GraphEdge::pair` builds both directions from the same source in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: EdgeId,
    pub reserve_in: U256,
    pub reserve_out: U256,
    /// The pool's attested fee. `None` means nobody has proven it, which is not
    /// the same thing as zero.
    pub fee: Option<Fee>,
    /// Chain position of the authoritative state event these reserves came from.
    pub state_position: UpdatePosition,
}

impl PartialOrd for GraphEdge {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Edges are ordered by identity first, since an `EdgeId` is unique, then by the
/// remaining fields so that ordering can never disagree with equality.
/// `Fee` deliberately has no `Ord` — comparing numerators would not compare
/// fees — so it enters as its raw ratio parts.
impl Ord for GraphEdge {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

/// A pool that cannot price a pair, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum EdgeRejection {
    /// One side of the pool is empty. That is not a cheap market, it is no
    /// market: every output amount computed against it would be a division by
    /// nothing.
    EmptySide,
    /// `token0 == token1` cannot come from a real attestation, and a route from
    /// a token to itself would let a search price nothing while looking like an
    /// edge.
    SameToken,
}

impl GraphEdge {
    /// Both directions of a pool, from its metadata and its latest state.
    ///
    /// Both come out of one call on purpose: re-orienting `reserve0`/`reserve1`
    /// to `token_in`/`token_out` is this type's whole job, and getting it
    /// backwards in one direction would silently invert every price downstream.
    pub fn pair(
        meta: &evm_core::PoolMeta,
        state: &evm_core::PoolState,
    ) -> Result<[GraphEdge; 2], EdgeRejection> {
        if meta.token0 == meta.token1 {
            return Err(EdgeRejection::SameToken);
        }
        if !state.has_valid_reserves() {
            return Err(EdgeRejection::EmptySide);
        }
        let position = UpdatePosition::new(state.block_number, state.log_index);
        Ok([
            GraphEdge {
                id: EdgeId::new(meta.id, meta.token0, meta.token1),
                reserve_in: state.reserve0,
                reserve_out: state.reserve1,
                fee: meta.fee,
                state_position: position,
            },
            GraphEdge {
                id: EdgeId::new(meta.id, meta.token1, meta.token0),
                reserve_in: state.reserve1,
                reserve_out: state.reserve0,
                fee: meta.fee,
                state_position: position,
            },
        ])
    }

    pub fn pool(&self) -> PoolId {
        self.id.pool
    }

    pub fn token_in(&self) -> TokenId {
        self.id.token_in
    }

    pub fn token_out(&self) -> TokenId {
        self.id.token_out
    }

    pub fn flip(&self) -> GraphEdge {
        GraphEdge {
            id: EdgeId::new(self.id.pool, self.id.token_out, self.id.token_in),
            reserve_in: self.reserve_out,
            reserve_out: self.reserve_in,
            fee: self.fee,
            state_position: self.state_position,
        }
    }

    fn sort_key(&self) -> (EdgeId, UpdatePosition, U256, U256, Option<(u32, u32)>) {
        (
            self.id,
            self.state_position,
            self.reserve_in,
            self.reserve_out,
            self.fee.map(|fee| (fee.numerator, fee.denominator)),
        )
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, Address, U256};

    use super::*;
    use evm_core::{
        BlockNumber, ChainId, LogIndex, PoolId, PoolMeta, PoolState, PoolType, ProtocolId, TokenId,
    };

    const CHAIN: ChainId = ChainId(7);
    const A: Address = address!("0x000000000000000000000000000000000000000a");
    const B: Address = address!("0x000000000000000000000000000000000000000b");
    const P: Address = address!("0x000000000000000000000000000000000000000f");

    fn meta() -> PoolMeta {
        PoolMeta {
            id: PoolId::new(CHAIN, P),
            protocol: ProtocolId::new("test"),
            token0: TokenId::new(CHAIN, A),
            token1: TokenId::new(CHAIN, B),
            fee: None,
            pool_type: PoolType::ConstantProduct,
        }
    }

    fn state(reserve0: u64, reserve1: u64) -> PoolState {
        PoolState {
            pool: PoolId::new(CHAIN, P),
            reserve0: U256::from(reserve0),
            reserve1: U256::from(reserve1),
            block_number: BlockNumber(11),
            log_index: LogIndex(4),
        }
    }

    #[test]
    fn the_two_directions_share_one_pool_and_never_cross_reserves() {
        let [a_to_b, b_to_a] = GraphEdge::pair(&meta(), &state(100, 200)).expect("priced pair");
        assert_eq!(a_to_b.token_in(), TokenId::new(CHAIN, A));
        assert_eq!(a_to_b.token_out(), TokenId::new(CHAIN, B));
        assert_eq!(
            (a_to_b.reserve_in, a_to_b.reserve_out),
            (U256::from(100u64), U256::from(200u64))
        );
        assert_eq!(
            (b_to_a.reserve_in, b_to_a.reserve_out),
            (U256::from(200u64), U256::from(100u64))
        );
        assert_eq!(a_to_b.pool(), b_to_a.pool());
        assert_eq!(a_to_b.state_position, b_to_a.state_position);
        assert_ne!(a_to_b.id, b_to_a.id);
        assert_eq!(a_to_b.flip(), b_to_a);
    }

    #[test]
    fn an_empty_side_is_refused_as_no_market() {
        assert_eq!(
            GraphEdge::pair(&meta(), &state(0, 200)),
            Err(EdgeRejection::EmptySide)
        );
        assert_eq!(
            GraphEdge::pair(&meta(), &state(100, 0)),
            Err(EdgeRejection::EmptySide)
        );
    }

    #[test]
    fn a_tiny_but_nonzero_reserve_is_still_a_market() {
        let edges = GraphEdge::pair(&meta(), &state(1, 1_000_000_000_000)).expect("priced");
        assert_eq!(edges[0].reserve_in, U256::from(1u64));
    }

    #[test]
    fn a_self_pair_is_refused() {
        let mut meta = meta();
        meta.token1 = meta.token0;
        assert_eq!(
            GraphEdge::pair(&meta, &state(100, 200)),
            Err(EdgeRejection::SameToken)
        );
    }
}
