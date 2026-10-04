//! Turning records into a verdict, without asking anything.
//!
//! This is the only decision function in the crate, and it is pure: it takes
//! [`CandidateReads`] — records a collector wrote down — and returns verified or
//! rejected. No adapter, no clock, no "latest". Two consequences worth having
//! explicitly:
//!
//! - the same records always produce the same verdict, so the §21 determinism
//!   requirement is a property of this function rather than a hope about the run;
//! - a reviewer with the committed records can re-run the decision, and a negative
//!   control can hand it a doctored one, without a node that behaves badly.
//!
//! The checks run in the order M9.1 §9 declares: identity, then the token pair,
//! then state. Order matters for the evidence tables — a pool whose contract does
//! not answer `token0()` has not "failed the state check", it never got asked —
//! so the first stage that fails is the one recorded.
//!
//! ## The two kinds of "state", kept apart
//!
//! `getReserves()` answers what the contract holds at the pinned block. That is
//! [`ContractVerificationState`]: proof about a contract, used to decide whether
//! the address behaves like a V2 pair.
//!
//! A `Sync(uint112,uint112)` log is the pool's own published statement of its
//! reserves. That is [`AuthoritativeMarketState`], and it is the only thing here
//! that becomes a `PoolState`. §12 says plainly not to let the first replace the
//! second, and the way that goes wrong in real code is a convenient `eth_call`
//! result being written into the store because it had the same field names. They
//! are different types with different producers, so the substitution has to be
//! deliberate to happen at all.

use alloy_primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};

use evm_core::{BlockNumber, Fee, PoolState, TokenId};
use evm_protocol::signatures;

use crate::candidate::CandidatePool;
use crate::reads::{CallKind, CallRecord, CandidateReads, SyncRecord};

/// Which of §9's three checks a candidate reached, and where it stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum VerificationStage {
    /// The candidate's own identities disagree (a log from another chain reached
    /// this point), or the reads are not attached to this claim at all — wrong
    /// block, wrong address. Checked before anything is trusted, including content.
    Provenance,
    /// Bytecode and `getReserves()`: is there a V2-shaped contract here.
    Identity,
    /// `token0()` / `token1()`, against the claim and against each other.
    Tokens,
    /// A `Sync` the pool published itself.
    State,
}

impl VerificationStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Provenance => "provenance",
            Self::Identity => "identity",
            Self::Tokens => "tokens",
            Self::State => "state",
        }
    }
}

/// Why a candidate stopped being a candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RejectionReason {
    /// The claim's chain id, the pool's chain id and the tokens' chain id are not
    /// one chain. Nothing downstream can tell a wrong chain apart from a wrong
    /// pool, so this stops here.
    ChainMismatch,
    /// A read was made at a block other than the candidate's discovery block.
    /// Not the same thing as a failed read: it succeeded against the wrong chain
    /// state, which is the failure §22 exists to prevent.
    ReadNotPinnedAtDiscoveryBlock,
    /// A read was made of an address other than the claimed pool. `verify` is
    /// pure, so the address a record names is the only thing tying it to the
    /// candidate it claims to describe; without this check a claim could be
    /// retired by another pool's healthy reads.
    ReadAboutAnotherPool,
    /// `eth_getCode` answered with nothing, or could not be asked.
    NoBytecode,
    /// `getReserves()` reverted, errored, or returned fewer than three words.
    ReservesUnreadable,
    /// `token0()` or `token1()` errored or returned something not an address word.
    TokensUnreadable,
    /// The contract answers a different token than the factory claimed. The claim
    /// and the contract are two independent statements; agreeing is the check.
    ClaimedTokensDisagreeWithContract,
    /// `token0() == token1()`. Not a market: §11 refuses it, and the registry
    /// refuses it again on the way in.
    SameTokenOnBothSides,
    /// The pool never published a `Sync` in the range searched, so nobody knows
    /// what is in it. Verified as a contract; not a tradable pool.
    NoAuthoritativeState,
}

