use std::collections::BTreeMap;

use evm_core::{BlockNumber, ChainId, PoolId, PoolMeta, PoolState};

use crate::update::UpdatePosition;

/// One pool as the store knows it: who it is, and what its last authoritative
/// statement said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolRecord {
    pub meta: PoolMeta,
    pub state: Option<PoolState>,
    /// How far a complete `Sync` scan of this pool reached. `None` means nobody
    /// has looked past the state event itself, so the record proves the pool's
    /// price at its own block and nowhere else.
    pub coverage: Option<SyncScanCoverage>,
}

/// Proof that a pool's stored state is still its state at a later block.
///
/// A `Sync` log says what the reserves were at the block it lives in. It says
/// nothing about the blocks after — unless somebody scanned them and came back
/// empty. This type is that scan, stated as a range: it is the difference between
/// "the newest record I happen to have" and "the pool's price at block T".
///
/// It carries no verdict. Whether the range is wide enough to cover a given
/// target is asked of it by the consumer that needs to know
/// ([`SyncScanCoverage::proves_valid_at`]), so the same record can be audited
/// against any block without being re-fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncScanCoverage {
    /// Lowest block the scan covered, inclusive. Must be at or before the block
    /// of the state it is offered as proof for, or the scan leaves a gap.
    pub from: BlockNumber,
    /// Highest block the scan covered, inclusive.
    pub through: BlockNumber,
}

impl SyncScanCoverage {
    /// A scan of `from..=through`, normalised so an inverted range cannot be
    /// constructed by a caller that mixed its arguments up.
    pub fn new(from: BlockNumber, through: BlockNumber) -> Option<Self> {
        (from <= through).then_some(Self { from, through })
    }

