use std::collections::HashMap;

use alloy_primitives::U256;
use evm_core::{ChainId, PoolId, PoolMeta, PoolState};

use crate::error::{Result, StateError};
use crate::snapshot::StateSnapshot;
use crate::update::{StateUpdate, UpdatePosition};

/// The state engine's contract: take ordered updates, give ordered truth.
pub trait StateStore {
    fn chain_id(&self) -> ChainId;

    fn pool_meta(&self, pool: PoolId) -> Option<&PoolMeta>;

    fn pool_state(&self, pool: PoolId) -> Option<&PoolState>;

    /// Apply one update, or explain why it cannot be applied. A rejected update
    /// never changes the store.
    fn apply(&mut self, update: StateUpdate) -> Result<()>;

    fn snapshot(&self) -> StateSnapshot;
}

/// Process-local state. M1 needs no database: the whole point is that state can
/// be rebuilt from logs, so a map plus the rules below is the honest size.
#[derive(Clone, Debug)]
pub struct InMemoryStateStore {
    chain_id: ChainId,
    metas: HashMap<PoolId, PoolMeta>,
    states: HashMap<PoolId, PoolState>,
    /// Furthest position applied overall, used to catch a pipeline that feeds
    /// events out of execution order.
    position: Option<UpdatePosition>,
}

fn pool_label(pool: PoolId) -> String {
    format!("chain {} pool {}", pool.chain_id.0, pool.address)
}

impl InMemoryStateStore {
    pub fn new(chain_id: ChainId) -> Self {
        Self {
            chain_id,
            metas: HashMap::new(),
            states: HashMap::new(),
            position: None,
        }
    }

    /// The furthest applied position.
    pub fn position(&self) -> Option<UpdatePosition> {
        self.position
    }

    pub fn pool_count(&self) -> usize {
        self.metas.len()
    }

    pub fn synced_pool_count(&self) -> usize {
        self.states.len()
    }

    fn check_chain(&self, chain_id: ChainId) -> Result<()> {
        if chain_id == self.chain_id {
            Ok(())
        } else {
            Err(StateError::ChainMismatch {
                store: self.chain_id.0,
                update: chain_id.0,
            })
        }
    }

    fn register(&mut self, meta: PoolMeta) -> Result<()> {
        self.check_chain(meta.id.chain_id)?;
        if let Some(existing) = self.metas.get(&meta.id) {
            if existing != &meta {
                return Err(StateError::ConflictingRegistration(pool_label(meta.id)));
            }
        }
        self.metas.insert(meta.id, meta);
        Ok(())
    }

    fn sync(
        &mut self,
        pool: PoolId,
        reserve0: U256,
        reserve1: U256,
        position: UpdatePosition,
    ) -> Result<()> {
        self.check_chain(pool.chain_id)?;

        // An unknown pool stays unknown: no log, however well formed, is allowed
        // to create one here. Registration comes from attested metadata.
        if !self.metas.contains_key(&pool) {
            return Err(StateError::UnregisteredPool(pool_label(pool)));
        }

        let candidate = PoolState {
            pool,
            reserve0,
            reserve1,
            block_number: position.block_number,
            log_index: position.log_index,
        };
        if !candidate.has_valid_reserves() {
            return Err(StateError::InvalidReserves {
                pool: pool_label(pool),
                reserve0: reserve0.to_string(),
                reserve1: reserve1.to_string(),
                position,
            });
        }

        if let Some(previous) = self.position {
            if position <= previous {
                return Err(StateError::Regression {
                    previous,
                    attempted: position,
                });
            }
        }

        if let Some(stored) = self.states.get(&pool) {
            let stored_position = UpdatePosition::new(stored.block_number, stored.log_index);
            if position <= stored_position {
                return Err(StateError::OutOfOrder {
                    pool: pool_label(pool),
                    previous: stored_position,
                    attempted: position,
                });
            }
        }

        self.states.insert(pool, candidate);
        self.position = Some(position);
        Ok(())
    }
}

impl StateStore for InMemoryStateStore {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    fn pool_meta(&self, pool: PoolId) -> Option<&PoolMeta> {
        self.metas.get(&pool)
    }

    fn pool_state(&self, pool: PoolId) -> Option<&PoolState> {
        self.states.get(&pool)
    }

