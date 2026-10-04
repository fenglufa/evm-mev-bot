//! A candidate: an address the chain says was created, plus the saying.
//!
//! This type is the reason M9.1 exists. Before it, the only way a pool could be
//! known was to be written into `data/protocols/*.json` by hand, so discovery
//! meant "someone told us". A `CandidatePool` is what the chain itself says, and
//! it says only one thing:
//!
//! > this address was discovered.
//!
//! It does not say the address is a V2 pool, holds reserves, or can be traded
//! against — those are conclusions `verify` reaches later, from reads of its own.
//! Keeping the two apart in the type system is what stops a creation log from
//! becoming a trust decision, which is the failure mode the whole milestone is
//! scoped to prevent (M9.1 §1: `Discovery != Trust`).

use std::fmt::{self, Display, Formatter};

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

use evm_core::{ChainId, PoolId, TokenId};
use evm_protocol::{LogPosition, PoolCreatedEvent};

/// How a candidate entered the system.
///
/// Deliberately an enum with one variant, not a trait object (M9.1 §5: "avoid
/// premature generic frameworks"). A future Flashblocks or receipt-log source
/// adds a variant and a scan function; it does not add a framework, and nothing
/// downstream of `CandidatePool` has to change to accept it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DiscoverySource {
    /// `eth_getLogs` over a historical block range, decoding `PairCreated`.
    HistoricalPairCreated,
}

impl DiscoverySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HistoricalPairCreated => "historical_pair_created",
        }
    }
}

impl Display for DiscoverySource {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `PairCreated` claim, kept exactly as claimed.
///
/// The field names say what each value is: `claimed_token0` is the token the
/// factory named, not a token this program has read off the contract. When
/// verification later agrees with the claim, the verified pool carries the
/// confirmed value in a different type; a candidate never does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidatePool {
    /// The pair address the factory named, chain-scoped like every other
    /// identity in this project.
    pub pool: PoolId,
    pub source: DiscoverySource,
    /// The emitter. Provenance only (M9.1 §15): this address is where the claim
    /// came from, and no check here treats it as a reason to trust the claim.
    pub factory: Address,
    pub claimed_token0: TokenId,
    pub claimed_token1: TokenId,
    /// The factory's own counter, kept because it is part of the event and
    /// because a re-created pair with the same address would differ in it.
    pub pair_index: U256,
    /// Full chain position of the log this claim was read from: block, tx hash,
    /// tx index, log index (M9.1 §7).
    pub discovered_at: LogPosition,
}

impl CandidatePool {
    /// A candidate from a decoded claim. Nothing is checked here beyond what the
    /// decoder already checked, and that is the point: the candidate is the
    /// output of decoding, not the output of verification.
    pub fn from_claim(claim: &PoolCreatedEvent, source: DiscoverySource) -> Self {
        Self {
            pool: claim.pool,
            source,
            factory: claim.factory,
            claimed_token0: claim.token0,
            claimed_token1: claim.token1,
            pair_index: claim.pair_index,
            discovered_at: claim.position,
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.pool.chain_id
    }

    /// The block this candidate's claim was emitted in — which M9.1 §22 makes the
    /// block every verification read of this candidate has to be pinned at.
    pub fn discovery_block(&self) -> evm_core::BlockNumber {
        self.discovered_at.block_number
    }

    /// Identity of the *claim*, not of the pool: chain + pool address + the exact
    /// log it came from (M9.1 §7). Two `PairCreated` logs naming one address are
    /// two claims, and collapsing them by pool address alone would throw away the
    /// fact that the chain said it twice.
    ///
    /// This is also the key dedup and evidence assembly use, so a row's identity
    /// never depends on the order rows came back in.
    pub fn identity(&self) -> (u64, Address, u64, u64, u64) {
        (
            self.pool.chain_id.0,
            self.pool.address,
            self.discovered_at.block_number.0,
            self.discovered_at.tx_index.0,
            self.discovered_at.log_index.0,
        )
    }

    /// Deterministic order for every list of candidates this crate emits: the
    /// same tuple [`Self::identity`] is built from, so sorted output can never
    /// disagree with row identity. `(block, tx, log)` is already unique on a
    /// chain, so no tie-breaker is needed.
    pub fn sort_key(&self) -> (u64, Address, u64, u64, u64) {
        self.identity()
    }
}

impl Ord for CandidatePool {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for CandidatePool {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