impl RejectionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ChainMismatch => "chain_mismatch",
            Self::ReadNotPinnedAtDiscoveryBlock => "read_not_pinned_at_discovery_block",
            Self::ReadAboutAnotherPool => "read_about_another_pool",
            Self::NoBytecode => "no_bytecode",
            Self::ReservesUnreadable => "reserves_unreadable",
            Self::TokensUnreadable => "tokens_unreadable",
            Self::ClaimedTokensDisagreeWithContract => "claimed_tokens_disagree_with_contract",
            Self::SameTokenOnBothSides => "same_token_on_both_sides",
            Self::NoAuthoritativeState => "no_authoritative_state",
        }
    }

    /// Which stage this reason can only come from — so a table that pairs a
    /// reason with a stage cannot quietly invent a combination.
    pub fn stage(&self) -> VerificationStage {
        match self {
            Self::ChainMismatch => VerificationStage::Provenance,
            Self::ReadNotPinnedAtDiscoveryBlock | Self::ReadAboutAnotherPool => {
                VerificationStage::Provenance
            }
            Self::NoBytecode | Self::ReservesUnreadable => VerificationStage::Identity,
            Self::TokensUnreadable
            | Self::ClaimedTokensDisagreeWithContract
            | Self::SameTokenOnBothSides => VerificationStage::Tokens,
            Self::NoAuthoritativeState => VerificationStage::State,
        }
    }
}

/// A candidate that stopped, with the record behind the stop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedPool {
    pub candidate: CandidatePool,
    pub stage: VerificationStage,
    pub reason: RejectionReason,
    /// The read's own words — a node error, a return length, the addresses that
    /// disagreed. Required, because "rejected: 6" means nothing without each row
    /// saying why that row was rejected (M9.1 §19).
    pub detail: String,
}

impl RejectedPool {
    /// Row identity: the claim it stopped at, plus the stage it stopped at. The
    /// stage is part of the key rather than a column because one candidate can be
    /// re-run after a different read failed, and the two rows are different
    /// findings — collapsing them by candidate alone would lose that.
    pub fn identity(&self) -> (u64, Address, u64, u64, u64, u8) {
        let (chain, pool, block, tx, log) = self.candidate.identity();
        (chain, pool, block, tx, log, self.stage as u8)
    }
}

/// What `getReserves()` answered at the pinned block. Proof about a contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractVerificationState {
    pub reserve0: U256,
    pub reserve1: U256,
    pub block_timestamp_last: U256,
    pub read_at_block: BlockNumber,
}

/// What the pool's own `Sync` published. The only thing that becomes a `PoolState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritativeMarketState {
    pub sync: SyncRecord,
}

impl AuthoritativeMarketState {
    /// The store's shape, built only from the `Sync` record. Note there is no
    /// constructor here that accepts `ContractVerificationState` — that is the
    /// §12 rule made structural.
    pub fn to_pool_state(&self) -> PoolState {
        PoolState {
            pool: self.sync.pool,
            reserve0: self.sync.reserve0,
            reserve1: self.sync.reserve1,
            block_number: self.sync.block_number,
            log_index: self.sync.log_index,
        }
    }

    /// A zero-reserve side is the pool's own statement and stays recorded, but it
    /// prices nothing: the graph will skip this pool with `EmptySide`. Saying so
    /// here keeps "attested" and "tradable" from collapsing into one word.
    pub fn prices_anything(&self) -> bool {
        self.to_pool_state().has_valid_reserves()
    }
}

/// A candidate that passed all three checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedPool {
    pub candidate: CandidatePool,
    /// The tokens the contract itself answered — not the ones the factory
    /// claimed, even though they had better be the same.
    pub token0: TokenId,
    pub token1: TokenId,
    pub contract_state: ContractVerificationState,
    pub market_state: AuthoritativeMarketState,
    /// Always `None` for a pool discovery verified (M9.1 §14). Kept as a field so
    /// the unknown is visible in the record rather than absent from it.
    pub fee: Option<Fee>,
    pub pinned_at: BlockNumber,
}

impl VerifiedPool {
    /// Row identity: the candidate it came from. A verified pool and its candidate
    /// are the same row in two tables, so they share a key.
    pub fn identity(&self) -> (u64, Address, u64, u64, u64) {
        self.candidate.identity()
    }
}

/// The verdict for one candidate.
///
/// `large_enum_variant` fires because a `VerifiedPool` holds every read that
/// produced it (~560 bytes) against a `RejectedPool`'s ~250. Boxing the larger arm
/// only moves the lint to the other one, and the numbers this enum ever holds are
/// a few thousand candidates in one window: the whole verdict set is a couple of
/// megabytes, in a diagnosis-only path that never runs on a hot loop. The two
/// variants stay values so evidence assembly can borrow either without a deref
/// layer, which is the shape the tables need.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verification {
    Verified(VerifiedPool),
    Rejected(RejectedPool),
}