    fn apply(&mut self, update: StateUpdate) -> Result<()> {
        match update {
            StateUpdate::PoolRegistered(meta) => self.register(meta),
            StateUpdate::PoolSynced {
                pool,
                reserve0,
                reserve1,
                position,
            } => self.sync(pool, reserve0, reserve1, position),
        }
    }

    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot::from_parts(self.chain_id(), self.position, &self.metas, &self.states)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;
    use evm_core::{BlockNumber, LogIndex, PoolType, ProtocolId, TokenId};

    const CHAIN: ChainId = ChainId(91342);
    const POOL: PoolId = PoolId::new(
        CHAIN,
        address!("0x3978e57bbceb7666d54a03551c03691f897f6092"),
    );
    const OTHER: PoolId = PoolId::new(
        CHAIN,
        address!("0xcaafb95fc292c10a526f03fa480407bb438dac67"),
    );

    fn meta(pool: PoolId) -> PoolMeta {
        PoolMeta {
            id: pool,
            protocol: ProtocolId::new("v2-compatible"),
            token0: TokenId::new(
                CHAIN,
                address!("0x304912af0ce0dd6479735634d567715107bdc0c6"),
            ),
            token1: TokenId::new(
                CHAIN,
                address!("0x4200000000000000000000000000000000000006"),
            ),
            fee: None,
            pool_type: PoolType::ConstantProduct,
        }
    }

    fn position(block: u64, log: u64) -> UpdatePosition {
        UpdatePosition::new(BlockNumber(block), LogIndex(log))
    }

    fn sync(pool: PoolId, reserve0: u64, reserve1: u64, at: UpdatePosition) -> StateUpdate {
        StateUpdate::PoolSynced {
            pool,
            reserve0: U256::from(reserve0),
            reserve1: U256::from(reserve1),
            position: at,
        }
    }

    fn store_with_pool_registered() -> InMemoryStateStore {
        let mut store = InMemoryStateStore::new(CHAIN);
        store
            .apply(StateUpdate::PoolRegistered(meta(POOL)))
            .expect("registration");
        store
    }

    #[test]
    fn reserves_need_metadata_first() {
        let mut store = InMemoryStateStore::new(CHAIN);
        let rejected = store.apply(sync(POOL, 100, 200, position(100, 1)));
        assert_eq!(
            rejected,
            Err(StateError::UnregisteredPool(pool_label(POOL)))
        );
        assert!(store.pool_state(POOL).is_none());

        store
            .apply(StateUpdate::PoolRegistered(meta(POOL)))
            .expect("registration");
        store
            .apply(sync(POOL, 100, 200, position(100, 1)))
            .expect("sync");
        let state = store.pool_state(POOL).expect("state");
        assert_eq!(state.reserve0, U256::from(100u64));
        assert_eq!(state.reserve1, U256::from(200u64));
        assert_eq!(state.block_number, BlockNumber(100));
        assert_eq!(state.log_index, LogIndex(1));
    }

    #[test]
    fn state_follows_syncs_across_blocks() {
        let mut store = store_with_pool_registered();
        store
            .apply(sync(POOL, 100, 200, position(100, 3)))
            .expect("first sync");
        store
            .apply(sync(POOL, 120, 180, position(101, 7)))
            .expect("second sync");
        let state = store.pool_state(POOL).expect("state");
        assert_eq!(state.reserve0, U256::from(120u64));
        assert_eq!(state.reserve1, U256::from(180u64));
        assert_eq!(state.block_number, BlockNumber(101));
    }

    #[test]
    fn the_last_sync_of_a_block_wins() {
        // What the chain really did for this pool in block 37257255: three
        // syncs, at global log indexes 150, 174 and 181.
        let mut store = store_with_pool_registered();
        store
            .apply(sync(POOL, 11, 21, position(37257255, 150)))
            .expect("sync 1");
        store
            .apply(sync(POOL, 12, 22, position(37257255, 174)))
            .expect("sync 2");
        store
            .apply(sync(POOL, 13, 23, position(37257255, 181)))
            .expect("sync 3");
        let state = store.pool_state(POOL).expect("state");
        assert_eq!(state.reserve0, U256::from(13u64));
        assert_eq!(state.log_index, LogIndex(181));
        assert_eq!(store.position(), Some(position(37257255, 181)));
    }

