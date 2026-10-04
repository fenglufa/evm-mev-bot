//! The nine negative controls M9.1 §20 asks for, plus §14's fee rule and §22's pin.
//!
//! Every test here hands [`evm_discovery::verify`] a doctored [`CandidateReads`] —
//! the same record type the live run writes into `data/evidence/m9/m9.1` — and
//! asserts the verdict changed. Nothing in this file touches a network: that is the
//! property that makes the controls worth anything, because a control that needs a
//! misbehaving node to produce its bad record cannot be re-run by a reviewer.
//!
//! Each control names the §20 condition it covers in its own doc line, so the
//! evidence table can point at a test instead of paraphrasing it.

mod fixtures;

use alloy_primitives::{Address, Bytes, U256};
use serde_json::Value;

use evm_core::{BlockNumber, ChainId, PoolId};
use evm_discovery::{
    attestation_of, fee_of, CallKind, CallRecord, CandidateReads, DiscoverySource, RejectionReason,
    VerificationStage,
};
use fixtures::{
    abi_address_word, candidate, code_record, failed_record, healthy_candidate, healthy_pool_reads,
    healthy_reads, rejected, sync_of, verified, BLOCK, CHAIN, FACTORY, NOWHERE, OTHER_FACTORY,
    PAIR, TOKEN_A, TOKEN_B,
};

fn reason_of(reads: &CandidateReads) -> RejectionReason {
    rejected(reads).reason
}

fn record_mut(reads: &mut CandidateReads, kind: CallKind) -> &mut CallRecord {
    reads
        .calls
        .iter_mut()
        .find(|record| record.kind == kind)
        .unwrap()
}

fn set_return(reads: &mut CandidateReads, kind: CallKind, data: Bytes) {
    let record = record_mut(reads, kind);
    record.return_data = Some(data);
    record.error = None;
}

/// Records rewritten to be about the address the claim names, the way the collector
/// would have written them. Without this, a wrong-pair test would trip the
/// attachment check instead of the check it means to exercise.
fn aim_at(reads: &mut CandidateReads, address: Address) {
    for record in &mut reads.calls {
        record.target = address;
    }
    if let Some(sync) = &mut reads.sync {
        sync.pool.address = address;
    }
}

/// How `verify` writes an address into a rejection detail: the lowercase hex every
/// evidence row in this project also uses. Asserted through a helper so a test does
/// not fail because a formatter was changed.
fn hexed(value: Address) -> String {
    format!("{value:?}")
}

/// §22, and the shape every evidence row has to carry: a verified pool records the
/// block it was decided at, and each read records its own.
#[test]
fn a_verified_pool_keeps_the_block_every_read_was_pinned_at() {
    let reads = healthy_pool_reads();
    let pool = verified(&reads);

    assert_eq!(pool.pinned_at, reads.candidate.discovery_block());
    assert_eq!(pool.pinned_at, BlockNumber(BLOCK));
    assert_eq!(pool.contract_state.read_at_block, BlockNumber(BLOCK));
    assert_eq!(pool.token0.address, TOKEN_A);
    assert_eq!(pool.token1.address, TOKEN_B);
    assert!(pool.market_state.prices_anything());
    for record in &reads.calls {
        assert_eq!(
            record.pinned_at,
            pool.pinned_at,
            "{} lost its pin",
            record.kind.signature()
        );
    }
}

/// §14 / §20 "unknown fee becoming 997/1000": the unknown has to survive the whole
/// bridge to `PoolAttestation`, in the record itself and not just in a field some
/// caller agrees to ignore.
#[test]
fn an_unknown_fee_stays_unknown_all_the_way_into_the_attestation() {
    let reads = healthy_pool_reads();
    let pool = verified(&reads);
    assert_eq!(pool.fee, None);

    let attestation = attestation_of(&pool);
    assert_eq!(fee_of(&attestation), None);
    assert_eq!(attestation.to_meta().fee, None);
    assert!(attestation.evidence.is_complete());

    let text = serde_json::to_string(&attestation).unwrap();
    assert!(
        text.contains("\"fee\":null"),
        "the attestation must carry the fee as an explicit unknown: {text}"
    );
    // A substituted fee would have to be *written down*, so its shape and its two
    // forbidden numbers must be absent from the record entirely.
    assert!(!text.contains("numerator"), "a fee was invented: {text}");
    assert!(
        !text.contains("997"),
        "997/1000 leaked into the record: {text}"
    );
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed["fee"].is_null());
    assert_eq!(parsed["pool_type"], "ConstantProduct");
}

