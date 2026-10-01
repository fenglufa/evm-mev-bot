//! The same §41 arrow, pulled off the live node by the code that was fixed afterwards.
//!
//! `real_validation_receipt.rs` proves the receipt tracker's binding and cost reading on
//! the answers the endpoint gave during the §35 run, replayed from
//! `data/evidence/m6/validation/node-answers-37503978.json`. A replay can only say what
//! those saved answers mean. It cannot say that the tracker as it now stands would have
//! reached `Included` when asked of the node today — and the live run of this transaction
//! ended `failed`, because the tracker that ran it called an unreadable block
//! [`TrackedReceipt::Unbound`].
//!
//! So this file asks the running question, and it is a read-only one:
//!
//! ```text
//! cargo test -p evm-execution --test real_validation_live -- --ignored --nocapture
//! ```
//!
//! * The endpoint URL comes out of the committed evidence file, never typed here (§5's
//!   "configure, don't bake in"), and the chain id both this file and `connect` compare
//!   comes from the same document.
//! * The adapter is assembled in [`ExecutionMode::SignOnly`], the mode that cannot hand
//!   bytes to a node (§20). Nothing in this file can put a transaction on the network, so
//!   re-running it costs nothing and cannot spend a nonce — the transaction it asks about
//!   was mined in block 37 503 978 and is already on the chain.
//! * Every assertion compares the live answer with the frozen one. A drift is a fact about
//!   the endpoint and is reported as such rather than smoothed over. The output of the run
//!   that first turned this green is kept beside those answers, in
//!   `data/evidence/m6/validation/live-retrack-37503978.txt`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use serde_json::Value;

use evm_execution::{
    ChainReader, EndpointKind, ExecutionMode, ExpectedTransaction, GiwaSequencerDirect,
    NonceSource, Receipt, ReceiptPolicy, ReceiptStatus, ReceiptTracker, TrackedReceipt,
    TransactionSubmitter,
};

/// The session §35 ran in submit mode; its signed row is the expected side of §27.
const SESSION: &str = "data/evidence/m6/validation/validate-91342-1790849090542";
const NODE_ANSWERS: &str = "data/evidence/m6/validation/node-answers-37503978.json";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/execution sits two levels below the workspace root")
        .to_path_buf()
}

