use std::collections::BTreeMap;

use evm_core::{BlockNumber, ChainId, PoolId, PoolMeta, PoolState};

use crate::update::UpdatePosition;

/// One pool as the store knows it: who it is, and what its last authoritative
/// statement said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolRecord {
    pub meta: PoolMeta,
    pub state: Option<PoolState>,
}

/// An immutable view of the store at one point in the applied sequence.
///
/// Later stages read this instead of borrowing the live store, so a replay run
/// and a live run can be compared value to value, and so two runs of the same
/// input can be checked for equality.
///
/// Pools live in a `BTreeMap`, which makes iteration order — and anything
/// derived from it — deterministic rather than a `HashMap` iteration artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    pub chain_id: ChainId,
    /// The furthest applied position when this snapshot was taken.
    pub position: Option<UpdatePosition>,
    pools: BTreeMap<PoolId, PoolRecord>,
}

impl StateSnapshot {
    pub(crate) fn from_parts(
        chain_id: ChainId,
        position: Option<UpdatePosition>,
        metas: &std::collections::HashMap<PoolId, PoolMeta>,
        states: &std::collections::HashMap<PoolId, PoolState>,
    ) -> Self {
        let pools = metas
            .iter()
            .map(|(id, meta)| {
                (
                    *id,
                    PoolRecord {
                        meta: meta.clone(),
                        state: states.get(id).copied(),
                    },
                )
            })
            .collect();
        Self {
            chain_id,
            position,
            pools,
        }
    }

    /// The block the snapshot's state comes from, if anything has been applied.
    pub fn block_number(&self) -> Option<BlockNumber> {
        self.position.map(|p| p.block_number)
    }

    pub fn get(&self, pool: PoolId) -> Option<&PoolRecord> {
        self.pools.get(&pool)
    }

    pub fn pool_meta(&self, pool: PoolId) -> Option<&PoolMeta> {
        self.pools.get(&pool).map(|r| &r.meta)
    }

    pub fn pool_state(&self, pool: PoolId) -> Option<PoolState> {
        self.pools.get(&pool).and_then(|r| r.state)
    }

    pub fn pools(&self) -> impl Iterator<Item = (&PoolId, &PoolRecord)> {
        self.pools.iter()
    }

    /// Pools that have an authoritative reserve statement — the only ones a
    /// market graph may use.
    pub fn synced_pools(&self) -> impl Iterator<Item = (&PoolId, &PoolState)> {
        self.pools
            .iter()
            .filter_map(|(id, record)| record.state.as_ref().map(|state| (id, state)))
    }

    pub fn len(&self) -> usize {
        self.pools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }
}
