//! Attestation → Registry → StateStore → StateSnapshot → Graph, on the paths that
//! already exist (M9.1 §17).
//!
//! Nothing in this module is a parallel implementation of anything. The registry is
//! `evm_protocol::Registry` behind its `validate()` guard, the store is
//! `evm_state::InMemoryStateStore` with its monotonic-position rule, and the graph
//! is `evm_graph::MarketGraphBuilder`, which reads the state layer and refuses to
//! price a pool it cannot place at the target block. Discovery's contribution ends
//! at the attestation; from there the data goes through the same doors as every
//! milestone since M2. That is what makes the §30 boundary `Verified Pool !=
//! Tradable Opportunity` enforceable rather than merely stated — the graph's own
//! gates do the not-trading, and a discovery run cannot reach an opportunity search
//! through a door that does not exist here.
//!
//! Three things this module has to get right that the existing types do not do by
//! themselves:
//!
//! - **Two claims, one address.** The registry keys on `PoolId`, so a pair the
//!   factory announced twice has one slot. The earliest claim wins and the later
//!   one is reported, never silently merged (§30: `Duplicate ≠ Reusable`).
//! - **One order into the store.** The store enforces a *global* increasing
//!   position, so syncs are fed sorted by `(block, log_index)`. Feeding them in
//!   candidate order would trip `Regression` on the second pool and lose both.
//! - **The store's refusals are data.** A pool whose `Sync` publishes an empty side
//!   is refused by `InvalidReserves`. That is the store being correct, so it is
//!   recorded and the run continues: a verified-but-unpriceable pool is a finding,
//!   not a crash.

use std::collections::BTreeMap;

use serde::Serialize;

use evm_core::{BlockNumber, ChainId, PoolId, PoolState};
use evm_graph::{GraphBuild, MarketGraphBuilder};
use evm_protocol::{PoolAttestation, Registry, RegistryError};
use evm_state::{
    InMemoryStateStore, StateError, StateSnapshot, StateStore, StateUpdate, SyncScanCoverage,
    UpdatePosition,
};

use crate::attest::attestation_of;
use crate::error::{DiscoveryError, Result};
use crate::reconstruct::Reconstruction;
use crate::verify::VerifiedPool;

/// Which claim a duplicated address kept, and which one it did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DuplicateClaim {
    pub pool: PoolId,
    /// The discovery block of the claim that produced the attestation.
    pub kept_block: BlockNumber,
    /// The one that was not used.
    pub dropped_block: BlockNumber,
}

/// A `Sync` the state layer refused, in the state layer's own words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoreRejection {
    pub pool: PoolId,
    pub position: UpdatePosition,
    /// The rule that fired, named: `empty_reserves` today.
    pub rule: &'static str,
    /// The store's formatted message, kept verbatim so no table paraphrases a
    /// correctness rule.
    pub message: String,
}

/// The graph, or the honest reason there is not one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphOutcome {
    Built(GraphBuild),
    /// The store applied no position, so the builder has no block to take a graph
    /// at. Zero pools reaching state is a legitimate result of a window full of
    /// dead candidates, and reporting it as an error would turn "nothing here is
    /// tradable" into "the run broke".
    NoStateApplied,
}

impl GraphOutcome {
    pub fn build(&self) -> Option<&GraphBuild> {
        match self {
            Self::Built(build) => Some(build),
            Self::NoStateApplied => None,
        }
    }
}

/// What one discovery run leaves behind, as values — so two runs of the same input
/// can be compared with `==` rather than with a report (§21).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredState {
    /// The registry this run produced: whatever it was seeded with, plus every
    /// discovered attestation that survived dedup and validation.
    pub registry: Registry,
    /// Discovery's own contributions, in `PoolId` order.
    pub attested: Vec<PoolAttestation>,
    pub duplicates: Vec<DuplicateClaim>,
    pub store_rejections: Vec<StoreRejection>,
    pub snapshot: StateSnapshot,
    pub graph: GraphOutcome,
}

fn pool_label(pool: PoolId) -> String {
    format!("chain {} pool {}", pool.chain_id.0, pool.address)
}

fn registry_error(layer: &'static str, err: &RegistryError) -> DiscoveryError {
    let pool = match err {
        RegistryError::Unevidenced(pool, _)
        | RegistryError::Conflict(pool, _)
        | RegistryError::DegeneratePair(pool, _) => pool.clone(),
        // Only `load` / `load_dir` produce these, and this module loads no file.
        RegistryError::Io(detail) | RegistryError::Format(detail) => detail.clone(),
    };
    DiscoveryError::Integration {
        layer,
        pool,
        message: err.to_string(),
    }
}

fn validate_error(err: RegistryError) -> DiscoveryError {
    registry_error("Registry::validate", &err)
}

