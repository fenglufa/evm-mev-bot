//! Builders for the records verification reads, shared by this crate's targets.
//!
//! Everything here fabricates a *record*, never a chain: the point is that
//! [`evm_discovery::verify`] decides from what a collector wrote down, so a test can
//! hand it a doctored note and watch the verdict change. The one exception is
//! `historical_live.rs`, which runs the same functions against a real node.
//!
//! The addresses are real ones this project already carries, so a reader comparing a
//! fixture row against committed evidence recognises the shape: the pair, its tokens
//! and the second-pair pools come from `data/protocols/v2-giwap-sepolia.json` and
//! `data/protocols/v2-narusswap-sepolia.json`, the two-pools-on-one-pair triple from
//! `data/evidence/m7/census-pair-created.json`, and the emitter from
//! `data/evidence/m7/probe-pair-created-log-shape.json`. No verification rule
//! consults any of them — §8 forbids discovery depending on a list, and §15 forbids
//! the emitter address being a reason to trust a claim — so these constants are
//! recognisable, not privileged. The *pairings* are real too, except where a test
//! says it is putting two of these addresses together to exercise graph shape.

#![allow(dead_code)]

use alloy_primitives::{address, Address, Bytes, B256, U256};

use evm_chain::{ChainLog, LogFilter};
use evm_core::{BlockNumber, ChainId, LogIndex, PoolId, TokenId, TxHash, TxIndex};
use evm_discovery::{
    verify, CallKind, CallRecord, CandidatePool, CandidateReads, DiscoverySource, RejectedPool,
    SyncRecord, Verification, VerifiedPool,
};
use evm_protocol::{LogPosition, PoolCreatedEvent};

pub const CHAIN: u64 = 91342;
pub const TOKEN_A: Address = address!("0x304912af0ce0dd6479735634d567715107bdc0c6");
pub const TOKEN_B: Address = address!("0x4200000000000000000000000000000000000006");
/// The third token this chain's committed registry attests.
pub const TOKEN_C: Address = address!("0xfa0d1d1703b55929e262d9301a001d30e45b97a2");
/// A token from the M7 census's multi-venue pairs, with two real pools on one pair.
pub const TOKEN_D: Address = address!("0x000d8dad0881c061d74911fd3ff45da41c6c2cb2");
pub const PAIR: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");
pub const PAIR_2: Address = address!("0x8df9062fc2995b06de754c040f1f7eac411abbdb");
pub const PAIR_3: Address = address!("0x4db758ab5e494d5b81627dd39258240aa9db9d46");
/// The two pools `census-pair-created.json` reports on `TOKEN_D`/`TOKEN_B`, and the
/// factories that emitted each claim, at the block both were created.
pub const MULTI_POOL_1: Address = address!("0x5543d6dadbbc4a6ec474243a154177d066469ea4");
pub const MULTI_POOL_2: Address = address!("0xd9f800f5b2dc606794d18e143f47ad6517021341");
pub const MULTI_FACTORY_1: Address = address!("0xe636b01458237b8372794180ac4978ca28d49378");
pub const MULTI_FACTORY_2: Address = address!("0x9fd6aaea57eb46aece9c49fcdba2d0ae9309a6e4");
pub const MULTI_BLOCK: u64 = 37_190_158;
/// A pool on a *different* pair, from the same census (`multi_venue_pairs`, row
/// `0x0745e840…-0x4200…06`), so a test can hold three pools without two of them
/// sharing a market. Its claim came from the Giwap factory below.
pub const TOKEN_E: Address = address!("0x0745e84011b0558a37eb4bfc894b5a29c3876f29");
pub const OTHER_PAIR_POOL: Address = address!("0x616d834775633f027b86df65dfcae1d5c2239d09");
pub const OTHER_PAIR_BLOCK: u64 = 37_187_564;
pub const FACTORY: Address = address!("0x5f6e8a566c0944af181f158e6dee60bafbaf3b9e");
/// A second real factory, for the control that shows the emitter changes nothing.
pub const OTHER_FACTORY: Address = address!("0xe5eed7d40a3f63c6277246847a478a16dda4386a");
pub const BLOCK: u64 = 37_187_701;
/// An address with no pool behind it, for the wrong-pair control.
pub const NOWHERE: Address = address!("0x00000000000000000000000000000000deadbeef");

