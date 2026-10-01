//! §16, §17, §24: an opportunity is a claim about *one* state version, and it
//! stops being a claim the moment that version moves.
//!
//! M3 priced a graph and M4 simulated a finding; neither had to answer how long
//! either was true. A live loop does: blocks arrive once a second, a pool that was
//! the cheapest way round at log 40 is not the cheapest way round at log 61, and a
//! pipeline that keeps quoting the old number is not fast — it is wrong.
//!
//! So a finding here carries the exact chain position it was priced against
//! ([`Opportunity::block_number`] plus the store's [`evm_state::UpdatePosition`],
//! and the hash of the block that position fell in), and the ledger invalidates it
//! as soon as a pool *on that route* is restated. Invalidation is not a soft
//! warning: a stale entry leaves the active set, and the only accessor the
//! simulation stage is given ([`OpportunityLedger::simmable`]) cannot yield it.
//! That is §65's "STALE must not reach Simulation / Risk" expressed as a missing
//! code path rather than as a check every caller has to remember.
//!
//! What deliberately is **not** a staleness rule here is the store's global
//! position. §16 makes the test the *relevant* state — "发生了相关 pool reserve
//! 更新" — and a chain that writes a log every second would otherwise turn every
//! simulation that took longer than one block into a stale answer about a market
//! that had not moved. Whether a finding is still true would then depend on how
//! fast this process reads blocks, which is the opposite of §67's parity claim.

use std::collections::{BTreeSet, VecDeque};

use alloy_primitives::B256;
use evm_core::{BlockNumber, ChainId, PoolId};
use evm_state::UpdatePosition;
use serde::{Deserialize, Serialize};

use crate::detector::Opportunity;

/// Which of the two pools a cycle enters first.
///
/// §17's fifth component. A route `A -> pool_x -> B -> pool_y -> A` and its mirror
/// share a pool *set*, so without this field the two directions of one pair would
/// collide — and they are not the same trade: they buy and sell opposite sides at
/// different prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Direction {
    /// The first hop's pool, which fixes the order the two tokens are traded in.
    pub entered_first: PoolId,
}

/// The stable identity of a finding (§17).
///
/// The two pools are stored ordered by address so that the identity does not
/// depend on which order the graph happened to enumerate them in; the direction
/// carries the order the *trade* used. `observed_block` is part of the identity
/// because a claim about block N and a claim about block N+1 are different claims,
/// even over identical pools — that is what makes the ledger's dedup a statement
/// about findings rather than about routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OpportunityId {
    pub chain_id: ChainId,
    pub observed_block: BlockNumber,
    /// The lower of the two pool addresses.
    pub pool_a: PoolId,
    /// The higher of the two pool addresses.
    pub pool_b: PoolId,
    pub direction: Direction,
}

impl OpportunityId {
    /// From the finding itself: the route already names its two pools in trade
    /// order, so no external ordering is consulted.
    pub fn new(opportunity: &Opportunity) -> Self {
        let [first, second] = opportunity.path.pools();
        let (pool_a, pool_b) = if first <= second {
            (first, second)
        } else {
            (second, first)
        };
        Self {
            chain_id: opportunity.chain_id,
            observed_block: opportunity.block_number,
            pool_a,
            pool_b,
            direction: Direction {
                entered_first: first,
            },
        }
    }

    /// Whether a pool restatement touches this finding at all. Only the two pools
    /// the route was priced against can invalidate it — a third pool moving does
    /// not change what these two hold.
    pub fn involves(&self, pool: PoolId) -> bool {
        self.pool_a == pool || self.pool_b == pool
    }
}

impl std::fmt::Display for OpportunityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "chain {} block {} pools {}|{} entering {}",
            self.chain_id.0,
            self.observed_block.0,
            self.pool_a.address,
            self.pool_b.address,
            self.direction.entered_first.address,
        )
    }
}

/// Why a finding stopped being true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Staleness {
    /// One of the route's own pools was restated at `position`. This is §16's
    /// rule, and the only one: the pools a cycle is priced across are the whole
    /// of what it claims about the market.
    PoolChanged {
        pool: PoolId,
        position: UpdatePosition,
    },
}