impl Verification {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    pub fn rejection(&self) -> Option<&RejectedPool> {
        match self {
            Self::Rejected(rejection) => Some(rejection),
            Self::Verified(_) => None,
        }
    }
}

/// Decide from records. See the module doc for why this is the only decision point.
pub fn verify(reads: &CandidateReads) -> Verification {
    let candidate = &reads.candidate;

    // Provenance first: the claim has to be about one chain, and every read has to
    // have been made at the block the claim names. Both are preconditions for the
    // reads meaning anything at all, so they are checked before their content.
    if candidate.discovered_at.chain_id != candidate.pool.chain_id
        || candidate.claimed_token0.chain_id != candidate.pool.chain_id
        || candidate.claimed_token1.chain_id != candidate.pool.chain_id
    {
        return reject(
            candidate,
            RejectionReason::ChainMismatch,
            format!(
                "claim chain {}, pool chain {}, token0 chain {}, token1 chain {}",
                candidate.discovered_at.chain_id.0,
                candidate.pool.chain_id.0,
                candidate.claimed_token0.chain_id.0,
                candidate.claimed_token1.chain_id.0
            ),
        );
    }
    if reads.pinned_at != candidate.discovery_block() {
        return reject(
            candidate,
            RejectionReason::ReadNotPinnedAtDiscoveryBlock,
            format!(
                "reads were taken at block {}, discovery was at {}",
                reads.pinned_at.0,
                candidate.discovery_block().0
            ),
        );
    }
    if let Some(early) = reads.calls.iter().find(|c| c.pinned_at != reads.pinned_at) {
        return reject(
            candidate,
            RejectionReason::ReadNotPinnedAtDiscoveryBlock,
            format!(
                "{} was read at block {}, not {}",
                early.kind.signature(),
                early.pinned_at.0,
                reads.pinned_at.0
            ),
        );
    }
    if let Some(detail) = stray_read(reads) {
        return reject(candidate, RejectionReason::ReadAboutAnotherPool, detail);
    }

    // Identity.
    let bytecode = reads.call(CallKind::Bytecode);
    if !bytecode.is_some_and(|record| record.succeeded() && record.code_len.is_some_and(|l| l > 0))
    {
        let detail = match bytecode {
            Some(record) => bytecode_detail(record),
            None => "no eth_getCode record was collected at all".to_string(),
        };
        return reject(candidate, RejectionReason::NoBytecode, detail);
    }
    let contract_state = match contract_state_of(reads) {
        Ok(state) => state,
        Err(detail) => return reject(candidate, RejectionReason::ReservesUnreadable, detail),
    };

    // Tokens: read them, compare them to the claim, then to each other.
    let answered0 = match call_token(reads, CallKind::Token0) {
        Ok(address) => address,
        Err(detail) => return reject(candidate, RejectionReason::TokensUnreadable, detail),
    };
    let answered1 = match call_token(reads, CallKind::Token1) {
        Ok(address) => address,
        Err(detail) => return reject(candidate, RejectionReason::TokensUnreadable, detail),
    };
    if answered0 != candidate.claimed_token0.address
        || answered1 != candidate.claimed_token1.address
    {
        return reject(
            candidate,
            RejectionReason::ClaimedTokensDisagreeWithContract,
            format!(
                "contract answers {:?} / {:?}, the claim says {:?} / {:?}",
                answered0,
                answered1,
                candidate.claimed_token0.address,
                candidate.claimed_token1.address
            ),
        );
    }
    if answered0 == answered1 {
        return reject(
            candidate,
            RejectionReason::SameTokenOnBothSides,
            format!("{answered0:?} on both sides"),
        );
    }

    // State: the pool's own publication, or nothing.
    let Some(sync) = reads.sync else {
        return reject(
            candidate,
            RejectionReason::NoAuthoritativeState,
            format!(
                "no Sync(uint112,uint112) from this pool in {}..={} ({} logs of any shape \
                 were seen)",
                candidate.discovery_block().0,
                reads.state_search_to_block.0,
                reads.sync_logs_seen
            ),
        );
    };

    Verification::Verified(VerifiedPool {
        candidate: candidate.clone(),
        token0: TokenId::new(candidate.pool.chain_id, answered0),
        token1: TokenId::new(candidate.pool.chain_id, answered1),
        contract_state,
        market_state: AuthoritativeMarketState { sync },
        fee: None,
        pinned_at: reads.pinned_at,
    })
}

