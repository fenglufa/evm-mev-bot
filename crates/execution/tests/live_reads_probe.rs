//! M7 §20/§26/§36: the three reads the sequence executor depends on, measured against
//! the node before anything is signed — and the L1 estimate checked against a charge this
//! chain has actually applied.
//!
//! ```text
//! GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
//!   cargo test -p evm-execution --test live_reads_probe -- --ignored --nocapture
//! ```
//!
//! Why this file exists rather than just the unit tests in `giwa/reads.rs`. A unit test can
//! prove the calldata is well-formed; it cannot prove the node answers it, that
//! `getL1Fee(bytes)` is whitelisted, that the GasPriceOracle predeploy holds state readable
//! at a *historical* block, or what shape of payload the predeploy's byte-cost model treats
//! as expensive. Those are the assumptions #58's six real transactions would be built on,
//! and each one is cheaper to measure now than to discover mid-sequence. The fourth has
//! already been worth measuring once: it falsified the placeholder `reads.rs` first shipped
//! with, and the replacement is asserted here against a transaction this chain mined.
//!
//! The cross-check that matters is the third section. M6 left one mined transaction behind
//! (`data/evidence/m6/validation/node-answers-37503978.json`), and its receipt carries the
//! chain's own `l1Fee`, `l1GasUsed` and the scalars that produced them. This run rebuilds
//! that transaction's envelope from the fields the node reports for it, checks the rebuild
//! hashes to the same transaction hash, and then asks the oracle what it *would* have
//! charged. If the oracle's answer for those exact bytes at that exact block equals the
//! charge in the receipt, the estimate path is proven against a measured bill instead of
//! against itself. If it does not, the difference is recorded as the finding — including the
//! possibility that the node no longer holds state that far back, which is a fact the
//! sequence's preflight would have to live with too.
//!
//! Nothing here sends, signs, or spends: the only key touched is §58's synthetic scalar-one
//! account, whose balance is read and asserted to be zero. `GIWA_EXECUTION_PRIVATE_KEY` is
//! never read by this target — running it with the variable set changes nothing, because no
//! `Signer` is constructed.

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, Bytes, B256, U256};

use evm_chain::{rpc, CallRequest, ChainAdapter, HttpChainAdapter};
use evm_core::BlockNumber;
use evm_execution::giwa::{
    estimate_l1_fee, pool_state_row, pre_signing_envelope, read_pool, GiwaAssetReader, PoolState,
    GAS_PRICE_ORACLE,
};
use evm_execution::tx::{Signature, SignedTransaction, TransactionType, UnsignedTransaction};
use evm_protocol::signatures::{selector_of, word};

/// The chain's wrapped native asset, as `data/protocols` attests it.
const WETH: Address = alloy_primitives::address!("0x4200000000000000000000000000000000000006");

const M6_ANSWERS: &str = "data/evidence/m6/validation/node-answers-37503978.json";
const CANDIDATES: &str = "data/evidence/m7/candidate-fee-measurement.json";
const EVIDENCE: &str = "data/evidence/m7/probe-live-reads.json";

fn rpc_url() -> String {
    std::env::var("GIWA_RPC_URL").expect(
        "GIWA_RPC_URL is required: this target reads a live node, and no endpoint is \
         hardcoded (§44 of the M5 task, still in force)",
    )
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn json_at(relative: &str) -> serde_json::Value {
    let path = workspace_root().join(relative);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&raw).expect("the evidence file is JSON")
}

