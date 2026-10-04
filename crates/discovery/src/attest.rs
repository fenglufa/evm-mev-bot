//! The bridge from a verified candidate to the trust object every other stage
//! already consumes (M9.1 §13).
//!
//! Discovery does not invent a registry, an attestation shape, or a graph. It
//! produces `evm_protocol::PoolAttestation` and hands it to `evm_protocol::Registry`
//! — the same pair `data/protocols/*.json` loads into, so a pool the chain told us
//! about and a pool a human told us about are indistinguishable downstream and are
//! held to the same `Registry::validate()` (M9.1 §16).
//!
//! ## What the evidence fields are allowed to say
//!
//! `AttestationEvidence` has three buckets, and §9 makes them three different
//! questions. Filling all three from one convenient read is the easy way to make
//! the type check and the claim wrong, so each ref here names the specific request
//! that produced it and the block it was pinned at:
//!
//! - `identity` — `eth_getCode(address)` and `getReserves()` at the pinned block.
//!   Both say something about the *contract*, neither of which is a reserve anyone
//!   published.
//! - `tokens` — `token0()` and `token1()` at the pinned block.
//! - `state` — the pool's own `Sync(uint112,uint112)` log, which is the only ref
//!   here that carries a transaction hash and a log index, because it is the only
//!   one that is a published statement rather than a question asked of a node.
//!
//! ## The fee stays unknown
//!
//! §14 is a correctness requirement, not a TODO: discovery never fills in 997/1000.
//! A `VerifiedPool` carries `fee: Option<Fee>`, and this module passes that field
//! through without looking at it — there is no `unwrap_or`, no default, and no
//! branch on `PoolType::ConstantProduct` anywhere below. An unknown fee reaching
//! `PoolMeta.fee == None` is the *correct* outcome, and M3's fee math is written to
//! handle it. The test that pins this lives in `tests/verification.rs`; the reason
//! it needs pinning is that the trap is one keystroke wide (`fee.unwrap_or(FEE_300)`
//! compiles and silently makes every discovered pool a Uniswap-v2 clone).

use evm_core::{BlockNumber, EvidenceRef, EvidenceSource, Fee, PoolType, ProtocolId};
use evm_protocol::v2::PROTOCOL_NAME;
use evm_protocol::{AttestationEvidence, PoolAttestation};

use crate::reads::{CallKind, SyncRecord};
use crate::verify::VerifiedPool;

/// The signature string the state evidence is filed under.
pub const SYNC_SIGNATURE: &str = "Sync(uint112,uint112)";

/// One `eth_getCode` / `eth_call` ref: what was asked, of which block.
///
/// No transaction hash and no log index, because there is neither: a read-only
/// call is not a chain event. Leaving the fields `None` rather than borrowing the
/// `Sync` log's position is what keeps "I asked the node" distinguishable from
/// "the chain said" in the evidence file.
fn call_evidence(kind: CallKind, at: BlockNumber) -> EvidenceRef {
    EvidenceRef {
        source: match kind {
            CallKind::Bytecode => EvidenceSource::Bytecode,
            _ => EvidenceSource::EthCall,
        },
        block_number: Some(at),
        transaction_hash: None,
        log_index: None,
        signature: Some(kind.signature().to_string()),
    }
}

/// The `Sync` ref: source `ChainLog`, with the full chain position that produced it.
fn sync_evidence(sync: &SyncRecord) -> EvidenceRef {
    sync.position().evidence(SYNC_SIGNATURE)
}

/// The attestation one verified pool becomes.
pub fn attestation_of(verified: &VerifiedPool) -> PoolAttestation {
    let identity = vec![
        call_evidence(CallKind::Bytecode, verified.pinned_at),
        call_evidence(CallKind::GetReserves, verified.pinned_at),
    ];
    let tokens = vec![
        call_evidence(CallKind::Token0, verified.pinned_at),
        call_evidence(CallKind::Token1, verified.pinned_at),
    ];
    PoolAttestation {
        protocol: ProtocolId::new(PROTOCOL_NAME),
        pool: verified.candidate.pool,
        // The tokens the contract answered, which `verify` has already required to
        // equal the claimed ones. Carrying the verified pair rather than the claim
        // is what makes the attestation independent of the factory having been right.
        token0: verified.token0,
        token1: verified.token1,
        fee: verified.fee,
        pool_type: PoolType::ConstantProduct,
        evidence: AttestationEvidence {
            identity,
            tokens,
            state: vec![sync_evidence(&verified.market_state.sync)],
        },
    }
}

/// The fee of an attestation, exactly as attested — which for every pool
/// discovery has verified so far is `None`.
///
/// This exists so the §14 rule has one named place to be tested against: whatever
/// came in is what goes out, and `None` is a legal, expected answer rather than
/// something to be fixed up.
pub fn fee_of(attestation: &PoolAttestation) -> Option<Fee> {
    attestation.fee
}