fn reject(candidate: &CandidatePool, reason: RejectionReason, detail: String) -> Verification {
    Verification::Rejected(RejectedPool {
        candidate: candidate.clone(),
        stage: reason.stage(),
        reason,
        detail,
    })
}

/// The first read that is not about the pool the claim names, if there is one.
///
/// `verify` never asks a node anything, so the address written in each record is
/// the only evidence that the records belong to the candidate they are filed under.
/// Checking it is what makes §9's "read the contract itself" unforgeable: a claim
/// about an address that is not a pool cannot be retired by another pool's healthy
/// reads, whether the mixing came from a collector bug or from an edited evidence
/// file being recomputed.
fn stray_read(reads: &CandidateReads) -> Option<String> {
    let claimed = reads.candidate.pool;
    if let Some(record) = reads
        .calls
        .iter()
        .find(|record| record.target != claimed.address)
    {
        return Some(format!(
            "{} was read of {:?}, but the claim names {:?}",
            record.kind.signature(),
            record.target,
            claimed.address
        ));
    }
    match &reads.sync {
        Some(sync) if sync.pool != claimed => Some(format!(
            "the Sync record is from {:?}, but the claim names {:?}",
            sync.pool.address, claimed.address
        )),
        _ => None,
    }
}

fn bytecode_detail(record: &CallRecord) -> String {
    match (&record.error, record.code_len) {
        (Some(message), _) => format!("eth_getCode at block {}: {}", record.pinned_at.0, message),
        (None, Some(length)) => format!(
            "eth_getCode at block {} answered {} bytes, which is an address with no code",
            record.pinned_at.0, length
        ),
        (None, None) => format!(
            "the eth_getCode record at block {} carries neither an answer nor an error",
            record.pinned_at.0
        ),
    }
}

/// The return data one recorded call answered, or the reason there is none to
/// read. The failure text is the record's own — the node's words for an errored
/// call, the shape for a short one — so a rejection can be quoted rather than
/// paraphrased (M9.1 §19).
fn call_bytes(reads: &CandidateReads, kind: CallKind) -> std::result::Result<Bytes, String> {
    let Some(record) = reads.call(kind) else {
        return Err(format!("no {} record was collected", kind.signature()));
    };
    if let Some(message) = &record.error {
        return Err(format!(
            "{} at block {}: {}",
            kind.signature(),
            record.pinned_at.0,
            message
        ));
    }
    record.return_data.clone().ok_or_else(|| {
        format!(
            "{} at block {} returned no data",
            kind.signature(),
            record.pinned_at.0
        )
    })
}

/// `getReserves()` as a contract statement: three words, read at the pinned
/// block. This is [`ContractVerificationState`] — evidence about a contract, and
/// not the pool's market state, which only a `Sync` produces (§12).
fn contract_state_of(
    reads: &CandidateReads,
) -> std::result::Result<ContractVerificationState, String> {
    const WORDS: usize = 3;
    let data = call_bytes(reads, CallKind::GetReserves)?;
    if data.len() < WORDS * 32 {
        return Err(format!(
            "{} at block {} answered {} bytes, fewer than the {} words its declaration \
             returns",
            CallKind::GetReserves.signature(),
            reads.pinned_at.0,
            data.len(),
            WORDS
        ));
    }
    let reserve0 = signatures::word(&data, 0).map_err(|err| err.to_string())?;
    let reserve1 = signatures::word(&data, 1).map_err(|err| err.to_string())?;
    let block_timestamp_last = signatures::word(&data, 2).map_err(|err| err.to_string())?;
    Ok(ContractVerificationState {
        reserve0,
        reserve1,
        block_timestamp_last,
        read_at_block: reads.pinned_at,
    })
}

/// `token0()` / `token1()` as a contract statement.
///
/// Read with the same strict shape rule the event decoder uses (§6): one word
/// whose high 12 bytes are zero. `V2Call::decode_return` would truncate a
/// non-address word into an address that looks plausible, and a candidate passing
/// on a truncated answer is exactly the wrong-kind-of-pool bug this milestone is
/// meant to catch.
fn call_token(reads: &CandidateReads, kind: CallKind) -> std::result::Result<Address, String> {
    let data = call_bytes(reads, kind)?;
    if data.len() < 32 {
        return Err(format!(
            "{} at block {} answered {} bytes, less than one word",
            kind.signature(),
            reads.pinned_at.0,
            data.len()
        ));
    }
    signatures::address_word(&data[..32]).map_err(|err| format!("{}: {}", kind.signature(), err))
}