    #[test]
    fn out_of_order_is_rejected_and_changes_nothing() {
        let mut store = store_with_pool_registered();
        store
            .apply(sync(POOL, 120, 180, position(101, 7)))
            .expect("sync");
        let rejected = store.apply(sync(POOL, 100, 200, position(100, 3)));
        assert_eq!(
            rejected,
            Err(StateError::Regression {
                previous: position(101, 7),
                attempted: position(100, 3),
            })
        );
        let state = store.pool_state(POOL).expect("state unchanged");
        assert_eq!(state.reserve0, U256::from(120u64));
        assert_eq!(state.block_number, BlockNumber(101));
    }

    #[test]
    fn a_repeated_position_is_rejected() {
        let mut store = store_with_pool_registered();
        store
            .apply(sync(POOL, 100, 200, position(100, 3)))
            .expect("sync");
        assert_eq!(
            store.apply(sync(POOL, 999, 999, position(100, 3))),
            Err(StateError::Regression {
                previous: position(100, 3),
                attempted: position(100, 3),
            })
        );
        assert_eq!(
            store.pool_state(POOL).expect("state").reserve0,
            U256::from(100u64)
        );
    }

    #[test]
    fn empty_reserves_are_rejected_without_panicking() {
        let mut store = store_with_pool_registered();
        store
            .apply(sync(POOL, 100, 200, position(100, 3)))
            .expect("sync");
        for (a, b) in [(0u64, 0u64), (0, 200), (100, 0)] {
            let rejected = store.apply(sync(POOL, a, b, position(101, 4)));
            assert!(
                matches!(rejected, Err(StateError::InvalidReserves { .. })),
                "{a}/{b} must be rejected, got {rejected:?}"
            );
        }
        // Every rejection left the last good state intact.
        assert_eq!(
            store.pool_state(POOL).expect("state").reserve1,
            U256::from(200u64)
        );
    }

    #[test]
    fn another_chain_is_not_this_store() {
        let foreign = PoolId::new(
            ChainId(1),
            address!("0x3978e57bbceb7666d54a03551c03691f897f6092"),
        );
        let mut store = store_with_pool_registered();
        assert_eq!(
            store.apply(StateUpdate::PoolRegistered(meta(foreign))),
            Err(StateError::ChainMismatch {
                store: 91342,
                update: 1,
            })
        );
        assert_eq!(
            store.apply(sync(foreign, 1, 1, position(100, 1))),
            Err(StateError::ChainMismatch {
                store: 91342,
                update: 1,
            })
        );
    }

    #[test]
    fn metadata_conflicts_are_rejected() {
        let mut store = store_with_pool_registered();
        let mut different = meta(POOL);
        different.token1 = TokenId::new(
            CHAIN,
            address!("0x0000000000000000000000000000000000000001"),
        );
        assert_eq!(
            store.apply(StateUpdate::PoolRegistered(different)),
            Err(StateError::ConflictingRegistration(pool_label(POOL)))
        );
        // Re-registering identical metadata is idempotent.
        store
            .apply(StateUpdate::PoolRegistered(meta(POOL)))
            .expect("identical metadata is not a conflict");
    }

    #[test]
    fn snapshots_are_deterministic_and_detached() {
        let mut a = InMemoryStateStore::new(CHAIN);
        let mut b = InMemoryStateStore::new(CHAIN);
        for store in [&mut a, &mut b] {
            // Registered in the opposite order on purpose: a snapshot must not
            // depend on insertion order.
            store
                .apply(StateUpdate::PoolRegistered(meta(OTHER)))
                .expect("meta other");
            store
                .apply(StateUpdate::PoolRegistered(meta(POOL)))
                .expect("meta pool");
            store
                .apply(sync(OTHER, 5, 6, position(100, 1)))
                .expect("sync other");
            store
                .apply(sync(POOL, 100, 200, position(100, 2)))
                .expect("sync pool");
        }
        let (sa, sb) = (a.snapshot(), b.snapshot());
        assert_eq!(sa, sb);
        assert_eq!(
            sa.pools().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![
                // BTreeMap order: pool 0x3978.. sorts before 0xcaaf..
                POOL, OTHER
            ]
        );
        assert_eq!(sa.block_number(), Some(BlockNumber(100)));
        assert_eq!(sa.synced_pools().count(), 2);

        // The snapshot does not move when the store does.
        let before = a.snapshot();
        a.apply(sync(POOL, 300, 400, position(101, 0)))
            .expect("later sync");
        assert_eq!(
            before.pool_state(POOL).expect("detached").reserve0,
            U256::from(100u64)
        );
        assert_eq!(
            a.snapshot().pool_state(POOL).expect("fresh").reserve0,
            U256::from(300u64)
        );
    }
}