/// Where a finding is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Lifecycle {
    /// Priced against the state the store currently holds.
    Active,
    /// No longer true. Terminal: nothing in this crate moves an entry back to
    /// `Active`, and a re-registration of the same identity is a duplicate, not a
    /// resurrection (§24).
    Stale { reason: Staleness },
}

/// One finding, as the ledger holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackedOpportunity {
    pub id: OpportunityId,
    pub opportunity: Opportunity,
    /// The store position the graph this was priced from was taken at (§16).
    pub state_version: UpdatePosition,
    /// The hash of the block `state_version` falls in, as the pipeline sealed it.
    /// A simulation is asked to run at a *number*; this is what lets it check the
    /// number still means the same block (§19/§20 — a reorg is caught here, not
    /// assumed away).
    pub pinned_block_hash: B256,
    /// When this process saw it, for the staleness latency §70 asks for.
    pub observed_at_unix_ms: u64,
    pub lifecycle: Lifecycle,
}

impl TrackedOpportunity {
    pub const fn is_active(&self) -> bool {
        matches!(self.lifecycle, Lifecycle::Active)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerPolicy {
    /// How many active findings the ledger will hold. A live loop produces one
    /// per block per route; this is the ordering half of §50's bound on work the
    /// simulation stage is being asked to do.
    pub max_active: usize,
    /// How many stale findings are kept for the record. Bounded, because §48's
    /// session file must not grow without limit on a long run.
    pub stale_history: usize,
}

impl Default for LedgerPolicy {
    fn default() -> Self {
        Self {
            max_active: 64,
            stale_history: 256,
        }
    }
}

/// What one ledger operation did, as a countable fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LedgerStats {
    pub registered: u64,
    pub duplicates: u64,
    pub invalidated_by_pool: u64,
    /// Findings that were invalidated before any simulation was asked for them:
    /// the number §65's rule saves, counted by the caller that knows whether a
    /// finding had been handed on.
    pub evicted_for_space: u64,
    pub highest_active: u64,
}

/// The outcome of registering a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    /// New, and active.
    Accepted,
    /// This identity is already known. `first` is the version it was first
    /// registered at, so a duplicate that arrives with a *different* state version
    /// is visible as one rather than being silently merged.
    Duplicate { first_version: UpdatePosition },
    /// The ledger is full: the finding was not held. Reported, never dropped
    /// silently (§51).
    RejectedForCapacity,
}

/// Active findings plus the audit trail of the ones that stopped being true.
pub struct OpportunityLedger {
    policy: LedgerPolicy,
    seen: BTreeSet<OpportunityId>,
    active: Vec<TrackedOpportunity>,
    stale: VecDeque<TrackedOpportunity>,
    stats: LedgerStats,
}

impl OpportunityLedger {
    pub fn new(policy: LedgerPolicy) -> Self {
        Self {
            policy,
            seen: BTreeSet::new(),
            active: Vec::new(),
            stale: VecDeque::new(),
            stats: LedgerStats::default(),
        }
    }

    pub const fn policy(&self) -> LedgerPolicy {
        self.policy
    }

    pub const fn stats(&self) -> LedgerStats {
        self.stats
    }

    pub fn active(&self) -> &[TrackedOpportunity] {
        &self.active
    }

    pub fn stale_history(&self) -> impl Iterator<Item = &TrackedOpportunity> {
        self.stale.iter()
    }

    /// Register one finding.
    ///
    /// The caller hands over the state version the graph was read at and the hash
    /// of the block that version fell in — the detector knows neither, and
    /// guessing them from a block number would be the exact sloppiness §18
    /// forbids.
    pub fn register(
        &mut self,
        opportunity: &Opportunity,
        state_version: UpdatePosition,
        pinned_block_hash: B256,
        observed_at_unix_ms: u64,
    ) -> Registered {
        let id = OpportunityId::new(opportunity);
        if let Some(previous) = self.seen_version(&id) {
            self.stats.duplicates += 1;
            return Registered::Duplicate {
                first_version: previous,
            };
        }
        if self.active.len() >= self.policy.max_active {
            self.stats.evicted_for_space += 1;
            return Registered::RejectedForCapacity;
        }
        self.stats.registered += 1;
        self.seen.insert(id);
        self.active.push(TrackedOpportunity {
            id,
            opportunity: *opportunity,
            state_version,
            pinned_block_hash,
            observed_at_unix_ms,
            lifecycle: Lifecycle::Active,
        });
        // Held in identity order, so two runs over the same findings present them
        // the same way (§60's determinism, at this layer).
        self.active.sort_by_key(|entry| entry.id);
        self.stats.highest_active = self.stats.highest_active.max(self.active.len() as u64);
        Registered::Accepted
    }