/// §15 / §20 "fake PairCreated emitter", first half: the emitter is provenance, not
/// a gate. Two candidates that differ only in who emitted the log get the same
/// verdict — a check that consulted the emitter would show up here as a difference.
#[test]
fn swapping_the_emitter_changes_nothing() {
    let claimed = |factory: Address| {
        let mut candidate = healthy_candidate();
        candidate.factory = factory;
        healthy_reads(&candidate)
    };

    let ours = verified(&claimed(FACTORY));
    let theirs = verified(&claimed(OTHER_FACTORY));
    assert_eq!(ours.token0, theirs.token0);
    assert_eq!(ours.token1, theirs.token1);
    assert_eq!(ours.candidate.pool, theirs.candidate.pool);
    assert_ne!(ours.candidate.factory, theirs.candidate.factory);
}

/// §20 "fake PairCreated emitter", second half — the part that actually has to
/// reject: a claim about an address no contract lives at. Whichever factory emits
/// it, the identity read says the same thing, and no emitter's address appears in
/// the reason.
#[test]
fn a_claim_about_an_empty_address_is_rejected_whosever_factory_emitted_it() {
    for factory in [FACTORY, OTHER_FACTORY] {
        let mut candidate = candidate(TOKEN_A, TOKEN_B, NOWHERE);
        candidate.factory = factory;
        let mut reads = healthy_reads(&candidate);
        *record_mut(&mut reads, CallKind::Bytecode) = code_record(0);
        aim_at(&mut reads, NOWHERE);

        let rejection = rejected(&reads);
        assert_eq!(rejection.reason, RejectionReason::NoBytecode);
        assert_eq!(rejection.stage, VerificationStage::Identity);
        assert!(
            rejection.detail.contains("0 bytes"),
            "the detail should quote the read: {}",
            rejection.detail
        );
        for address in [FACTORY, OTHER_FACTORY] {
            assert!(
                !rejection.detail.contains(&hexed(address)),
                "the emitter must not appear in a verdict: {}",
                rejection.detail
            );
        }
    }
}

/// §20 "wrong pair address", as it happens on chain: the claim names an address that
/// answers nothing, and the reads aimed at it come back empty. The check that stops
/// it is the contract's, not a list of known pairs.
#[test]
fn a_wrong_pair_address_is_rejected_because_nothing_answers_there() {
    let candidate = candidate(TOKEN_A, TOKEN_B, NOWHERE);
    let mut reads = healthy_reads(&candidate);
    *record_mut(&mut reads, CallKind::Bytecode) = code_record(0);
    aim_at(&mut reads, NOWHERE);
    reads
        .calls
        .retain(|record| record.kind == CallKind::Bytecode);

    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::NoBytecode);
    assert_eq!(rejection.candidate.pool.address, NOWHERE);
}

/// §20 "wrong pair address", the doctored version: healthy reads belonging to a
/// different pool cannot retire a claim about this one. This is what makes the
/// pairing of claim and reads in the evidence file auditable — a pure `verify` would
/// otherwise trust whatever address a collector attached.
#[test]
fn readings_from_another_pool_are_never_this_candidate_s() {
    let mut reads = healthy_pool_reads();
    reads.candidate.pool.address = NOWHERE;

    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::ReadAboutAnotherPool);
    assert_eq!(rejection.stage, VerificationStage::Provenance);
    assert!(rejection.detail.contains(&hexed(PAIR)));
    assert!(rejection.detail.contains(&hexed(NOWHERE)));

    // The state read is aimed the same way: a `Sync` from a pool that is not this
    // one is not this pool's publication.
    let mut reads = healthy_pool_reads();
    let other = PoolId::new(ChainId(CHAIN), NOWHERE);
    reads.sync = Some(sync_of(other, U256::from(1u32), U256::from(2u32), BLOCK));
    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::ReadAboutAnotherPool);
    assert!(rejection.detail.contains("Sync"));
}