    /// Does this scan cover the whole interval after `state` up to `target`?
    ///
    /// Three conditions, all needed: the scan starts no later than the state it
    /// vouches for (otherwise a `Sync` in the unscanned gap is missed), the state
    /// is not from after the target (a later observation cannot price an earlier
    /// block), and the scan reaches the target (otherwise validity past the last
    /// block looked at is assumed rather than shown).
    ///
    /// A scan that began inside the state's own block still counts: the chain
    /// orders a block's logs by a single global index, so covering the block covers
    /// every position after the state's within it.
    pub fn proves_valid_at(&self, state: UpdatePosition, target: BlockNumber) -> bool {
        self.from <= state.block_number && state.block_number <= target && self.through >= target
    }
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
    /// The block this snapshot is being read *at*, when a caller has named one.
    /// `None` means the snapshot is read at its own applied position, which is
    /// what every live and replay path does.
    target: Option<BlockNumber>,
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
                        coverage: None,
                    },
                )
            })
            .collect();
        Self {
            chain_id,
            position,
            target: None,
            pools,
        }
    }

    /// Re-read this snapshot at an explicit target block, attaching the scan
    /// coverage that can justify a pool's older state still pricing that block.
    ///
    /// Nothing here rewrites a `PoolState`: the reserves keep the position of the
    /// `Sync` that published them, and the coverage sits beside them as a separate
    /// claim about the blocks after it. That separation is the point — a caller
    /// that later questions the projection can see both the observation and the
    /// reason it was trusted, and a pool nobody scanned keeps being trusted only
    /// for the block it was observed in.
    ///
    /// Coverage for a pool this snapshot does not know is dropped rather than
    /// erroring: it describes a pool that is not in this market, and refusing to
    /// build a snapshot over it would turn an over-wide scan into a failure.
    #[must_use]
    pub fn at_target(
        mut self,
        target: BlockNumber,
        coverage: BTreeMap<PoolId, SyncScanCoverage>,
    ) -> Self {
        for (pool, record) in &mut self.pools {
            record.coverage = coverage.get(pool).copied();
        }
        self.target = Some(target);
        self
    }

    /// The block the snapshot is being read at, if a caller named one.
    pub fn target_block(&self) -> Option<BlockNumber> {
        self.target
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

    /// How far a `Sync` scan of this pool reached, if one was made.
    pub fn pool_coverage(&self, pool: PoolId) -> Option<SyncScanCoverage> {
        self.pools.get(&pool).and_then(|r| r.coverage)
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy_primitives::{address, U256};

    use evm_core::{LogIndex, PoolType, ProtocolId, TokenId};

    use super::*;

    const CHAIN: ChainId = ChainId(91342);
    const POOL: PoolId = PoolId::new(
        CHAIN,
        address!("0x3978e57bbceb7666d54a03551c03691f897f6092"),
    );

    fn meta() -> PoolMeta {
        PoolMeta {
            id: POOL,
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

    fn state_at(block: u64, log: u64) -> PoolState {
        PoolState {
            pool: POOL,
            reserve0: U256::from(100u64),
            reserve1: U256::from(200u64),
            block_number: BlockNumber(block),
            log_index: LogIndex(log),
        }
    }

    fn snapshot_with(state: PoolState) -> StateSnapshot {
        let mut metas = HashMap::new();
        metas.insert(POOL, meta());
        let mut states = HashMap::new();
        states.insert(POOL, state);
        StateSnapshot::from_parts(
            CHAIN,
            Some(UpdatePosition::new(state.block_number, state.log_index)),
            &metas,
            &states,
        )
    }

    fn coverage(from: u64, through: u64) -> SyncScanCoverage {
        SyncScanCoverage::new(BlockNumber(from), BlockNumber(through)).expect("ordered range")
    }

    fn position(block: u64) -> UpdatePosition {
        UpdatePosition::new(BlockNumber(block), LogIndex(3))
    }

    #[test]
    fn an_inverted_scan_range_is_not_a_scan() {
        assert!(SyncScanCoverage::new(BlockNumber(200), BlockNumber(100)).is_none());
        assert_eq!(
            SyncScanCoverage::new(BlockNumber(100), BlockNumber(100)),
            Some(coverage(100, 100))
        );
    }

    #[test]
    fn a_scan_proves_validity_only_across_the_interval_it_covered() {
        // Sync at 100, scan 100..=120, target 120: the canonical valid case.
        assert!(coverage(100, 120).proves_valid_at(position(100), BlockNumber(120)));
        // The scan stopped one block short, so 121 could hold a Sync nobody saw.
        assert!(!coverage(100, 119).proves_valid_at(position(100), BlockNumber(120)));
        // The scan started after the state it vouches for: (100, 110] is unlooked at.
        assert!(!coverage(110, 120).proves_valid_at(position(100), BlockNumber(120)));
        // A Sync after the target cannot price it, however wide the scan around it.
        assert!(!coverage(100, 200).proves_valid_at(position(121), BlockNumber(120)));
        // A scan covering exactly the state's own block proves that block and no other.
        assert!(coverage(100, 100).proves_valid_at(position(100), BlockNumber(100)));
        assert!(!coverage(100, 100).proves_valid_at(position(100), BlockNumber(101)));
    }

    #[test]
    fn projecting_onto_a_target_keeps_the_observation_untouched() {
        let state = state_at(100, 3);
        let projected =
            snapshot_with(state).at_target(BlockNumber(120), [(POOL, coverage(100, 120))].into());

        assert_eq!(projected.target_block(), Some(BlockNumber(120)));
        // §14: the target is a separate claim from the evidence. Rewriting either
        // field here would destroy the ability to audit one against the other.
        assert_eq!(projected.pool_state(POOL), Some(state));
        assert_eq!(projected.pool_coverage(POOL), Some(coverage(100, 120)));
        // `block_number()` still reports where the state came from, not the target.
        assert_eq!(projected.block_number(), Some(BlockNumber(100)));
    }

    #[test]
    fn a_snapshot_without_a_target_is_read_where_it_was_applied() {
        let snapshot = snapshot_with(state_at(100, 3));
        assert_eq!(snapshot.target_block(), None);
        assert_eq!(snapshot.pool_coverage(POOL), None);
    }

    #[test]
    fn coverage_for_an_unknown_pool_is_dropped_not_stored() {
        let other = PoolId::new(
            CHAIN,
            address!("0xcaafb95fc292c10a526f03fa480407bb438dac67"),
        );
        let snapshot = snapshot_with(state_at(100, 3))
            .at_target(BlockNumber(120), [(other, coverage(100, 120))].into());
        assert_eq!(snapshot.pool_coverage(POOL), None);
        assert_eq!(snapshot.pool_coverage(other), None);
        assert_eq!(snapshot.target_block(), Some(BlockNumber(120)));
    }

    #[test]
    fn projecting_twice_replaces_the_first_attempts_coverage() {
        // A re-run against a wider scan must not leave the earlier, narrower
        // coverage attached to a pool the new scan also names — and a pool the new
        // scan does not name must fall back to no evidence at all, not to stale
        // evidence from the previous projection.
        let once = snapshot_with(state_at(100, 3))
            .at_target(BlockNumber(120), [(POOL, coverage(100, 120))].into());
        let twice = once.at_target(BlockNumber(130), BTreeMap::new());
        assert_eq!(twice.pool_coverage(POOL), None);
        assert_eq!(twice.target_block(), Some(BlockNumber(130)));
    }
}
