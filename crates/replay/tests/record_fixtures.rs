//! Regenerates the deterministic replay fixtures under `fixtures/replay/`, and
//! captures the real block under `fixtures/real/`.
//!
//! ```text
//! cargo test -p evm-replay --test record_fixtures -- --ignored
//! ```
//!
//! Synthetic fixtures are written as normalized `BlockData` JSON so the recorded
//! adapter and the live adapter hand the pipeline byte-identical shapes. Real
//! blocks are captured straight from the RPC, through the same normalization
//! code the pipeline uses, so nothing about them is transcribed by hand.

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address, Bytes, B256, U256};

use evm_chain::{BlockData, ChainAdapter, ChainBlock, ChainLog, ChainReceipt, ChainTransaction};
use evm_core::{BlockNumber, ChainId, LogIndex, TxHash, TxIndex};
use evm_protocol::V2Topics;

const CHAIN: ChainId = ChainId(91342);

const POOL_A: Address = address!("0x1111111111111111111111111111111111111111");
const POOL_B: Address = address!("0x2222222222222222222222222222222222222222");
/// Deliberately absent from every registry: it emits a `Sync`-shaped log anyway.
const UNATTESTED: Address = address!("0x3333333333333333333333333333333333333333");
const TOKEN_0: Address = address!("0x4444444444444444444444444444444444444444");
const TOKEN_1: Address = address!("0x5555555555555555555555555555555555555555");
const TRADER: Address = address!("0x6666666666666666666666666666666666666666");
const RECIPIENT: Address = address!("0x7777777777777777777777777777777777777777");

const TX_ONE: B256 = B256::repeat_byte(0xa1);
const TX_TWO: B256 = B256::repeat_byte(0xa2);
const TX_THREE: B256 = B256::repeat_byte(0xa3);
const TX_FOUR: B256 = B256::repeat_byte(0xa4);
const TX_SOLO: B256 = B256::repeat_byte(0xf1);

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .expect("fixtures/ exists at the workspace root")
}

fn topic_word(address: Address) -> B256 {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(&address.into_array());
    B256::from(word)
}

fn word(value: u128) -> [u8; 32] {
    U256::from(value).to_be_bytes::<32>()
}

fn sync_data(reserve0: u128, reserve1: u128) -> Vec<u8> {
    [word(reserve0), word(reserve1)].concat()
}

fn swap_data(amount0_in: u128, amount1_in: u128, amount0_out: u128, amount1_out: u128) -> Vec<u8> {
    [
        word(amount0_in),
        word(amount1_in),
        word(amount0_out),
        word(amount1_out),
    ]
    .concat()
}

fn log(
    block: u64,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
    address: Address,
    topics: Vec<B256>,
    data: Vec<u8>,
) -> ChainLog {
    ChainLog {
        chain_id: CHAIN,
        block_number: BlockNumber(block),
        tx_hash: TxHash(tx_hash),
        tx_index: TxIndex(tx_index),
        log_index: LogIndex(log_index),
        address,
        topics,
        data: Bytes::from(data),
    }
}

fn sync(
    block: u64,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
    pool: Address,
    r0: u128,
    r1: u128,
) -> ChainLog {
    log(
        block,
        tx_hash,
        tx_index,
        log_index,
        pool,
        vec![V2Topics::default().sync],
        sync_data(r0, r1),
    )
}

fn swap(
    block: u64,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
    pool: Address,
    amounts: [u128; 4],
) -> ChainLog {
    let topics = V2Topics::default();
    log(
        block,
        tx_hash,
        tx_index,
        log_index,
        pool,
        vec![topics.swap, topic_word(TRADER), topic_word(RECIPIENT)],
        swap_data(amounts[0], amounts[1], amounts[2], amounts[3]),
    )
}

/// An ERC20 `Transfer`. Real, ordinary, and deliberately unclaimed: a token
/// movement says nothing about a pool's reserves.
fn transfer(
    block: u64,
    tx_hash: B256,
    tx_index: u64,
    log_index: u64,
    token: Address,
    from: Address,
    to: Address,
) -> ChainLog {
    log(
        block,
        tx_hash,
        tx_index,
        log_index,
        token,
        vec![
            V2Topics::default().transfer,
            topic_word(from),
            topic_word(to),
        ],
        word(1_000).to_vec(),
    )
}

fn receipt(tx_hash: B256, block: u64, tx_index: u64, logs: Vec<ChainLog>) -> ChainReceipt {
    ChainReceipt {
        tx_hash: TxHash(tx_hash),
        tx_index: TxIndex(tx_index),
        block_number: BlockNumber(block),
        status: true,
        logs,
    }
}

fn transaction(tx_hash: B256, tx_index: u64) -> ChainTransaction {
    ChainTransaction {
        hash: TxHash(tx_hash),
        tx_index: TxIndex(tx_index),
        from: Some(TRADER),
        to: Some(POOL_A),
        value: U256::ZERO,
        input: Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]),
    }
}

/// `transactions` and `receipts` are given in execution order; individual cases
/// shuffle the arrays on purpose to prove the pipeline ignores array order.
fn block_data(number: u64, receipts: Vec<ChainReceipt>, order: Vec<usize>) -> BlockData {
    let transactions: Vec<ChainTransaction> = receipts
        .iter()
        .map(|r| transaction(r.tx_hash.0, r.tx_index.0))
        .collect();
    let receipts = order.into_iter().map(|i| receipts[i].clone()).collect();
    BlockData {
        block: ChainBlock {
            chain_id: CHAIN,
            number: BlockNumber(number),
            hash: B256::left_padding_from(&number.to_be_bytes()),
            parent_hash: B256::left_padding_from(&(number - 1).to_be_bytes()),
            timestamp: 1_790_000_000 + number,
            transaction_count: transactions.len(),
        },
        transactions,
        receipts,
    }
}