fn store_error(pool: PoolId, detail: impl Into<String>) -> DiscoveryError {
    DiscoveryError::Integration {
        layer: "StateStore",
        pool: pool_label(pool),
        message: detail.into(),
    }
}

/// Run the verified pools into the existing pipeline.
///
/// `base` is the registry the rest of the project already trusts (loaded from
/// `data/protocols/*.json`). It is neither written to disk nor ignored: every
/// discovered attestation is merged into a clone of it, so an address the
/// hand-maintained registry already describes differently is refused by that
/// registry's own conflict rule rather than by a new one invented here (§16).
///
/// The state store is scoped to this run: it applies `PoolRegistered` for the pools
/// discovery attested, and nothing else. The committed registry is the counterparty
/// for conflicts, not a backlog to re-apply — the live pipeline's own
/// registry-to-store wiring is M5's and is not reimplemented here (§17 "do not create
/// a parallel implementation").
///
/// `verified` may arrive in any order. The claims are sorted by candidate identity
/// first, so "the earliest claim of an address wins" is a property of this function
/// rather than a promise about the caller (§21).
pub fn integrate(
    chain_id: ChainId,
    base: &Registry,
    verified: &[VerifiedPool],
) -> Result<DiscoveredState> {
    let (claims, duplicates) = unique_claims(verified);
    // Only the pools that got an attestation may be synced: a dropped duplicate's
    // `Sync` would otherwise move a pool the registry knows by a different claim.
    let syncs = syncs_of(&claims);
    integrate_with(chain_id, base, &claims, syncs, duplicates, None)
}

/// The same pipeline, priced at an explicitly named target block, with state that a
/// `Sync` scan reconstructed rather than the state verification happened to see.
///
/// This is M9.2's door from reconstruction to graph, and it is deliberately a sibling
/// of [`integrate`] rather than a change to it: M9.1's evidence tables are the output
/// of `integrate`, and its rule — a pool prices the graph iff its state is in the very
/// block the snapshot was applied at — stays exactly where it was. What differs here is
/// only *which* `Sync` is fed and the evidence attached to the projection:
///
/// - the state a pool is given is the one `reconstruction` selected for the target,
///   which may be from any block at or before it, and a pool whose scan found no `Sync`
///   is registered and unsynced, which the builder reports as `StateUnavailable`;
/// - the snapshot is projected with `StateSnapshot::at_target`, so the graph is built at
///   `reconstruction.target` and each pool's coverage range travels with it;
/// - the builder still decides admission, on the same predicate it has always used plus
///   the scan proof (§16: the rule is not weakened, the evidence is added).
///
/// The verified pools are still the source of identity: attestation, dedup, and the
/// registry's own validation are untouched, so a reconstructed state cannot put a pool
/// on the graph that discovery never attested.
pub fn integrate_at_target(
    chain_id: ChainId,
    base: &Registry,
    verified: &[VerifiedPool],
    reconstruction: &Reconstruction,
) -> Result<DiscoveredState> {
    let (claims, duplicates) = unique_claims(verified);
    let attested: BTreeMap<PoolId, ()> = claims
        .iter()
        .map(|pool| (pool.candidate.pool, ()))
        .collect();
    let syncs: Vec<PoolState> = reconstruction
        .rows
        .iter()
        .filter(|row| attested.contains_key(&row.pool))
        .filter_map(|row| row.state())
        .collect();
    integrate_with(
        chain_id,
        base,
        &claims,
        syncs,
        duplicates,
        Some(reconstruction),
    )
}