    /// A pool was restated: everything priced against it is stale.
    ///
    /// Returns the identities that left the active set, so the caller can log the
    /// invalidation next to the state change that caused it (§11).
    pub fn invalidate_pool(
        &mut self,
        pool: PoolId,
        position: UpdatePosition,
    ) -> Vec<OpportunityId> {
        let mut moved = Vec::new();
        let mut still = Vec::with_capacity(self.active.len());
        while let Some(entry) = self.active.pop() {
            if entry.id.involves(pool) {
                self.stats.invalidated_by_pool += 1;
                self.push_stale(TrackedOpportunity {
                    lifecycle: Lifecycle::Stale {
                        reason: Staleness::PoolChanged { pool, position },
                    },
                    ..entry
                });
                moved.push(entry.id);
            } else {
                still.push(entry);
            }
        }
        self.active = still;
        moved
    }

    /// The findings the next stage may be given: everything still active, in
    /// identity order.
    ///
    /// There is no second filter here on purpose. A finding stops being true when
    /// one of *its* pools is restated, and [`Self::invalidate_pool`] applies that
    /// the moment the pipeline is told about the restatement — so by the time this
    /// is called, the active set is exactly the set §24's
    /// "opportunity state version == current relevant state version" would keep.
    /// Filtering on the store's global position instead would make an answer that
    /// arrived a block late into a stale finding about a market that had not
    /// moved, and would tie that to this process's reading speed rather than to
    /// the chain.
    pub fn simmable(&mut self) -> Vec<TrackedOpportunity> {
        self.active.clone()
    }

    fn seen_version(&self, id: &OpportunityId) -> Option<UpdatePosition> {
        self.active
            .iter()
            .find(|entry| entry.id == *id)
            .map(|entry| entry.state_version)
            .or_else(|| {
                self.stale
                    .iter()
                    .find(|entry| entry.id == *id)
                    .map(|entry| entry.state_version)
            })
    }

    fn push_stale(&mut self, entry: TrackedOpportunity) {
        if self.stale.len() >= self.policy.stale_history {
            self.stale.pop_front();
        }
        self.stale.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, U256};

    use super::*;
    use crate::support::test_opportunity;
    use evm_core::LogIndex;

    const CHAIN: ChainId = ChainId(1);

    const POOL_X: PoolId = PoolId::new(
        CHAIN,
        address!("0x1111111111111111111111111111111111111111"),
    );
    const POOL_Y: PoolId = PoolId::new(
        CHAIN,
        address!("0x2222222222222222222222222222222222222222"),
    );
    const POOL_Z: PoolId = PoolId::new(
        CHAIN,
        address!("0x3333333333333333333333333333333333333333"),
    );

    fn position(block: u64, log: u64) -> UpdatePosition {
        UpdatePosition::new(BlockNumber(block), LogIndex(log))
    }

    /// The block hash a test pretends the chain sealed at `block`.
    fn pinned(block: u64) -> B256 {
        B256::left_padding_from(&block.to_be_bytes())
    }

    #[test]
    fn identity_orders_pools_by_address_and_keeps_the_trade_direction() {
        let forward = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let reverse = test_opportunity(CHAIN, BlockNumber(100), POOL_Y, POOL_X, U256::from(1u64));
        let a = OpportunityId::new(&forward);
        let b = OpportunityId::new(&reverse);
        assert_eq!(a.pool_a, POOL_X);
        assert_eq!(a.pool_b, POOL_Y);
        assert_eq!(b.pool_a, POOL_X, "the pool pair is the same set");
        assert_eq!(b.pool_b, POOL_Y);
        assert_ne!(a, b, "but the two directions of it are two findings (§17)");
        assert_eq!(a.direction.entered_first, POOL_X);
        assert_eq!(b.direction.entered_first, POOL_Y);
    }