fn write_block(dir_name: &str, data: BlockData) {
    let dir = fixtures_root().join("replay").join(dir_name);
    std::fs::create_dir_all(&dir).expect("create fixture directory");
    let path = dir.join(format!("block-{}.json", data.block.number.0));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&data).expect("serialize"),
    )
    .expect("write fixture");
}

#[test]
#[ignore = "regenerates committed fixtures; run deliberately"]
fn regenerate_synthetic_fixtures() {
    // 1. A pool that swaps but never syncs: no reserves may appear.
    write_block(
        "swap_only",
        block_data(
            200,
            vec![receipt(
                TX_SOLO,
                200,
                0,
                vec![
                    // The two token movements of a swap. Neither states reserves.
                    transfer(200, TX_SOLO, 0, 9, TOKEN_1, TRADER, POOL_A),
                    transfer(200, TX_SOLO, 0, 10, TOKEN_0, POOL_A, RECIPIENT),
                    swap(200, TX_SOLO, 0, 11, POOL_A, [100, 0, 0, 90]),
                ],
            )],
            vec![0],
        ),
    );

    // 2. One authoritative sync.
    write_block(
        "single_sync",
        block_data(
            210,
            vec![receipt(
                TX_SOLO,
                210,
                0,
                vec![sync(210, TX_SOLO, 0, 5, POOL_A, 100, 200)],
            )],
            vec![0],
        ),
    );

    // 3. State evolves block by block; the later sync is the current state.
    write_block(
        "multiple_sync",
        block_data(
            220,
            vec![receipt(
                TX_ONE,
                220,
                0,
                vec![sync(220, TX_ONE, 0, 5, POOL_A, 100, 200)],
            )],
            vec![0],
        ),
    );
    write_block(
        "multiple_sync",
        block_data(
            221,
            vec![receipt(
                TX_TWO,
                221,
                0,
                vec![sync(221, TX_TWO, 0, 3, POOL_A, 120, 180)],
            )],
            vec![0],
        ),
    );

    // 4. Three syncs for pool A and one for pool B inside one block, with the
    //    receipts deliberately stored out of execution order. The log indexes
    //    mirror the real block: 150, 174 and 181 are pool 0x3978e57b..'s three
    //    syncs in block 37257255.
    let receipts = vec![
        receipt(
            TX_ONE,
            230,
            0,
            vec![sync(230, TX_ONE, 0, 150, POOL_A, 11, 22)],
        ),
        receipt(
            TX_TWO,
            230,
            1,
            vec![sync(230, TX_TWO, 1, 160, POOL_B, 33, 44)],
        ),
        receipt(
            TX_THREE,
            230,
            2,
            vec![sync(230, TX_THREE, 2, 174, POOL_A, 55, 66)],
        ),
        receipt(
            TX_FOUR,
            230,
            3,
            vec![
                swap(230, TX_FOUR, 3, 180, POOL_A, [1, 0, 0, 1]),
                sync(230, TX_FOUR, 3, 181, POOL_A, 77, 88),
            ],
        ),
    ];
    write_block(
        "same_block_ordering",
        block_data(230, receipts, vec![3, 1, 0, 2]),
    );

    // 5a. Empty reserves: rejected, and never applied.
    write_block(
        "invalid_reserves",
        block_data(
            240,
            vec![receipt(
                TX_ONE,
                240,
                0,
                vec![
                    sync(240, TX_ONE, 0, 5, POOL_A, 100, 200),
                    sync(240, TX_ONE, 0, 6, POOL_A, 0, 0),
                ],
            )],
            vec![0],
        ),
    );

    // 5b. A log carrying the Sync topic but the wrong body.
    write_block(
        "malformed_log",
        block_data(
            250,
            vec![receipt(
                TX_SOLO,
                250,
                0,
                vec![log(
                    250,
                    TX_SOLO,
                    0,
                    4,
                    POOL_A,
                    vec![V2Topics::default().sync],
                    word(1_000).to_vec(),
                )],
            )],
            vec![0],
        ),
    );

    // 5c. An address nobody attested emits a well-formed Sync.
    write_block(
        "unattested_emitter",
        block_data(
            260,
            vec![receipt(
                TX_SOLO,
                260,
                0,
                vec![
                    sync(260, TX_SOLO, 0, 5, UNATTESTED, 1_000, 2_000),
                    sync(260, TX_SOLO, 0, 6, POOL_A, 1, 2),
                ],
            )],
            vec![0],
        ),
    );
}

/// Chain 91342, block 37257255: the block used by the real data acceptance
/// test. Pool 0x3978e57b.. syncs three times in it (global log indexes 150,
/// 174, 181), and its last sync is what `getReserves()` reports at that block.
#[tokio::test]
#[ignore = "reads the live RPC; run deliberately to refresh the real fixture"]
async fn capture_real_block() {
    const REAL_BLOCK: u64 = 37257255;
    let adapter = evm_chain::HttpChainAdapter::connect("https://sepolia-rpc.giwa.io")
        .await
        .expect("rpc reachable");
    let data = adapter
        .get_block_data(BlockNumber(REAL_BLOCK))
        .await
        .expect("historical block readable");

    let dir = fixtures_root().join("real");
    std::fs::create_dir_all(&dir).expect("create real fixture directory");
    let path = dir.join(format!("block-{REAL_BLOCK}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&data).expect("serialize"),
    )
    .expect("write real fixture");

    let logs = data.ordered_logs().len();
    println!(
        "{}: {} transactions, {} logs",
        path.display(),
        data.transactions.len(),
        logs
    );
}