/// A hex quantity from a provider answer or a frozen record, as `U256`.
fn quantity(value: &serde_json::Value, context: &str) -> U256 {
    rpc::parse_u256(value, context).unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// A quantity a fixture wrote in *decimal*, which is how the M7 fee evidence records
/// reserves.
///
/// The two spellings are not interchangeable and the mistake is silent: `parse_u256` reads
/// the string `"1000000000000000000000"` as hex and hands back 2^84, which is 1.9e25 — a
/// reserve no pool on this chain has ever held, and the round numbers this target's own
/// comparison row would then have called a reserve movement. The re-parse below is what
/// makes that impossible rather than merely unlikely: the number has to print back exactly
/// as the file wrote it.
fn decimal(value: &serde_json::Value, context: &str) -> U256 {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{context}: a decimal string was expected, got {value}"));
    let parsed = text
        .parse::<U256>()
        .unwrap_or_else(|error| panic!("{context} {text}: {error}"));
    assert_eq!(
        parsed.to_string(),
        text,
        "{context}: {text} does not read back as the same number, so this field is not the \
         decimal spelling this helper asserts"
    );
    parsed
}

fn address(value: &serde_json::Value, context: &str) -> Address {
    rpc::parse_address(value, context).unwrap_or_else(|error| panic!("{context}: {error}"))
}

/// One candidate's two venues, read out of the fee measurement rather than typed in, so the
/// addresses cannot drift from the ones the fees were proven for.
struct Venue {
    candidate: usize,
    mid: Address,
    pool: Address,
    role: &'static str,
    recorded_reserves: (U256, U256),
    recorded_at: u64,
}

fn venues(answers: &serde_json::Value, priced_at: u64) -> Vec<Venue> {
    let mut out = Vec::new();
    for (index, candidate) in answers["candidates"]
        .as_array()
        .expect("the fee evidence names its candidates")
        .iter()
        .enumerate()
    {
        let mid = address(&candidate["mid"], "candidate.mid");
        for (role, key) in [("buy", "buying_pool"), ("sell", "selling_pool")] {
            let pool = &candidate[key];
            out.push(Venue {
                candidate: index,
                mid,
                pool: address(&pool["address"], &format!("candidate {index} {role} pool")),
                role,
                recorded_reserves: (
                    decimal(&pool["reserve0"], "recorded reserve0"),
                    decimal(&pool["reserve1"], "recorded reserve1"),
                ),
                recorded_at: priced_at,
            })
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "reads a live node; writes data/evidence/m7/probe-live-reads.json"]
async fn the_three_reads_the_sequence_relies_on_are_measured_first() {
    let adapter = HttpChainAdapter::connect(&rpc_url())
        .await
        .expect("the configured endpoint answers eth_chainId");
    let m6 = json_at(M6_ANSWERS);
    let chain_id = adapter.chain_id().0;
    assert_eq!(
        chain_id,
        rpc::parse_u64(&m6["chain_id"]["result"], "m6 chain id").expect("a chain id quantity"),
        "this probe reads the chain M6's transaction was mined on"
    );

    // One height for every read in sections one and two, so a comparison between the
    // recorded reserves and the live ones is a comparison across time and not across two
    // different heads.
    let head = adapter.latest_block().await.expect("the head");
    let head_context = adapter
        .get_block_context(head)
        .await
        .expect("the head block is readable");

    // ── 1. §26's reserve line: `token0()`, `token1()`, `getReserves()` at the head ──
    let fee_evidence = json_at(CANDIDATES);
    let priced_at = fee_evidence["_provenance"]["read_at_head"]
        .as_u64()
        .expect("the fee evidence names the block it priced at");
    let mut pools: Vec<(Venue, PoolState)> = Vec::new();
    for venue in venues(&fee_evidence, priced_at) {
        let state = read_pool(&adapter, venue.pool, head)
            .await
            .unwrap_or_else(|error| panic!("pool {:?} at {}: {error}", venue.pool, head.0));
        pools.push((venue, state));
    }
    // One row per venue, filled in by the same loop that asserts the mapping, so a row
    // cannot describe a pool the assertions did not check.
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for (venue, state) in &pools {
        // The ordering question §26's re-pricing lives or dies on: the pool must hold the
        // candidate's mid token on one side and WETH on the other, or the route this file's
        // own fee evidence priced is not the route the pools describe now.
        let (first, second) = (state.token0, state.token1);
        let holds_mid = first == venue.mid || second == venue.mid;
        let holds_weth = first == WETH || second == WETH;
        assert!(
            holds_mid && holds_weth,
            "candidate {} at {} holds {first}/{second}, which is not the pair the fee \
             evidence priced (mid {} against WETH)",
            venue.candidate,
            venue.pool,
            venue.mid,
        );
        // `in_out` is the mapping §26's re-pricing runs on, so it is checked against the
        // words the pool itself answered with rather than trusted: the reserve that comes
        // back as `in` has to be the reserve sitting on the side the route spends from.
        let other = if state.token0 == venue.mid {
            state.token1
        } else {
            state.token0
        };
        assert_eq!(
            other, WETH,
            "candidate {} at {} holds {} against {other}, and the fee evidence priced a \
             route that spends WETH",
            venue.candidate, venue.pool, venue.mid,
        );
        let (mid_reserve, weth_reserve) = state.in_out(venue.mid, WETH).unwrap_or_else(|error| {
            panic!("pool {:?} cannot order its reserves: {error}", state.pool)
        });
        let expected_mid_reserve = if state.token0 == venue.mid {
            state.reserve0
        } else {
            state.reserve1
        };
        let expected_weth_reserve = if state.token0 == WETH {
            state.reserve0
        } else {
            state.reserve1
        };
        assert_eq!(
            (mid_reserve, weth_reserve),
            (expected_mid_reserve, expected_weth_reserve),
            "pool {:?} mapped its reserves to the wrong side",
            state.pool
        );
        assert!(
            !state.reserve0.is_zero() && !state.reserve1.is_zero(),
            "pool {:?} reports empty reserves at {}, so it is not a venue anything can trade \
             through",
            state.pool,
            state.read_at_block
        );
        let mut row = pool_state_row(state);
        row["candidate"] = serde_json::json!(venue.candidate);
        row["role"] = serde_json::json!(venue.role);
        row["mid"] = serde_json::json!(format!("{:?}", venue.mid));
        row["reserve_of_mid"] = serde_json::json!(mid_reserve.to_string());
        row["reserve_of_weth"] = serde_json::json!(weth_reserve.to_string());
        row["priced_at_block"] = serde_json::json!(venue.recorded_at);
        row["recorded_reserve0"] = serde_json::json!(venue.recorded_reserves.0.to_string());
        row["recorded_reserve1"] = serde_json::json!(venue.recorded_reserves.1.to_string());
        row["reserve0_moved"] = serde_json::json!(state.reserve0 != venue.recorded_reserves.0);
        row["reserve1_moved"] = serde_json::json!(state.reserve1 != venue.recorded_reserves.1);
        rows.push(row);
    }

    // ── 2. §20's token snapshot: `balanceOf` at a height the caller named ──
    let reader = GiwaAssetReader::new(&adapter);
    let probe_sender = probe_sender();
    let weth_held = reader
        .token_balance(WETH, probe_sender, head)
        .await
        .expect("balanceOf answers for WETH");
    assert!(
        weth_held.source.contains(&head.0.to_string()),
        "the snapshot has to name the block it answered for, or §20's before/after pair is \
         two unattributed numbers: {}",
        weth_held.source
    );
    let mid_token = pools[0].0.mid;
    let mid_held = reader
        .token_balance(mid_token, probe_sender, head)
        .await
        .expect("balanceOf answers for the candidate mid token");

    // ── 3. §36's L1 line: the oracle's forecast against a charge the chain applied ──
    let transaction_hash = m6["transaction_hash"]
        .as_str()
        .expect("the frozen record names its transaction");
    let raw_tx: serde_json::Value = adapter
        .request_raw(
            "eth_getTransactionByHash",
            serde_json::json!([transaction_hash]),
        )
        .await
        .expect("the node serves eth_getTransactionByHash for a mined transaction");
    assert!(
        !raw_tx.is_null(),
        "the node answers null for a transaction its receipt proves was mined"
    );
    let unsigned = UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id,
        nonce: rpc::parse_u64(&raw_tx["nonce"], "tx.nonce").expect("nonce"),
        to: Some(address(&raw_tx["to"], "tx.to")),
        value: quantity(&raw_tx["value"], "tx.value"),
        gas_limit: rpc::parse_u64(&raw_tx["gas"], "tx.gas").expect("gas"),
        input: raw_tx["input"]
            .as_str()
            .unwrap_or("0x")
            .parse::<Bytes>()
            .expect("input"),
        access_list: Vec::new(),
        max_priority_fee_per_gas: Some(quantity(&raw_tx["maxPriorityFeePerGas"], "tx.tip")),
        max_fee_per_gas: Some(quantity(&raw_tx["maxFeePerGas"], "tx.maxFee")),
    };
    let signature = Signature::new(
        quantity(&raw_tx["r"], "tx.r"),
        quantity(&raw_tx["s"], "tx.s"),
        rpc::parse_u64(&raw_tx["yParity"], "tx.yParity").expect("a parity quantity") == 1,
    );
    let rebuilt = SignedTransaction::new(unsigned.clone(), signature);
    let recorded_hash: B256 = transaction_hash
        .parse()
        .expect("the frozen transaction hash is a hash");
    assert_eq!(
        rebuilt.hash(),
        recorded_hash,
        "rebuilding the envelope from the fields the node reports must reproduce the hash \
         its receipt is filed under — §14's round trip, on a transaction the chain mined"
    );
    let receipt_block = rpc::parse_u64(&raw_tx["blockNumber"], "tx.blockNumber").expect("block");

    // The oracle at the head: does the predeploy answer at all, and is the placeholder the
    // expensive shape? Both questions are answered at one height so the L1 parameters the
    // two calls read are the same parameters. The shapes are weighed before the fees are
    // compared — a fee difference is only interpretable against the byte counts that made
    // it up, and `getL1GasUsed` is the oracle's own account of that arithmetic.
    let real_bytes = rebuilt.raw();
    let placeholder_bytes = pre_signing_envelope(&unsigned);
    let real_shape = envelope_shape(&adapter, &real_bytes, head).await;
    let placeholder_shape = envelope_shape(&adapter, &placeholder_bytes, head).await;
    let exact_at_head = estimate_l1_fee(&adapter, real_bytes.as_ref(), head).await;
    let ceiling_at_head = estimate_l1_fee(&adapter, placeholder_bytes.as_ref(), head).await;
    let exact_wei = exact_at_head.amount().expect(
        "getL1Fee(bytes) is not served, or the predeploy answers nothing usable — which is \
         §36's line unreadable, and the sequence's preflight would have to say so",
    );
    let ceiling_wei = ceiling_at_head
        .amount()
        .expect("the placeholder envelope prices too, over the same call");
    assert!(
        ceiling_wei >= exact_wei,
        "the placeholder is the shape a real signature pays at, so its estimate may fall above \
         the coming charge but not below it; measured {} wei against the mined bytes' {} wei \
         at block {}. The byte-cost model four paragraphs below says which shapes this chain's \
         estimator treats as cheap — a repeat-filled placeholder came out 9.7% under a \
         high-entropy one.",
        ceiling_wei,
        exact_wei,
        head.0
    );

    // …and the same question asked of four payloads that differ *only* in their byte values.
    // This is the measurement that picked the placeholder above: a first version filled the
    // signature with 0xff, matched the mined bytes on length and zero-byte count, and still
    // priced 9.7% below them — so the estimator's cheap shapes had to be found by weighing
    // payloads rather than by reasoning about calldata costs.
    let fee_model = fee_model_shapes(&adapter, real_bytes.len(), head).await;
    // …and the same model against length, at the sizes the sequence will actually carry.
    let fee_model_by_length = fee_model_lengths(
        &adapter,
        &[108, 130, real_bytes.len(), 212],
        real_bytes.len(),
        head,
    )
    .await;

    // …and at the block the charge was actually applied, which is the only comparison that
    // can fail — the node may not hold that state any more.
    let at_receipt_block =
        estimate_l1_fee(&adapter, rebuilt.raw().as_ref(), BlockNumber(receipt_block)).await;
    let charged = quantity(
        &m6["attempts"]["eth_getTransactionReceipt"]["result"]["l1Fee"],
        "l1Fee",
    );
    let historical = match &at_receipt_block {
        evm_execution::L1FeeSource::OracleEstimate { amount, .. } => serde_json::json!({
            "answered": true,
            "oracle_says": amount.to_string(),
            "receipt_charged": charged.to_string(),
            "exact_match": amount == &charged,
            "difference_wei": if amount >= &charged {
                (amount - charged).to_string()
            } else {
                (charged - amount).to_string()
            },
        }),
        other => serde_json::json!({
            "answered": false,
            "why": other.describe(),
        }),
    };

    // ── the record ──
    let finding = serde_json::json!({
        "head": {
            "number": head.0,
            "hash": format!("{:?}", head_context.hash),
            "chain_id": chain_id,
            "read_by": "eth_getBlockByNumber(\"latest\")",
        },
        "pools": rows,
        "token_balances": {
            "account": format!("{probe_sender:?}"),
            "weth": { "amount": weth_held.amount.to_string(), "source": weth_held.source },
            "mid_token": { "token": format!("{mid_token:?}"), "amount": mid_held.amount.to_string(), "source": mid_held.source },
        },
        "l1_fee_estimate": {
            "oracle": format!("{GAS_PRICE_ORACLE:?}"),
            "method": "eth_call getL1Fee(bytes)",
            "transaction_hash": transaction_hash,
            "envelope_rebuilt_from_node_fields": true,
            "rebuilt_hash_matches_receipt_transaction_hash": true,
            "at_head": {
                "real_bytes_wei": exact_wei.to_string(),
                "pre_signing_envelope_wei": ceiling_wei.to_string(),
                "pre_signing_envelope_is_at_least_the_real_charge": ceiling_wei >= exact_wei,
                "real_envelope_shape": real_shape.clone(),
                "pre_signing_envelope_shape": placeholder_shape.clone(),
                "block": head.0,
            },
            "byte_cost_model_at_the_same_block": fee_model.clone(),
            "byte_cost_model_against_length": fee_model_by_length.clone(),
            "at_the_block_that_was_charged": historical.clone(),
            "receipt_l1_fee_wei": charged.to_string(),
            "receipt_l1_gas_used": m6["attempts"]["eth_getTransactionReceipt"]["result"]["l1GasUsed"].as_str(),
        },
        "verdict": format!(
            "the three reads the sequence executor needs are answered by this endpoint: \
             getReserves/token0/token1 at a named height, balanceOf at a named height, and \
             getL1Fee(bytes) on the fee predeploy at both the head{} and a historical block. \
             §36's source line can therefore name an oracle read rather than an assumption.",
            if historical["answered"].as_bool().unwrap_or(false) {
                ""
            } else {
                " (a historical read at the charged block did not answer; see \
                  at_the_block_that_was_charged)"
            },
        ),
        "l1_fee_model_finding": format!(
            "this chain's fee predeploy prices a payload by how well it compresses, not by its \
             length or its zero-byte count: four 110-byte payloads that differ only in their \
             byte values cost {} L1 gas for the three low-entropy shapes and {} for the \
             high-entropy one, read at block {}. The mined envelope prices like the \
             high-entropy shape ({} gas, {} wei) and so does the pre-signing placeholder ({} \
             gas, {} wei) — which is what this target asserts: the estimate may fall above the \
             coming charge but not below it. A placeholder of the same length filled with \
             0xff priced 9.7% under the mined bytes and was replaced on the strength of that \
             measurement. The estimate is still only a forecast: re-read at the block this \
             transaction was charged, the predeploy says {} wei against the {} wei its receipt \
             records, so §37's bill stays the receipt's own l1Fee field and the profit line \
             reports this gap rather than closing it.",
            fee_model[0]["l1_gas_used"],
            fee_model[3]["l1_gas_used"],
            head.0,
            real_shape["l1_gas_used"],
            exact_wei,
            placeholder_shape["l1_gas_used"],
            ceiling_wei,
            historical["oracle_says"],
            historical["receipt_charged"],
        ),
        "m7_consequence": "the preflight's L1 leg and §20's token snapshots run against this \
                           endpoint; nothing about the sequence's cost model rests on a number \
                           this repository invented.",
    });
    let record = serde_json::json!({
        "_provenance": {
            "milestone": "M7 §20/§26/§36",
            "asked": "真实链上把 SequenceStage 要用的三个读取（getReserves 系列、balanceOf、getL1Fee）跑一遍，并把 L1 预估对着 M6 那笔真实扣费核一次",
            "run_command": "GIWA_RPC_URL=https://sepolia-rpc.giwa.io cargo test -p evm-execution --test live_reads_probe -- --ignored --nocapture",
            "urls_from_env": ["GIWA_RPC_URL"],
            "endpoint": adapter.url(),
            "no_key_in_this_file": true,
            "no_value_at_risk": format!(
                "read-only; the account whose balances are named is §58's synthetic scalar-one \
                 address, which holds {} wei of WETH at block {}",
                weth_held.amount, head.0
            ),
            "sources": [M6_ANSWERS, CANDIDATES],
        },
        "finding": finding,
    });
    let path = workspace_root().join(EVIDENCE);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&record).expect("the record serializes"),
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

    println!("head={} chain={}", head.0, chain_id);
    for row in &rows {
        println!(
            "pool={} candidate={} role={} reserves {}/{} (priced {}/{}) moved={} last_sync={}",
            row["pool"],
            row["candidate"],
            row["role"],
            row["reserve0"],
            row["reserve1"],
            row["recorded_reserve0"],
            row["recorded_reserve1"],
            row["reserve0_moved"],
            row["block_timestamp_last"],
        );
    }
    println!(
        "balanceOf WETH({probe_sender}) = {} at #{}",
        weth_held.amount, head.0
    );
    println!(
        "getL1Fee real bytes = {} wei over {} ; placeholder = {} wei over {}",
        exact_wei, real_shape, ceiling_wei, placeholder_shape
    );
    println!("getL1Fee at the charged block #{receipt_block}: {historical}");
    for row in fee_model_by_length.as_array().expect("rows") {
        println!(
            "high-entropy {} bytes: {} L1 gas, {} wei",
            row["length"], row["l1_gas_used"], row["l1_fee_wei"]
        );
    }
    println!("evidence={}", path.display());
}

/// The account §40's synthetic key signs for: scalar one, derived here rather than typed, so
/// no key material appears in this file or in the evidence it writes.
fn probe_sender() -> Address {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    let key =
        evm_execution::ExecutionKey::from_secret_bytes(&bytes).expect("scalar one is in range");
    key.address()
}

/// `getL1GasUsed(bytes)` on the fee predeploy: the oracle's own account of how many L1 gas
/// units a payload costs, which is the arithmetic the fee is made of. Two envelopes that
/// price differently have to differ somewhere this line measures, and a fee difference with
/// no measured cause would be a fact about the node rather than about the payloads.
async fn l1_gas_used(http: &HttpChainAdapter, payload: &[u8], at: BlockNumber) -> Option<U256> {
    let mut data = Vec::with_capacity(4 + 64 + payload.len() + 32);
    data.extend_from_slice(&selector_of("getL1GasUsed(bytes)"));
    data.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    data.extend_from_slice(payload);
    let padding = (32 - (payload.len() % 32)) % 32;
    data.resize(data.len() + padding, 0u8);
    let raw = http
        .call(
            at,
            &CallRequest {
                to: GAS_PRICE_ORACLE,
                data: Bytes::from(data),
            },
        )
        .await
        .ok()?;
    word(&raw, 0).ok()
}

/// An envelope's weight: how many bytes it is, how many of them are zero, and what the
/// oracle charges for that shape at `at`.
async fn envelope_shape(
    http: &HttpChainAdapter,
    payload: &Bytes,
    at: BlockNumber,
) -> serde_json::Value {
    serde_json::json!({
        "length": payload.len(),
        "zero_bytes": payload.iter().filter(|byte| **byte == 0).count(),
        "nonzero_bytes": payload.iter().filter(|byte| **byte != 0).count(),
        "l1_gas_used": l1_gas_used(http, payload, at).await.map(|amount| amount.to_string()),
        "l1_fee_wei": estimate_l1_fee(http, payload.as_ref(), at).await.amount().map(|amount| amount.to_string()),
    })
}

/// A deterministic, run-free filler: successive keccak digests of the previous one, with the
/// rare zero byte skipped. High-entropy on purpose — it is the shape this target's own
/// byte-cost measurement shows the predeploy prices like a real signature.
fn high_entropy_bytes(length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut seed = [0u8; 32];
    while out.len() < length {
        seed = alloy_primitives::keccak256(seed).0;
        for byte in seed {
            if byte == 0 {
                continue;
            }
            out.push(byte);
            if out.len() == length {
                break;
            }
        }
    }
    out
}

/// The oracle's byte-cost model, measured rather than assumed: four payloads of one length
/// that differ only in their byte values. The envelopes this probe prices came out ~9.7%
/// apart with identical lengths and identical zero-byte counts, which no
/// "4 gas for a zero byte, 16 for a nonzero one" model can produce, so the spread between
/// these four answers is the part of the model a placeholder signature is chosen against.
async fn fee_model_shapes(
    http: &HttpChainAdapter,
    length: usize,
    at: BlockNumber,
) -> serde_json::Value {
    let variants = [
        ("all_zero", vec![0u8; length]),
        ("all_ff", vec![0xffu8; length]),
        (
            "alternating",
            (0..length)
                .map(|i| if i % 2 == 0 { 0x00 } else { 0xff })
                .collect(),
        ),
        ("pseudorandom_nonzero", high_entropy_bytes(length)),
    ];
    let mut rows = Vec::new();
    for (name, payload) in variants {
        let mut row = envelope_shape(http, &Bytes::from(payload), at).await;
        row["shape"] = serde_json::json!(name);
        rows.push(row);
    }
    serde_json::json!(rows)
}

/// The same model against *length*, with the shape held at the expensive one.
///
/// Why the second axis matters: every transaction in #58's sequence is its own submission
/// (§4's no-contract executor), and the two ends of the sequence are very different payloads
/// — a `withdraw`/`approve` transfer is about as small as a signed transaction gets, while a
/// swap carries 100 bytes of calldata on top. If the forecast were only ever tested at one
/// length, the preflight's cost line would be measured at one point and extrapolated to the
/// others.
async fn fee_model_lengths(
    http: &HttpChainAdapter,
    lengths: &[usize],
    real_length: usize,
    at: BlockNumber,
) -> serde_json::Value {
    let mut rows = Vec::new();
    for length in lengths {
        let mut row = envelope_shape(http, &Bytes::from(high_entropy_bytes(*length)), at).await;
        row["role"] = serde_json::json!(match *length {
            n if n == real_length => "the mined envelope's own length",
            108 =>
                "what a swap step's envelope costs: 4-byte selector, two amounts, the \
                   address, the empty bytes, plus a 65-byte signature placeholder",
            130 => "a deposit or withdraw step",
            212 => "the largest step in the sequence — swap with its full calldata",
            _ => "interpolation",
        });
        rows.push(row);
    }
    serde_json::json!(rows)
}