    #[test]
    fn a_block_difference_makes_a_new_identity_over_the_same_route() {
        let early = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let later = test_opportunity(CHAIN, BlockNumber(101), POOL_X, POOL_Y, U256::from(1u64));
        assert_ne!(OpportunityId::new(&early), OpportunityId::new(&later));
    }

    #[test]
    fn registering_the_same_finding_twice_is_a_duplicate_not_a_second_copy() {
        let opportunity =
            test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        assert_eq!(
            ledger.register(&opportunity, position(100, 10), pinned(100), 1_000),
            Registered::Accepted
        );
        assert_eq!(
            ledger.register(&opportunity, position(100, 10), pinned(100), 1_001),
            Registered::Duplicate {
                first_version: position(100, 10)
            }
        );
        assert_eq!(ledger.active().len(), 1);
        assert_eq!(ledger.stats().duplicates, 1);
    }

    #[test]
    fn an_invalidated_finding_cannot_be_resurrected_by_registering_it_again() {
        let opportunity =
            test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        ledger.register(&opportunity, position(100, 10), pinned(100), 1_000);
        ledger.invalidate_pool(POOL_X, position(100, 11));
        assert!(ledger.active().is_empty());
        // The same identity, re-priced later in the same block: still known, so
        // still one finding, and it does not come back as active.
        assert!(matches!(
            ledger.register(&opportunity, position(100, 12), pinned(100), 1_002),
            Registered::Duplicate { .. }
        ));
        assert!(ledger.active().is_empty());
    }