/// §20 "wrong token address": the factory's claim and the contract's answer are two
/// independent statements, and the contract wins.
#[test]
fn a_wrong_token_address_is_rejected_because_the_contract_disagrees() {
    let mut reads = healthy_pool_reads();
    set_return(&mut reads, CallKind::Token0, abi_address_word(NOWHERE));

    let rejection = rejected(&reads);
    assert_eq!(
        rejection.reason,
        RejectionReason::ClaimedTokensDisagreeWithContract
    );
    assert_eq!(rejection.stage, VerificationStage::Tokens);
    assert!(rejection.detail.contains(&hexed(NOWHERE)));
    assert!(rejection.detail.contains(&hexed(TOKEN_A)));

    // A token read that is not an address word at all is a different failure —
    // unreadable, not disagreeing — and stays in the same stage.
    let mut reads = healthy_pool_reads();
    set_return(&mut reads, CallKind::Token1, Bytes::from(vec![7u8; 32]));
    assert_eq!(
        reason_of(&reads),
        RejectionReason::TokensUnreadable,
        "a non-address word must not be truncated into an address"
    );
}

/// §11 / §20 "same token on both sides": not a market, whether the claim says so
/// honestly and the contract confirms it.
#[test]
fn the_same_token_on_both_sides_is_rejected() {
    let reads = healthy_reads(&candidate(TOKEN_A, TOKEN_A, PAIR));

    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::SameTokenOnBothSides);
    assert_eq!(rejection.stage, VerificationStage::Tokens);
    assert!(rejection.detail.contains(&hexed(TOKEN_A)));
}

/// §20 "wrong chain": one candidate has to be about one chain. Provenance is checked
/// before content, so a cross-chain mixture stops at the first field, whatever the
/// reads say.
#[test]
fn a_candidate_spanning_two_chains_is_rejected_before_the_reads_are_read() {
    let mut reads = healthy_pool_reads();
    reads.candidate.pool.chain_id = ChainId(31337);
    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::ChainMismatch);
    assert_eq!(rejection.stage, VerificationStage::Provenance);
    assert!(rejection.detail.contains(&CHAIN.to_string()));
    assert!(rejection.detail.contains("31337"));

    for which in [0u8, 1] {
        let mut reads = healthy_pool_reads();
        if which == 0 {
            reads.candidate.claimed_token0.chain_id = ChainId(31337);
        } else {
            reads.candidate.claimed_token1.chain_id = ChainId(31337);
        }
        assert_eq!(reason_of(&reads), RejectionReason::ChainMismatch);
    }
}

/// §20 "missing state evidence": a contract that answers every question is still not
/// a pool anyone can price. The two kinds of silence are different findings and the
/// detail has to keep them apart.
#[test]
fn a_candidate_with_no_state_evidence_is_rejected_at_the_state_stage() {
    let mut reads = healthy_pool_reads();
    reads.sync = None;
    reads.sync_logs_seen = 0;
    let quiet = rejected(&reads);
    assert_eq!(quiet.reason, RejectionReason::NoAuthoritativeState);
    assert_eq!(quiet.stage, VerificationStage::State);
    assert!(quiet.detail.contains(&BLOCK.to_string()));

    let mut reads = healthy_pool_reads();
    reads.sync = None;
    reads.sync_logs_seen = 7;
    let undecodable = rejected(&reads);
    assert_eq!(undecodable.reason, RejectionReason::NoAuthoritativeState);
    assert!(undecodable.detail.contains("7 logs"));
    assert_ne!(
        quiet.detail, undecodable.detail,
        "\"never emitted\" and \"emitted something we could not read\" must not collapse"
    );
}

/// §22: reads that succeeded at the wrong block are not evidence about this
/// candidate, and that has to be its own reason rather than a failed read.
#[test]
fn a_read_at_the_wrong_block_is_rejected_as_unpinned() {
    let mut reads = healthy_pool_reads();
    reads.pinned_at = BlockNumber(BLOCK + 5);
    let rejection = rejected(&reads);
    assert_eq!(
        rejection.reason,
        RejectionReason::ReadNotPinnedAtDiscoveryBlock
    );
    assert!(rejection.detail.contains(&BLOCK.to_string()));
    assert!(rejection.detail.contains(&(BLOCK + 5).to_string()));

    // One record off the pin is enough, and the detail names which question was
    // asked at the wrong height.
    let mut reads = healthy_pool_reads();
    record_mut(&mut reads, CallKind::Token1).pinned_at = BlockNumber(BLOCK + 1);
    let rejection = rejected(&reads);
    assert_eq!(
        rejection.reason,
        RejectionReason::ReadNotPinnedAtDiscoveryBlock
    );
    assert!(rejection.detail.contains("token1()"));
}