fn json_file(relative: &str) -> Value {
    let path = workspace_root().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn single_row(file: &str) -> Value {
    let path = workspace_root().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let row = lines
        .next()
        .unwrap_or_else(|| panic!("{file}: the session wrote no row"));
    assert!(
        lines.next().is_none(),
        "{file}: this evidence is about one transaction, so a second row would mean the \
         session ran the ladder twice"
    );
    serde_json::from_str(row).unwrap_or_else(|error| panic!("{file}: {error}"))
}

/// The quantity the endpoint itself wrote, in the `0x…` form it used.
fn hex_u64(value: &Value, context: &str) -> u64 {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a hex quantity"));
    u64::from_str_radix(text.trim_start_matches("0x"), 16)
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn hex_u128(value: &Value, context: &str) -> U256 {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: expected a hex quantity"));
    U256::from_str_radix(text.trim_start_matches("0x"), 16)
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// The endpoint this transaction was sent to, and the chain id it answered with — both as
/// the run recorded them.
fn endpoint_record() -> (String, u64) {
    let node = json_file(NODE_ANSWERS);
    let url = node["endpoint"]
        .as_str()
        .expect("the evidence file names the endpoint it read");
    assert!(
        url.starts_with("https://"),
        "the recorded endpoint is an https URL: {url}"
    );
    (
        url.to_string(),
        hex_u64(&node["chain_id"]["result"], "chain_id"),
    )
}

/// §27's expected side, from the lane's own §52 row.
fn expected() -> ExpectedTransaction {
    let signed = single_row(&format!("{SESSION}/signed-transactions.jsonl"));
    ExpectedTransaction {
        transaction_hash: signed["signed_tx_hash"]
            .as_str()
            .and_then(|text| text.parse::<B256>().ok())
            .expect("the signed row carries the hash computed over the bytes"),
        sender: signed["recovered_sender"]
            .as_str()
            .and_then(|text| text.parse::<Address>().ok())
            .expect("the signed row carries the sender the signature recovered to"),
        target: Some(
            signed["to"]
                .as_str()
                .and_then(|text| text.parse::<Address>().ok())
                .expect("the signed row carries the target"),
        ),
        nonce: signed["nonce"]
            .as_u64()
            .expect("the signed row carries a nonce"),
        chain_id: signed["chain_id"].as_u64().expect("and its chain id"),
    }
}

/// The live endpoint, in the one mode that cannot send.
async fn connect() -> GiwaSequencerDirect {
    let (url, chain_id) = endpoint_record();
    let adapter = GiwaSequencerDirect::connect(
        &url,
        chain_id,
        ExecutionMode::SignOnly,
        EndpointKind::PublicHttpRpc,
    )
    .await
    .expect("the recorded endpoint answers and agrees about which chain it is");
    assert!(
        !adapter.mode().may_submit(),
        "this read ran under a mode that cannot broadcast, so it cannot spend a nonce: {}",
        adapter.mode().name()
    );
    let answered = <GiwaSequencerDirect as ChainReader>::endpoint_chain_id(&adapter)
        .await
        .expect("a chain id");
    assert_eq!(
        answered, chain_id,
        "§6's third number: what this node answers for, read now rather than remembered"
    );
    adapter
}

/// The transaction's own hash, as the §52 row and the saved receipt both name it.
fn transaction_hash() -> B256 {
    expected().transaction_hash
}

async fn live_receipt(adapter: &GiwaSequencerDirect) -> Option<Receipt> {
    adapter
        .receipt(transaction_hash())
        .await
        .expect("the endpoint answers eth_getTransactionReceipt")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live endpoint; the four gates must not depend on a node being up"]
async fn the_fixed_tracker_reads_the_same_inclusion_the_saved_answers_describe() {
    let adapter = connect().await;
    let node = json_file(NODE_ANSWERS);
    let saved = &node["attempts"]["eth_getTransactionReceipt"]["result"];
    assert!(
        !saved.is_null(),
        "the evidence file stores the answer itself"
    );

    let live = live_receipt(&adapter)
        .await
        .expect("the transaction the endpoint mined is still named by its hash");
    let expected = expected();
    // §26's fields, one by one: the node's answer today is the answer it gave then.
    assert_eq!(
        live.transaction_hash,
        transaction_hash(),
        "the receipt is for the hash asked for"
    );
    assert_eq!(
        live.block_number,
        hex_u64(&saved["blockNumber"], "saved blockNumber"),
        "the block the receipt names has not moved"
    );
    assert_eq!(
        format!("{:#x}", live.block_hash),
        saved["blockHash"].as_str().expect("a saved block hash"),
        "and it is still the same block"
    );
    assert_eq!(
        live.transaction_index,
        hex_u64(&saved["transactionIndex"], "saved transactionIndex")
    );
    assert_eq!(
        live.gas_used,
        hex_u64(&saved["gasUsed"], "saved gasUsed"),
        "the chain charged the same gas it charged during the run"
    );
    assert_eq!(
        live.effective_gas_price,
        hex_u128(&saved["effectiveGasPrice"], "saved effectiveGasPrice")
    );
    assert_eq!(
        live.l1_fee,
        Some(hex_u128(&saved["l1Fee"], "saved l1Fee")),
        "§11's L1 data fee is still itemised separately from the L2 bill"
    );
    assert_eq!(
        live.from, expected.sender,
        "the receipt's sender is the account the signature recovered to"
    );
    assert_eq!(live.to, Some(expected.sender));
    assert!(
        live.success,
        "a mined transfer either paid or said it did not (§P)"
    );
    assert_eq!(live.outcome(), ReceiptStatus::Included);

    // The second §27 leg, live: the endpoint's own block at that height.
    let hash = adapter
        .block_hash_at(evm_core::BlockNumber(live.block_number))
        .await
        .expect("the block read answers")
        .expect("the block the receipt names is readable");
    assert_eq!(
        hash, live.block_hash,
        "so this receipt is about a canonical block"
    );

    // And the whole thing through the tracker that was wrong during the live run: receipt
    // read, binding, block verification, in one call, against the node.
    let tracker = ReceiptTracker::new(ReceiptPolicy {
        attempts: 4,
        between_attempts: Duration::from_millis(250),
    });
    let reader = adapter.clone();
    let verifier = adapter.clone();
    let outcome = tracker
        .track(
            &expected,
            move || {
                let reader = reader.clone();
                async move {
                    reader
                        .receipt(transaction_hash())
                        .await
                        .map_err(|error| error.to_string())
                }
            },
            move |number| {
                let verifier = verifier.clone();
                async move {
                    verifier
                        .block_hash_at(evm_core::BlockNumber(number))
                        .await
                        .map_err(|error| error.to_string())
                }
            },
        )
        .await;
    match &outcome {
        TrackedReceipt::Included(receipt) => assert_eq!(
            receipt.l2_cost_wei(),
            Some(U256::from(
                node["cost"]["l2_gas_bill_wei"]
                    .as_str()
                    .expect("the cost section is part of the evidence")
                    .parse::<u128>()
                    .expect("a wei figure")
            )),
            "the live bill is the bill the frozen evidence itemises"
        ),
        other => panic!(
            "the tracker that ended the live run as `failed` must now reach `Included`: \
             {other:?}"
        ),
    }
    println!(
        "live re-track: tx {:#x} in block {} ({}), status {:?}, gas_used {}, \
         l2 bill {} wei, l1 fee {} wei — endpoint {}",
        transaction_hash(),
        live.block_number,
        live.block_hash,
        outcome.status(),
        live.gas_used,
        live.l2_cost_wei().unwrap_or(U256::ZERO),
        live.l1_fee.unwrap_or(U256::ZERO),
        adapter.url(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live endpoint; the four gates must not depend on a node being up"]
async fn the_wallet_holds_what_the_frozen_cost_section_says_it_should() {
    // The one number the run could not predict: §33's ceiling is `gas_limit × max_fee +
    // value`, which carries the L2 half of an OP-stack bill and not the L1 half. The
    // balance the endpoint reports now has to sit at `before − total`, because nothing else
    // has spent from this account since. That is the difference between the ceiling as a
    // promise and the fee as it was actually taken — and the nonce is how the same read
    // shows the account ran exactly the one transaction.
    let adapter = connect().await;
    let node = json_file(NODE_ANSWERS);
    let cost = &node["cost"];
    let before = cost["balance_before_wei"]
        .as_str()
        .expect("the cost section names the balance before")
        .parse::<u128>()
        .expect("a wei figure");
    let spent = cost["total_wei"]
        .as_str()
        .expect("and the total spent")
        .parse::<u128>()
        .expect("a wei figure");
    let recorded_after = cost["balance_after_wei"]
        .as_str()
        .expect("and the balance after")
        .parse::<u128>()
        .expect("a wei figure");

    let (head, head_hash) = adapter
        .head()
        .await
        .expect("the endpoint answers for its own head");
    let live_after = adapter
        .native_balance(expected().sender, head)
        .await
        .expect("and for a balance pinned at that head");
    assert_eq!(
        live_after,
        U256::from(recorded_after),
        "the account is untouched since the run: {} wei at block {} ({}), and the run's own \
         arithmetic says it should hold {}",
        live_after,
        head.0,
        head_hash,
        recorded_after
    );
    // The other half of "this transaction was the only thing the chain ran for this
    // account": the nonce the signed row carried is one below the nonce the chain now
    // confirms.
    let reading = <GiwaSequencerDirect as NonceSource>::nonce(&adapter, expected().sender)
        .await
        .expect("the nonce read answers");
    let sent_nonce = expected().nonce;
    assert_eq!(
        reading.confirmed,
        sent_nonce + 1,
        "§11's account view after exactly one executed transaction: the intent carried \
         nonce {sent_nonce} and the chain confirms {}",
        reading.confirmed
    );
    assert_eq!(
        reading.pending, reading.confirmed,
        "and nothing is in the pool for this account, so the §25 lane has really drained"
    );
    assert_eq!(
        before - spent,
        recorded_after,
        "the wallet's own arithmetic, checked against the live reading"
    );
}