/// Registry → store → snapshot → graph, for both doors above.
///
/// `reconstruction` is the projection: `None` means the snapshot is handed to the
/// builder exactly as the store applied it, which is the behavior M2 through M9.1 have
/// always had, and `Some` names the target block and the per-pool scan coverage that
/// licenses a pool older than the target to price it. Everything else — one attestation
/// per address, syncs fed in ascending chain position, the store's refusals recorded as
/// data rather than raised as errors, the builder as the only admission gate — is
/// shared, because two copies of those rules is how two doors start to disagree.
fn integrate_with(
    chain_id: ChainId,
    base: &Registry,
    claims: &[&VerifiedPool],
    mut syncs: Vec<PoolState>,
    duplicates: Vec<DuplicateClaim>,
    reconstruction: Option<&Reconstruction>,
) -> Result<DiscoveredState> {
    // Registry first, and the project's own guard on this run's output before it
    // can reach anything downstream: complete evidence, one chain per
    // attestation, no same-token pair.
    let mut discovered = Registry::default();
    let mut attested = Vec::with_capacity(claims.len());
    for pool in claims {
        let attestation = attestation_of(pool);
        discovered
            .pools
            .insert(attestation.pool, attestation.clone());
        attested.push(attestation);
    }
    attested.sort_by_key(|attestation| attestation.pool);
    discovered.validate().map_err(validate_error)?;
    let mut registry = base.clone();
    registry
        .merge(discovered)
        .map_err(|err| registry_error("Registry::merge", &err))?;
    registry.validate().map_err(validate_error)?;

    let mut store = InMemoryStateStore::new(chain_id);
    for attestation in &attested {
        let pool = attestation.pool;
        store
            .apply(StateUpdate::PoolRegistered(attestation.to_meta()))
            .map_err(|err| store_error(pool, err.to_string()))?;
    }

    syncs.sort_by_key(|state| (state.block_number.0, state.log_index.0, state.pool));
    let mut rejections = Vec::new();
    for sync in syncs {
        let position = UpdatePosition::new(sync.block_number, sync.log_index);
        let pool = sync.pool;
        let update = StateUpdate::PoolSynced {
            pool,
            reserve0: sync.reserve0,
            reserve1: sync.reserve1,
            position,
        };
        match store.apply(update) {
            Ok(()) => {}
            Err(err @ StateError::InvalidReserves { .. }) => rejections.push(StoreRejection {
                pool,
                position,
                rule: "empty_reserves",
                message: err.to_string(),
            }),
            Err(err) => {
                // Ordering is a property of this function, so any other refusal is
                // a bug here rather than a finding about the chain.
                return Err(store_error(
                    pool,
                    format!(
                        "syncs were fed in ascending chain position, so the store should not \
                         have had to refuse this one: {err}"
                    ),
                ));
            }
        }
    }
    rejections.sort_by_key(StoreRejection::identity);

    let snapshot = match reconstruction {
        Some(reconstruction) => {
            let coverage: BTreeMap<PoolId, SyncScanCoverage> = reconstruction.coverage();
            store.snapshot().at_target(reconstruction.target, coverage)
        }
        None => store.snapshot(),
    };
    let graph = match snapshot.position {
        Some(_) => GraphOutcome::Built(MarketGraphBuilder::new().build_traced(&snapshot).map_err(
            |err| DiscoveryError::Integration {
                layer: "MarketGraphBuilder",
                pool: format!("the {} snapshot", snapshot.chain_id.0),
                message: err.to_string(),
            },
        )?),
        None => GraphOutcome::NoStateApplied,
    };

    Ok(DiscoveredState {
        registry,
        attested,
        duplicates,
        store_rejections: rejections,
        snapshot,
        graph,
    })
}

impl StoreRejection {
    /// Sort key and row identity: the position the store refused, which is unique
    /// per pool by construction of the scan.
    fn identity(&self) -> (u64, u64, PoolId) {
        (
            self.position.block_number.0,
            self.position.log_index.0,
            self.pool,
        )
    }
}

/// The verified pools with at most one entry per pool address, plus the claims that
/// were left out.
///
/// Two claims of one address do not necessarily produce identical attestations —
/// the pinned block, and therefore the evidence, differs — which is why this is a
/// dedup with a report rather than a conflict.
///
/// The walk is in candidate-identity order, which is ascending by chain position
/// within one pool address, so the claim that wins is the earliest one on the chain
/// regardless of the order the verified pools were handed over in.
fn unique_claims(verified: &[VerifiedPool]) -> (Vec<&VerifiedPool>, Vec<DuplicateClaim>) {
    let mut order: Vec<usize> = (0..verified.len()).collect();
    order.sort_by_key(|index| verified[*index].candidate.identity());
    let mut claims: Vec<&VerifiedPool> = Vec::with_capacity(verified.len());
    let mut first_block: BTreeMap<PoolId, BlockNumber> = BTreeMap::new();
    let mut duplicates = Vec::new();
    for index in order {
        let pool = &verified[index];
        let pool_id = pool.candidate.pool;
        let claimed_at = pool.candidate.discovery_block();
        match first_block.get(&pool_id) {
            Some(kept_block) => duplicates.push(DuplicateClaim {
                pool: pool_id,
                kept_block: *kept_block,
                dropped_block: claimed_at,
            }),
            None => {
                first_block.insert(pool_id, claimed_at);
                claims.push(pool);
            }
        }
    }
    duplicates.sort_by_key(DuplicateClaim::sort_key);
    (claims, duplicates)
}

impl DuplicateClaim {
    fn sort_key(&self) -> (PoolId, BlockNumber, BlockNumber) {
        (self.pool, self.kept_block, self.dropped_block)
    }
}

/// The pools' own `Sync` records, in the order the store will accept them.
fn syncs_of(claims: &[&VerifiedPool]) -> Vec<PoolState> {
    let mut syncs: Vec<PoolState> = claims
        .iter()
        .map(|pool| pool.market_state.to_pool_state())
        .collect();
    syncs.sort_by_key(|state| (state.block_number.0, state.log_index.0, state.pool));
    syncs
}