/// The `PairCreated` log as this chain actually emits it: three topics, the pair and
/// the index as two data words.
pub fn pair_created_log(
    factory: Address,
    token0: Address,
    token1: Address,
    pair: Address,
    block: u64,
    log_index: u64,
) -> ChainLog {
    let mut data = Vec::with_capacity(64);
    data.extend(abi_word(pair));
    data.extend(U256::from(41u32).to_be_bytes::<32>());
    ChainLog {
        chain_id: ChainId(CHAIN),
        block_number: BlockNumber(block),
        tx_hash: TxHash(B256::left_padding_from(&[block as u8])),
        tx_index: TxIndex(1),
        log_index: LogIndex(log_index),
        address: factory,
        topics: vec![
            evm_discovery::pair_created_topic0(),
            address_topic(token0),
            address_topic(token1),
        ],
        data: Bytes::from(data),
    }
}

/// The general claim: any emitter, any pair, any chain position. `log_index` is a
/// parameter because the state store applies updates in strictly ascending chain
/// position, so two pools' records in one block have to sit at two different log
/// indexes — exactly as a real block emits them.
pub fn claim_at(
    factory: Address,
    token0: Address,
    token1: Address,
    pair: Address,
    block: u64,
    log_index: u64,
) -> PoolCreatedEvent {
    PoolCreatedEvent {
        position: LogPosition {
            chain_id: ChainId(CHAIN),
            block_number: BlockNumber(block),
            tx_hash: TxHash(B256::left_padding_from(&[7u8])),
            tx_index: TxIndex(3),
            log_index: LogIndex(log_index),
        },
        factory,
        pool: PoolId::new(ChainId(CHAIN), pair),
        token0: TokenId::new(ChainId(CHAIN), token0),
        token1: TokenId::new(ChainId(CHAIN), token1),
        pair_index: U256::from(41u32),
    }
}

/// The same claim, decoded — i.e. what a source hands to a candidate.
pub fn claim(token0: Address, token1: Address, pair: Address) -> PoolCreatedEvent {
    claim_at(FACTORY, token0, token1, pair, BLOCK, 4)
}

/// A candidate from that claim, in the shape the scan emits.
pub fn candidate_at(
    factory: Address,
    token0: Address,
    token1: Address,
    pair: Address,
    block: u64,
    log_index: u64,
) -> CandidatePool {
    CandidatePool::from_claim(
        &claim_at(factory, token0, token1, pair, block, log_index),
        DiscoverySource::HistoricalPairCreated,
    )
}

pub fn candidate(token0: Address, token1: Address, pair: Address) -> CandidatePool {
    candidate_at(FACTORY, token0, token1, pair, BLOCK, 4)
}

/// A healthy candidate on this chain's real pair, tokens in the order the pair
/// itself answers them.
pub fn healthy_candidate() -> CandidatePool {
    candidate(TOKEN_A, TOKEN_B, PAIR)
}

pub fn call_record(kind: CallKind, data: Bytes) -> CallRecord {
    record_with(kind, Some(data), None, None, None)
}

/// A bytecode read that answered `length` bytes. The digest is keccak256 of the
/// empty input — a placeholder: no verification rule reads the digest field, only
/// `code_len > 0`, and the live collector fills this one from the code it fetched.
pub fn code_record(length: usize) -> CallRecord {
    record_with(
        CallKind::Bytecode,
        None,
        None,
        Some(length),
        Some("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470".to_string()),
    )
}

pub fn failed_record(kind: CallKind, message: &str) -> CallRecord {
    record_with(kind, None, Some(message.to_string()), None, None)
}

fn record_with(
    kind: CallKind,
    return_data: Option<Bytes>,
    error: Option<String>,
    code_len: Option<usize>,
    code_digest: Option<String>,
) -> CallRecord {
    CallRecord {
        kind,
        target: PAIR,
        pinned_at: BlockNumber(BLOCK),
        return_data,
        error,
        code_len,
        code_digest,
    }
}

/// The four reads of a pool that answers like the pair it claims to be, in the order
/// the collector writes them down.
pub fn healthy_calls(candidate: &CandidatePool) -> Vec<CallRecord> {
    let mut calls = vec![
        pinned(code_record(24_000), candidate),
        pinned(
            call_record(
                CallKind::Token0,
                abi_address_word(candidate.claimed_token0.address),
            ),
            candidate,
        ),
        pinned(
            call_record(
                CallKind::Token1,
                abi_address_word(candidate.claimed_token1.address),
            ),
            candidate,
        ),
        pinned(
            call_record(
                CallKind::GetReserves,
                reserves(
                    U256::from(35_099_900_253_008u128),
                    U256::from(45_655_538_604_883_371_699_u128),
                ),
            ),
            candidate,
        ),
    ];
    calls.sort_by_key(|record| record.identity());
    calls
}