/// The other §9 identity failures: no bytecode at all, and a `getReserves()` that
/// cannot be read. Both stop at the identity stage, before any token is asked.
#[test]
fn an_unreadable_contract_stops_at_identity() {
    let mut reads = healthy_pool_reads();
    reads.calls.clear();
    assert_eq!(reason_of(&reads), RejectionReason::NoBytecode);

    let mut reads = healthy_pool_reads();
    *record_mut(&mut reads, CallKind::Bytecode) = code_record(0);
    assert_eq!(reason_of(&reads), RejectionReason::NoBytecode);

    let mut reads = healthy_pool_reads();
    *record_mut(&mut reads, CallKind::GetReserves) =
        failed_record(CallKind::GetReserves, "execution reverted");
    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::ReservesUnreadable);
    assert_eq!(rejection.stage, VerificationStage::Identity);
    assert!(rejection.detail.contains("execution reverted"));

    // Two words back instead of three is a short answer, not a missing one, and the
    // detail quotes the length it found.
    let mut reads = healthy_pool_reads();
    set_return(
        &mut reads,
        CallKind::GetReserves,
        Bytes::from(vec![0u8; 64]),
    );
    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::ReservesUnreadable);
    assert!(rejection.detail.contains("64 bytes"));
}

/// The §20 condition that lives one layer up — "unverified pool entering Registry" —
/// starts here: there is no path from a rejection to a `PoolAttestation`, because
/// `attestation_of` only accepts a `VerifiedPool`. The registry half is in
/// `tests/pipeline.rs`.
#[test]
fn only_a_verified_pool_has_an_attestation() {
    let mut reads = healthy_pool_reads();
    reads.sync = None;
    let rejection = rejected(&reads);
    assert_eq!(rejection.reason, RejectionReason::NoAuthoritativeState);

    // And the attestation names the read behind each of its three evidence buckets,
    // filed under the question it answers rather than under one convenient log.
    let reads = healthy_pool_reads();
    let attestation = attestation_of(&verified(&reads));
    let sync = reads.sync.unwrap();
    let kinds: Vec<&str> = attestation
        .evidence
        .identity
        .iter()
        .chain(attestation.evidence.tokens.iter())
        .map(|evidence| evidence.signature.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "eth_getCode(address)",
            "getReserves()",
            "token0()",
            "token1()"
        ]
    );
    let state = &attestation.evidence.state[0];
    assert_eq!(
        state.signature.as_deref(),
        Some(evm_discovery::SYNC_SIGNATURE)
    );
    // The state ref points at the log the `Sync` came from, position and all.
    assert_eq!(state.log_index, Some(sync.log_index.0));
    assert_eq!(state.block_number, Some(sync.block_number));
    assert!(state.transaction_hash.is_some());
    for evidence in attestation
        .evidence
        .identity
        .iter()
        .chain(attestation.evidence.tokens.iter())
    {
        assert_eq!(evidence.block_number, Some(BlockNumber(BLOCK)));
        assert!(
            evidence.transaction_hash.is_none(),
            "an eth_call is not a chain event and must not borrow a transaction hash"
        );
    }
}

/// A candidate's source and its claim survive into the verified record untouched:
/// verification adds conclusions, it does not rewrite what was claimed.
#[test]
fn verification_adds_conclusions_without_rewriting_the_claim() {
    let reads = healthy_pool_reads();
    let pool = verified(&reads);
    assert_eq!(pool.candidate, reads.candidate);
    assert_eq!(
        pool.candidate.source,
        DiscoverySource::HistoricalPairCreated
    );
    assert_eq!(pool.candidate.pair_index, reads.candidate.pair_index);
    assert_eq!(pool.candidate.discovered_at, reads.candidate.discovered_at);
}