    #[test]
    fn invalidation_reaches_only_routes_that_use_the_changed_pool() {
        let on_x = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let elsewhere = test_opportunity(CHAIN, BlockNumber(100), POOL_Y, POOL_Z, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        ledger.register(&on_x, position(100, 10), pinned(100), 1_000);
        ledger.register(&elsewhere, position(100, 10), pinned(100), 1_001);
        let moved = ledger.invalidate_pool(POOL_X, position(100, 11));
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0], OpportunityId::new(&on_x));
        assert_eq!(ledger.active().len(), 1, "the untouched route stays live");
        assert_eq!(ledger.stats().invalidated_by_pool, 1);
        let stale = ledger.stale_history().next().expect("one stale entry");
        assert_eq!(
            stale.lifecycle,
            Lifecycle::Stale {
                reason: Staleness::PoolChanged {
                    pool: POOL_X,
                    position: position(100, 11)
                }
            }
        );
    }

    #[test]
    fn simmable_keeps_a_finding_whose_own_pools_did_not_move() {
        // §16's test is the *relevant* state, not the store's clock: block 101
        // arrived and wrote logs, but neither pool of this route restated anything,
        // so the finding is still a true claim and still has an answer owed to it.
        let on_x = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        ledger.register(&on_x, position(100, 10), pinned(100), 1_000);
        let given = ledger.simmable();
        assert_eq!(given.len(), 1);
        assert_eq!(given[0].id, OpportunityId::new(&on_x));
        assert_eq!(
            given[0].pinned_block_hash,
            pinned(100),
            "the finding carries the block hash it was priced at, so the simulation \
             stage can tell a reorg from a fresh read (§19)"
        );
        // Asking twice hands over the same finding: this is a question about the
        // market, not a counter that ticks up per poll.
        assert_eq!(ledger.simmable().len(), 1);
        assert_eq!(ledger.active().len(), 1);
    }

    #[test]
    fn a_stale_finding_cannot_be_handed_to_simulation() {
        // §65, expressed as a missing code path rather than as a check the caller
        // has to remember: once a route's pool is restated, the entry has left the
        // active set, so `simmable` has nothing stale to offer.
        let on_x = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        ledger.register(&on_x, position(100, 10), pinned(100), 1_000);
        ledger.invalidate_pool(POOL_Y, position(101, 3));
        assert!(ledger.simmable().is_empty());
        assert!(matches!(
            ledger.stale_history().next().expect("kept").lifecycle,
            Lifecycle::Stale {
                reason: Staleness::PoolChanged { .. }
            }
        ));
    }

    #[test]
    fn a_full_ledger_refuses_a_finding_loudly() {
        let policy = LedgerPolicy {
            max_active: 1,
            stale_history: 4,
        };
        let first = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let second = test_opportunity(CHAIN, BlockNumber(100), POOL_Z, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(policy);
        assert_eq!(
            ledger.register(&first, position(100, 10), pinned(100), 1_000),
            Registered::Accepted
        );
        assert_eq!(
            ledger.register(&second, position(100, 10), pinned(100), 1_001),
            Registered::RejectedForCapacity
        );
        assert_eq!(ledger.active().len(), 1);
        assert_eq!(ledger.stats().evicted_for_space, 1);
    }

    #[test]
    fn the_stale_trail_is_bounded_and_keeps_the_newest() {
        let policy = LedgerPolicy {
            max_active: 8,
            stale_history: 2,
        };
        let mut ledger = OpportunityLedger::new(policy);
        for block in 100..105u64 {
            let opportunity =
                test_opportunity(CHAIN, BlockNumber(block), POOL_X, POOL_Y, U256::from(1u64));
            ledger.register(
                &opportunity,
                position(block, 10),
                pinned(block),
                block * 1_000,
            );
            ledger.invalidate_pool(POOL_X, position(block, 11));
        }
        let kept: Vec<u64> = ledger
            .stale_history()
            .map(|entry| entry.opportunity.block_number.0)
            .collect();
        assert_eq!(kept, vec![103, 104], "the trail holds the most recent two");
    }

    #[test]
    fn active_findings_are_presented_in_identity_order() {
        let a = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let b = test_opportunity(CHAIN, BlockNumber(100), POOL_Z, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        // Registered newest-first: the presentation order must not follow that.
        ledger.register(&b, position(100, 10), pinned(100), 1_000);
        ledger.register(&a, position(100, 10), pinned(100), 1_001);
        let ids: Vec<OpportunityId> = ledger.active().iter().map(|e| e.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn an_amount_change_does_not_change_the_identity() {
        // §17: identity is about the route and the block, not the size of it. Two
        // input amounts for one route are two quotes of one finding.
        let small = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let large = test_opportunity(
            CHAIN,
            BlockNumber(100),
            POOL_X,
            POOL_Y,
            U256::from(1_000u64),
        );
        assert_eq!(OpportunityId::new(&small), OpportunityId::new(&large));
    }

    #[test]
    fn the_display_line_names_every_identity_component() {
        let opportunity =
            test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let id = OpportunityId::new(&opportunity);
        let text = id.to_string();
        for part in [
            "chain 1",
            "block 100",
            &POOL_X.address.to_string(),
            &POOL_Y.address.to_string(),
            &POOL_X.address.to_string(),
        ] {
            assert!(text.contains(part), "{text} is missing {part}");
        }
    }

    #[test]
    fn a_chain_mismatch_is_a_different_identity() {
        let here = test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut there = here;
        there.chain_id = ChainId(91_342);
        assert_ne!(OpportunityId::new(&here), OpportunityId::new(&there));
        assert_ne!(OpportunityId::new(&here).pool_a, POOL_Z);
    }

    /// The pool pair is what invalidation reads, so a `PoolId` from another chain
    /// must not match one on this chain even at the same address.
    #[test]
    fn a_pool_from_another_chain_does_not_invalidate_this_chain_finding() {
        let opportunity =
            test_opportunity(CHAIN, BlockNumber(100), POOL_X, POOL_Y, U256::from(1u64));
        let mut ledger = OpportunityLedger::new(LedgerPolicy::default());
        ledger.register(&opportunity, position(100, 10), pinned(100), 1_000);
        let foreign = PoolId::new(ChainId(2), POOL_X.address);
        assert!(ledger
            .invalidate_pool(foreign, position(100, 11))
            .is_empty());
        assert_eq!(ledger.active().len(), 1);
    }
}