/// One record moved onto the candidate it belongs to. The builders above default to
/// `PAIR`/`BLOCK` because most tests edit a healthy set, and a record's target and
/// block are what `verify` checks it against.
fn pinned(mut record: CallRecord, candidate: &CandidatePool) -> CallRecord {
    record.target = candidate.pool.address;
    record.pinned_at = candidate.discovery_block();
    record
}

pub fn sync_at(
    pool: PoolId,
    reserve0: U256,
    reserve1: U256,
    block: u64,
    log_index: u64,
) -> SyncRecord {
    SyncRecord {
        pool,
        block_number: BlockNumber(block),
        tx_hash: TxHash(B256::left_padding_from(&[9u8])),
        tx_index: TxIndex(2),
        log_index: LogIndex(log_index),
        reserve0,
        reserve1,
    }
}

pub fn sync_of(pool: PoolId, reserve0: U256, reserve1: U256, block: u64) -> SyncRecord {
    sync_at(pool, reserve0, reserve1, block, block)
}

/// Records for a candidate that passes everything, with its `Sync` at the position
/// the claim itself names.
pub fn healthy_reads(candidate: &CandidatePool) -> CandidateReads {
    let claim_index = candidate.discovered_at.log_index.0;
    CandidateReads {
        candidate: candidate.clone(),
        pinned_at: candidate.discovery_block(),
        calls: healthy_calls(candidate),
        sync: Some(sync_at(
            candidate.pool,
            U256::from(1_000u32),
            U256::from(2_000u32),
            candidate.discovery_block().0,
            claim_index,
        )),
        sync_logs_seen: 1,
        state_search_to_block: candidate.discovery_block(),
    }
}

/// The healthy candidate at an explicit chain position, which is how a test puts two
/// pools in one block without giving them one log index.
pub fn healthy_reads_at(
    factory: Address,
    token0: Address,
    token1: Address,
    pair: Address,
    block: u64,
    log_index: u64,
) -> CandidateReads {
    healthy_reads(&candidate_at(
        factory, token0, token1, pair, block, log_index,
    ))
}

/// A filled `CandidateReads` for the healthy pair, the baseline every control edits.
pub fn healthy_pool_reads() -> CandidateReads {
    healthy_reads(&healthy_candidate())
}

/// The verdict, expecting verified. A control that stops being a control because its
/// bad record now passes has to say which candidate let it through.
pub fn verified(reads: &CandidateReads) -> VerifiedPool {
    match verify(reads) {
        Verification::Verified(pool) => pool,
        Verification::Rejected(rejection) => panic!(
            "expected verification, got {}: {}",
            rejection.reason.as_str(),
            rejection.detail
        ),
    }
}

/// The verdict, expecting rejection. Returns the rejection, so each test can assert
/// the reason, the stage, and that the detail is the record's own words.
pub fn rejected(reads: &CandidateReads) -> RejectedPool {
    match verify(reads) {
        Verification::Rejected(rejection) => rejection,
        Verification::Verified(pool) => {
            let (chain, pool_address, block, tx_index, log_index) = pool.identity();
            panic!(
                "expected rejection, verified chain {chain} pool {pool_address} \
                 at block {block} tx {tx_index} log {log_index}"
            )
        }
    }
}

/// One `address` as the ABI word a call returns it in.
pub fn abi_address_word(value: Address) -> Bytes {
    Bytes::from(abi_word(value).to_vec())
}

fn abi_word(value: Address) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(value.as_slice());
    word
}

fn address_topic(value: Address) -> B256 {
    B256::from_slice(&abi_word(value))
}

/// `getReserves()`' three words: two `uint112` and a `uint32` timestamp.
pub fn reserves(reserve0: U256, reserve1: U256) -> Bytes {
    let mut data = Vec::with_capacity(96);
    data.extend(reserve0.to_be_bytes::<32>());
    data.extend(reserve1.to_be_bytes::<32>());
    data.extend(U256::from(1_790_536_285u64).to_be_bytes::<32>());
    Bytes::from(data)
}

/// The filter a whole-window scan of `from..=to` asks the node for: one topic0, no
/// address list (§8 — discovery asks the chain, not a list of known factories).
pub fn window_filter(from: u64, to: u64) -> LogFilter {
    LogFilter {
        from_block: BlockNumber(from),
        to_block: BlockNumber(to),
        addresses: Vec::new(),
        topics: vec![Some(vec![evm_discovery::pair_created_topic0()])],
    }
}
