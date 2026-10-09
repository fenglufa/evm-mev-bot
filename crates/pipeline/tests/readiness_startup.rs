//! M12-B §3's gate, measured at the boundary a real run crosses: the pipeline's own
//! bootstrap, against an endpoint that answers JSON-RPC over TCP.
//!
//! The chain crate's `readiness_gate.rs` proves the decoder and the budget against
//! hand-written responses. This file proves the two things only a pipeline test can
//! prove — *where* the ask happens and *how many times*:
//!
//! * §3's 「在启动阶段执行一次」 and 「禁止进入会产生真实执行动作的阶段」: the ask sits
//!   in `build_canonical`, ahead of the registry read, the execution lane's connect,
//!   the evidence writer, and the first block. A held run therefore has no session
//!   directory and its endpoint sees no `eth_getBlockByNumber` at all — that ordering
//!   is the proof, not an assertion about intent.
//! * §3's 「不要在每次行情事件上调用」: a run that passes and works through several
//!   canonical blocks still asks once. The count comes off the endpoint's own tally of
//!   what arrived, which is the only place a per-event call could not hide.
//!
//! The market data is real: blocks come from `fixtures/live-m5/arbitrage-window` (a
//! recording of GIWA Testnet around the block where the WETH/TTAX pair restated both
//! reserves) and account state from `fixtures/simulation-m4/dump-37191169.json`, so the
//! passing run is doing the same work §32's replay acceptance does, only reached over
//! HTTP. Nothing here opens a real endpoint: the only address bound is 127.0.0.1 on an
//! ephemeral port owned by this process, and no submission method is ever answered.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use evm_execution::{ExecutionMode, ExecutionSetup};
use evm_pipeline::config::{CanonicalSource, PipelineConfig};
use evm_pipeline::{run, PipelineError};

const CHAIN_HEX: &str = "0x164ce";
/// A chain id nothing in this corpus attests, so a stub that answers it is §9's D1 shape:
/// the candidate endpoint disagreeing with the chain the run already settled on.
const OTHER_CHAIN_HEX: &str = "0x02";
const HEAD: u64 = 37_191_199;
/// The block both attested pools of one pair moved together, and the height the state
/// recording was taken at.
const STATE_BLOCK: u64 = 37_191_169;

/// What the stub answers `eth_syncing` with. Three of §3's shapes are enough here: the
/// chain crate's file covers every decoder branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sync {
    /// The node says it is not syncing — the pass case.
    Idle,
    /// A progress object — the node is behind, so the run is held.
    Behind,
    /// A JSON-RPC error: no answer, which is never readiness.
    Rejected,
}

/// A stub node on an ephemeral localhost port, with the endpoint's own ordered tally of
/// what arrived.
struct Stub {
    url: String,
    received: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn spawn(sync: Sync) -> Self {
        Self::spawn_at(sync, CHAIN_HEX)
    }

    /// [`Stub::spawn`], with the chain id the stub answers `eth_chainId` with. Only a
    /// candidate endpoint needs a disagreeing one: §9's D1 is about which side of the
    /// refusal names which endpoint.
    fn spawn_at(sync: Sync, chain: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("an address").port();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let received = received.clone();
            move || {
                // The listener moves into this thread and lives as long as it does, which
                // is as long as the test process: every stub in this file is answered by
                // exactly one process, and none of them outlives the test that bound it.
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let received = received.clone();
                    std::thread::spawn(move || serve(stream, sync, chain, received));
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            received,
        }
    }

    /// The methods the endpoint saw, in arrival order.
    fn received(&self) -> Vec<String> {
        self.received.lock().expect("the tally").clone()
    }

    fn count(&self, method: &str) -> usize {
        self.received()
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus() -> PathBuf {
    workspace_root().join("fixtures/live-m5/arbitrage-window")
}

fn registries() -> Vec<PathBuf> {
    vec![
        workspace_root().join("data/protocols"),
        workspace_root().join("data/protocols-m3"),
    ]
}

/// The recorded state at `STATE_BLOCK`, as `address -> slot -> decimal value`.
fn state_dump() -> Value {
    let path = workspace_root().join("fixtures/simulation-m4/dump-37191169.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the state recording"))
        .expect("it is JSON")
}

/// One recorded block, as the envelope the recording file wraps it in.
fn envelope(number: u64) -> Option<Value> {
    let file = corpus().join(format!("block-{number}.json"));
    serde_json::from_str(&std::fs::read_to_string(&file).ok()?).ok()
}

/// A block as a node serves it, rebuilt from the recording.
///
/// The recording holds the *normalized* types (`ChainBlock`, `ChainTransaction`,
/// `ChainReceipt` — snake_case fields, decimal numbers), which is what `RecordedSource`
/// deserializes straight into. A node answers in the wire format instead: camelCase, hex
/// quantities, a `transactions` array of hashes or of full objects depending on the
/// hydration flag. Handing the normalized object to `eth_getBlockByNumber` looks like a
/// block and decodes like nothing, so the run would fail on every read while still
/// passing an assertion that only counts gate asks. The conversion is what makes
/// 「worked through several canonical blocks」 a fact about this run rather than a claim.
///
/// Preceding fields (`gasLimit`, `miner`, `baseFeePerGas`, `excessBlobGas`, `mixHash`)
/// are not in the recording for every height; the stub answers them from the one header
/// the state dump does record, and no assertion here reads them.
fn rpc_block(envelope: &Value, hydrated: bool, header: &Value) -> Option<Value> {
    let block = envelope.get("block")?;
    let number = block.get("number")?.as_u64()?;
    let recorded = envelope.get("transactions")?.as_array()?;
    let transactions: Vec<Value> = if hydrated {
        recorded.iter().filter_map(rpc_transaction).collect()
    } else {
        recorded
            .iter()
            .filter_map(|tx| tx.get("hash").cloned())
            .collect()
    };
    Some(json!({
        "number": format!("0x{number:x}"),
        "hash": block["hash"],
        "parentHash": block["parent_hash"],
        "timestamp": format!("0x{:x}", block["timestamp"].as_u64()?),
        "gasLimit": format!("0x{:x}", header["gas_limit"].as_u64()?),
        "miner": header["beneficiary"],
        "baseFeePerGas": format!("0x{:x}", header["base_fee_per_gas"].as_u64()?),
        "excessBlobGas": format!("0x{:x}", header["excess_blob_gas"].as_u64()?),
        "mixHash": header["prevrandao"],
        "transactions": transactions,
    }))
}

/// One recorded transaction, as a hydrated block serves it.
fn rpc_transaction(tx: &Value) -> Option<Value> {
    Some(json!({
        "hash": tx["hash"],
        "transactionIndex": format!("0x{:x}", tx["tx_index"].as_u64()?),
        "from": tx["from"],
        "to": tx["to"],
        "value": tx["value"],
        "input": tx["input"],
    }))
}

/// One recorded block's receipts, as `eth_getBlockReceipts` serves them.
///
/// The recording stores receipts with snake_case keys and decimal indices, so each field
/// is re-qualified here; the adapter cross-checks that every receipt's `transactionHash`
/// and `transactionIndex` line up with the hydrated block it was asked for, which only
/// holds if both answers come from the same recording.
fn rpc_receipts(envelope: &Value) -> Vec<Value> {
    envelope["receipts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|receipt| {
            let logs = receipt["logs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|log| {
                    json!({
                        "blockNumber": format!("0x{:x}", log["block_number"].as_u64().unwrap_or(0)),
                        "transactionHash": log["tx_hash"],
                        "transactionIndex": format!("0x{:x}", log["tx_index"].as_u64().unwrap_or(0)),
                        "logIndex": format!("0x{:x}", log["log_index"].as_u64().unwrap_or(0)),
                        "address": log["address"],
                        "topics": log["topics"],
                        "data": log["data"],
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "transactionHash": receipt["tx_hash"],
                "transactionIndex": format!("0x{:x}", receipt["tx_index"].as_u64().unwrap_or(0)),
                "blockNumber": format!("0x{:x}", receipt["block_number"].as_u64().unwrap_or(0)),
                "status": if receipt["status"].as_bool().unwrap_or(true) { "0x1" } else { "0x0" },
                "logs": logs,
            })
        })
        .collect()
}

/// Every log the recording holds, as a node serves them — filtered the way the adapter
/// asks, i.e. by block range.
///
/// Deliberately unfiltered by address and topic: the stub answers the range and nothing
/// else, and the reader downstream applies its own topic filter. Returning more logs than
/// a filter would match is a node being generous; returning fewer would be the stub
/// hiding market movement from a run whose whole purpose is to show that a *passing* run
/// keeps working.
fn rpc_logs(from_block: u64, to_block: u64) -> Vec<Value> {
    let mut logs = Vec::new();
    for number in from_block..=to_block {
        let Some(envelope) = envelope(number) else {
            continue;
        };
        for receipt in envelope["receipts"].as_array().into_iter().flatten() {
            for log in receipt["logs"].as_array().into_iter().flatten() {
                logs.push(json!({
                    "blockNumber": format!("0x{:x}", log["block_number"].as_u64().unwrap_or(0)),
                    "transactionHash": log["tx_hash"],
                    "transactionIndex": format!("0x{:x}", log["tx_index"].as_u64().unwrap_or(0)),
                    "logIndex": format!("0x{:x}", log["log_index"].as_u64().unwrap_or(0)),
                    "address": log["address"],
                    "topics": log["topics"],
                    "data": log["data"],
                }));
            }
        }
    }
    logs
}

/// `params[0]` as a block number, for the quantity-shaped calls.
fn param_number(request: &Value, index: usize) -> Option<u64> {
    let text = request["params"][index].as_str()?;
    let hex = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(hex, 16).ok()
}

/// The answer for one request, or `None` for a method this stub does not pretend to be.
///
/// Every arm here is a read. There is no `eth_sendRawTransaction` and no `eth_call`
/// result that could move funds, because §3's held run has to be provable as a run that
/// reached nothing that could act.
fn answer(request: &Value, sync: Sync, chain: &str, dump: &Value) -> Option<Value> {
    let method = request["method"].as_str()?;
    let result = match method {
        "eth_chainId" => json!(chain),
        "eth_blockNumber" => json!(format!("0x{HEAD:x}")),
        "eth_syncing" => match sync {
            Sync::Idle => json!(false),
            Sync::Behind => json!({
                "startingBlock": "0x0",
                "currentBlock": "0x237b5e1",
                "highestBlock": "0x237b621"
            }),
            Sync::Rejected => {
                return Some(json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                    "code": -32601,
                    "message": "the method eth_syncing does not exist/is not available"
                }}))
            }
        },
        "eth_getBlockByNumber" => {
            let number = param_number(request, 0)?;
            let hydrated = request["params"][1].as_bool().unwrap_or(false);
            rpc_block(&envelope(number)?, hydrated, &dump["header"])?
        }
        "eth_getBlockReceipts" => {
            let number = param_number(request, 0)?;
            Value::Array(rpc_receipts(&envelope(number)?))
        }
        "eth_getCode" => {
            let address = request["params"][0].as_str()?;
            let code = dump["accounts"].get(address)?.get("code")?.as_str()?;
            json!(code)
        }
        "eth_getBalance" => {
            let address = request["params"][0].as_str()?;
            let balance = dump["accounts"].get(address)?.get("balance")?.as_str()?;
            json!(format!("0x{}", balance.parse::<u128>().ok()?))
        }
        "eth_getStorageAt" => {
            let address = request["params"][0].as_str()?;
            let slot = request["params"][1].as_str()?;
            let value = dump["storage"].get(address)?.get(slot)?.as_str()?;
            let decimal = value.parse::<u128>().ok()?;
            json!(format!("0x{decimal:064x}"))
        }
        "eth_getTransactionCount" => json!("0x0"),
        "eth_getLogs" => {
            let from = request["params"][0]["fromBlock"].as_str()?;
            let to = request["params"][0]["toBlock"].as_str()?;
            let from = u64::from_str_radix(from.strip_prefix("0x").unwrap_or(from), 16).ok()?;
            let to = u64::from_str_radix(to.strip_prefix("0x").unwrap_or(to), 16).ok()?;
            Value::Array(rpc_logs(from, to))
        }
        "eth_maxPriorityFeePerGas" | "eth_gasPrice" => json!("0x1"),
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
}

/// Serve one connection: read the body, tally the method, answer it.
///
/// `connection: close` on every reply, so the client's pool and this stub's threads do
/// not have to agree about keep-alive for the tally to be complete.
fn serve(
    mut stream: std::net::TcpStream,
    sync: Sync,
    chain: &'static str,
    received: Arc<Mutex<Vec<String>>>,
) {
    use std::io::{Read, Write};

    let dump = state_dump();
    let mut buf = vec![0u8; 65_536];
    let mut filled = 0usize;
    loop {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return,
            Ok(n) => {
                filled += n;
                let head = String::from_utf8_lossy(&buf[..filled]);
                let Some(end) = head.find("\r\n\r\n") else {
                    if filled == buf.len() {
                        return;
                    }
                    continue;
                };
                let Some(length) = content_length(&head[..end]) else {
                    return;
                };
                if filled < end + 4 + length {
                    if filled == buf.len() {
                        return;
                    }
                    continue;
                }
                let body = &buf[end + 4..end + 4 + length];
                let Ok(request) = serde_json::from_slice::<Value>(body) else {
                    return;
                };
                let Some(method) = request["method"].as_str().map(str::to_string) else {
                    return;
                };
                received.lock().expect("the tally").push(method.clone());
                let reply = match answer(&request, sync, chain, &dump) {
                    Some(value) => value.to_string(),
                    None => json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                        "code": -32601, "message": format!("{method} is not served here")
                    }})
                    .to_string(),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\
                     \r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                return;
            }
            Err(_) => return,
        }
    }
}

fn content_length(headers: &str) -> Option<usize> {
    headers
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|line| line.split_once(':')?.1.trim().parse().ok())
}

/// A live `HttpPoll` run against the stub, with the endpoint's URL in place of any real
/// node's. The evidence directory is per test and emptied first, so a directory left by
/// an earlier build cannot be read as this run's record.
fn config(stub: &Stub, name: &str, blocks: Option<u64>) -> PipelineConfig {
    config_at(&stub.url, name, blocks)
}

/// [`config`], with the endpoint spelled out rather than taken from a stub's own address,
/// so a test can point a run at a URL carrying something a record must not repeat.
fn config_at(url: &str, name: &str, blocks: Option<u64>) -> PipelineConfig {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = PipelineConfig::live(Some(url), None, registries(), dir);
    config.canonical_source = CanonicalSource::HttpPoll;
    // §7's pinned start: the run moves forward from just before the block whose state
    // the recording holds, so the blocks it reads are the ones the corpus contains.
    config.start_block = Some(STATE_BLOCK - 1);
    config.max_blocks = blocks;
    config.duration = std::time::Duration::from_secs(30);
    config.source.poll_interval_ms = 0;
    config.progress = false;
    config
}

/// The whole §3 hold: the node says it is still syncing, and the run stops at the gate.
#[tokio::test]
async fn a_node_that_is_still_syncing_is_refused_before_it_is_read() {
    let stub = Stub::spawn(Sync::Behind);
    let error = run(&config(&stub, "readiness-syncing", Some(1)))
        .await
        .expect_err("a syncing node holds the run");
    let PipelineError::NodeNotReady { detail, .. } = &error else {
        panic!("a syncing node has to be refused as not ready, got {error}");
    };
    assert!(detail.contains("sync still in progress"), "{detail}");

    // The endpoint's own tally: the gate's ask, and nothing after it. No block, no
    // header, no account read, no execution-lane connect — which is §3's 「不得把缺失的
    // 块/储备/receipt 解读成零」 satisfied structurally rather than by promise.
    let methods = stub.received();
    let sync_at = methods
        .iter()
        .position(|m| m == "eth_syncing")
        .expect("the gate asked");
    assert_eq!(
        methods.len(),
        sync_at + 1,
        "the run asked nothing of the node after the gate held it: {methods:?}"
    );
    assert_eq!(
        &methods[..],
        &["eth_chainId".to_string(), "eth_syncing".to_string()],
        "the connect and the gate are all a held run gets to: {methods:?}"
    );
    for banned in ["eth_getBlockByNumber", "eth_sendRawTransaction", "eth_call"] {
        assert!(!methods.iter().any(|m| m == banned), "{banned}");
    }

    // And no session was opened: the evidence writer comes after the gate, so a held
    // run leaves no directory behind to be read as a record of a session.
    let dir = workspace_root()
        .join("target/pipeline-tests")
        .join("readiness-syncing");
    assert!(
        !dir.exists(),
        "a refused run writes no session record: {}",
        dir.display()
    );
}

/// §3's 「不允许静默切回公共端点」 plus the budget, in one shape: the node rejects the
/// method. A rejected ask is not a pass, and it is not retried into a loop.
#[tokio::test]
async fn a_node_that_rejects_the_method_is_refused_rather_than_assumed_ready() {
    let stub = Stub::spawn(Sync::Rejected);
    let error = run(&config(&stub, "readiness-rejected", Some(1)))
        .await
        .expect_err("an endpoint that will not answer is not a ready one");
    assert!(
        matches!(error, PipelineError::NodeNotReady { .. }),
        "{error}"
    );
    let methods = stub.received();
    assert_eq!(
        methods,
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "a JSON-RPC error is terminal: one ask, no retry, no fall-through to ready"
    );
}

/// The run §3 is actually for: a node that answers `false` starts, asks exactly once for
/// the whole session, and asks before any block is read.
#[tokio::test]
async fn a_ready_node_is_asked_once_and_the_run_carries_on_reading_blocks() {
    let stub = Stub::spawn(Sync::Idle);
    // Three canonical blocks: enough market cycles that a per-event ask would show up
    // as three, and small enough to keep this test off the suite's critical path.
    let report = run(&config(&stub, "readiness-ready", Some(3)))
        .await
        .expect("a node that says it is not syncing is allowed to be read");

    let methods = stub.received();
    assert_eq!(
        stub.count("eth_syncing"),
        1,
        "one gate ask for a run that processed several blocks — the per-event path \
         never asked: {methods:?}"
    );
    let sync_at = methods
        .iter()
        .position(|m| m == "eth_syncing")
        .expect("the gate asked");
    let first_block_read = methods
        .iter()
        .position(|m| m == "eth_getBlockByNumber")
        .expect("the run read blocks");
    assert!(
        sync_at < first_block_read,
        "the gate sits ahead of the first block read: {methods:?}"
    );
    assert!(
        methods[..sync_at]
            .iter()
            .all(|m| m == "eth_chainId" || m == "eth_syncing"),
        "the only thing before the gate is the connect: {methods:?}"
    );

    // The record carries the gate's own tally rather than an inference: a reader of the
    // session can check the number against the endpoint's log.
    let status =
        std::fs::read_to_string(report.evidence_dir.join("status.jsonl")).expect("a status file");
    let line = status
        .lines()
        .map(|l| -> Value { serde_json::from_str(l).expect("a JSONL row") })
        .find(|row| row.get("readiness").is_some())
        .expect("one readiness line in the session record");
    assert_eq!(line["readiness"]["eth_syncing_asks"], 1);
    assert_eq!(line["readiness"]["verdict"], "ready");
    assert_eq!(line["readiness"]["head_freshness_policy"], "not_judged");

    let metrics_text =
        std::fs::read_to_string(report.evidence_dir.join("metrics.json")).expect("a metrics file");
    let metrics: Value = serde_json::from_str(&metrics_text).expect("valid JSON");
    assert_eq!(
        metrics["counters"]["readiness.eth_syncing_asks"], 1,
        "the counter is the gate's tally, not the number of blocks the run read"
    );

    // And the run did read: three canonical blocks in, the logs inside them decoded,
    // pools priced from the recorded state, candidates made. Without these numbers the
    // ask count above would be a measurement of a run that failed on its first block —
    // which is exactly what an unwrapped `eth_syncing == 1` assertion cannot tell apart
    // from a healthy one. `blocks` and `event.canonical` are the two the ingestion
    // itself writes, so the assertion is on the path the gate is supposed to clear.
    assert_eq!(metrics["counters"]["blocks"], 3, "{metrics_text}");
    assert_eq!(metrics["counters"]["event.canonical"], 3, "{metrics_text}");
    assert!(
        metrics["counters"]["logs"].as_u64().unwrap_or(0) > 0,
        "the blocks the run read carried their logs, so the stub's answers decoded as \
         a node's rather than as nothing: {metrics_text}"
    );
    for failure in [
        "canonical_source_failed_during_run",
        "event.disconnected",
        "event.source_failed",
    ] {
        assert_eq!(
            metrics["counters"]
                .get(failure)
                .and_then(Value::as_u64)
                .unwrap_or(0),
            0,
            "a run past the gate read its blocks without a source failure: {metrics_text}"
        );
    }
}

/// A pinned-start live run reads no head of its own (§7 says the start number is the
/// operator's). With a freshness requirement in force that is a run that cannot answer
/// the question it was asked, so §3's fail-closed rule applies and it is refused — the
/// alternative is a `ready` verdict built on a number nobody read.
#[tokio::test]
async fn a_freshness_rule_with_no_head_to_measure_is_not_a_pass() {
    let stub = Stub::spawn(Sync::Idle);
    let mut config = config(&stub, "readiness-no-head", Some(1));
    config.readiness = evm_chain::HeadFreshnessPolicy::AgainstReference {
        reference_head: HEAD + 2,
        tolerance_blocks: 1,
    };
    let error = run(&config).await.expect_err("no head, no judgement");
    let PipelineError::NodeNotReady { detail, .. } = &error else {
        panic!("{error}");
    };
    assert!(detail.contains("head"), "{detail}");
    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "the gate asked its one question and asked nothing else"
    );
}

/// The other half of §3's freshness rule: a node that says `false` can still be behind
/// the reference the operator named, and `false` is not a claim about the network head.
#[tokio::test]
async fn a_node_behind_the_named_reference_is_held_though_it_says_it_is_not_syncing() {
    let stub = Stub::spawn(Sync::Idle);
    let mut config = config(&stub, "readiness-behind-reference", Some(1));
    // The run reads its head (§7's live default), so drop the pinned start: with
    // `start_block` set there is no observed head and the previous test's rule applies.
    config.start_block = None;
    config.readiness = evm_chain::HeadFreshnessPolicy::AgainstReference {
        reference_head: HEAD + 10,
        tolerance_blocks: 2,
    };
    let error = run(&config)
        .await
        .expect_err("ten blocks behind a two-block tolerance");
    let PipelineError::NodeNotReady { detail, .. } = &error else {
        panic!("{error}");
    };
    assert!(detail.contains("behind"), "{detail}");
    assert_eq!(stub.count("eth_syncing"), 1);
}

/// §3's 「未就绪时 Signer/Submitter 未被调用」, asked where it could actually go wrong:
/// a run that *did* ask for the execution lane. The lane connects later in `run` than the
/// gate does, so a held run never reaches the code that reads a nonce or signs.
#[tokio::test]
async fn an_armed_execution_lane_never_connects_behind_a_held_gate() {
    let stub = Stub::spawn(Sync::Behind);
    let mut config = config(&stub, "readiness-armed-lane", Some(1));
    config.execution = Some(ExecutionSetup {
        mode: ExecutionMode::BuildOnly,
        ..ExecutionSetup::default()
    });
    assert!(matches!(
        run(&config).await,
        Err(PipelineError::NodeNotReady { .. })
    ));
    let methods = stub.received();
    assert_eq!(
        methods,
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "the lane's own reads — nonce, fee, code — are all downstream of a gate this \
         run did not pass"
    );
    assert!(
        !methods.iter().any(|m| m == "eth_getTransactionCount"),
        "a held run did not even look at a nonce: {methods:?}"
    );
}

/// §9's D1: when the candidate endpoint answers for a different chain, the refusal has to
/// name the two sides in their real roles. The run has already attested one chain; *this*
/// endpoint is the thing that disagrees. M12-A §16 found the message printing the two the
/// other way round, which sent a reader to fix the registry over a candidate URL. The
/// comparison is the same fail-closed check it always was — only the roles moved.
#[tokio::test]
async fn a_candidate_endpoint_on_another_chain_is_refused_naming_the_side_that_disagreed() {
    let canonical = Stub::spawn(Sync::Idle);
    let candidate = Stub::spawn_at(Sync::Idle, OTHER_CHAIN_HEX);
    let mut config = config(&canonical, "readiness-flashblocks-chain", Some(1));
    config.flashblocks_url = Some(candidate.url.clone());
    let error = run(&config)
        .await
        .expect_err("an endpoint that answers for another chain is not a candidate source");
    let PipelineError::ChainMismatch {
        registry,
        node,
        endpoint,
    } = &error
    else {
        panic!("a disagreeing endpoint has to stop the run as a chain mismatch: {error}");
    };
    let chain_number = |hex: &str| -> u64 {
        u64::from_str_radix(hex.trim_start_matches("0x"), 16).expect("a hex chain id")
    };
    assert_eq!(
        *registry,
        chain_number(CHAIN_HEX),
        "the `registry` slot holds the chain this run had already attested, not the answer \
         that came in late"
    );
    assert_eq!(
        *node,
        chain_number(OTHER_CHAIN_HEX),
        "the `node` slot holds what *this* endpoint answered to `eth_chainId`"
    );
    assert_eq!(
        endpoint, &candidate.url,
        "and the endpoint the message names is the candidate that disagreed, not the \
         canonical one the run is reading"
    );

    // Refused at connect: a source on the wrong chain never gets asked for a block, so a
    // reader cannot mistake half a run for a working all-local setup.
    let methods = candidate.received();
    for banned in ["eth_getBlockByNumber", "eth_sendRawTransaction", "eth_call"] {
        assert!(
            !methods.iter().any(|m| m == banned),
            "the refused endpoint was asked {methods:?}, which is past a connect"
        );
    }
}

/// §3's last rule, in the one shape a test can hold it in: a replay has no node, so the
/// gate is recorded as never asked rather than as answered. Writing `false` here would be
/// a measurement nobody took.
#[tokio::test]
async fn a_replay_records_that_readiness_was_never_asked() {
    let dir = workspace_root().join("target/pipeline-tests/readiness-replay");
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = PipelineConfig::replay(registries(), dir.clone(), corpus());
    config.max_blocks = Some(1);
    config.progress = false;
    let report = run(&config).await.expect("a replay run");

    let status =
        std::fs::read_to_string(report.evidence_dir.join("status.jsonl")).expect("a status file");
    let line = status
        .lines()
        .map(|l| -> Value { serde_json::from_str(l).expect("a JSONL row") })
        .find(|row| row.get("readiness").is_some())
        .expect("the replay also records its gate, as an absence");
    assert_eq!(
        line["readiness"]["verdict"],
        Value::Null,
        "no node was asked, so there is no verdict"
    );
    assert_eq!(line["readiness"]["eth_syncing_asks"], 0);

    let metrics_text =
        std::fs::read_to_string(report.evidence_dir.join("metrics.json")).expect("a metrics file");
    let metrics: Value = serde_json::from_str(&metrics_text).expect("valid JSON");
    assert_eq!(metrics["counters"]["readiness.eth_syncing_asks"], 0);
}

/// M12-B §4's three facts about one endpoint, held in three keys that are not derived
/// from one another: what the operator declared the endpoint to be, which endpoint the
/// declaration describes, and the digest the RPC trace lines call it by.
///
/// The URL is the worst shape §4 names, on purpose: a loopback address, which §4.1
/// forbids reading as a sign of anything, with a path segment that reads like an API key,
/// which §4.4 forbids repeating. So the two keys M12-B adds are searched for that
/// fragment rather than trusted, and the whole directory is searched with it.
#[tokio::test]
async fn a_run_records_the_declared_label_and_the_endpoint_identity_as_two_things() {
    let stub = Stub::spawn(Sync::Idle);
    let url = format!("{}/token/abc123", stub.url);

    let undeclared = run(&config_at(&url, "endpoints-undeclared", Some(1)))
        .await
        .expect("a node that says it is not syncing is ready, labelled or not");
    let quiet = &undeclared.session["endpoints"];

    // §4.1 and §4.6 at the record layer: this run's node is on 127.0.0.1 and the record
    // still says nobody declared anything. `unknown` is written rather than omitted — a
    // missing key reads as a schema change, and an inferred one reads as a measurement.
    assert_eq!(quiet["rpc_purpose"], "unknown");
    assert_eq!(quiet["flashblocks_purpose"], "unknown");
    let quiet_detail = quiet["purpose_detail"]
        .as_str()
        .expect("the labels travel with the sentence that says what they are");
    assert!(
        quiet_detail.contains("canonical: not declared"),
        "{quiet_detail}"
    );
    assert!(
        !quiet_detail.contains("local"),
        "a loopback host did not turn into a declaration: {quiet_detail}"
    );

    // §4.2: identity and digest in separate keys, and the digest is the trace's own — one
    // rule, so the two cannot disagree about whether two lines name one provider.
    let digest = quiet["rpc_endpoint_id"]
        .as_str()
        .expect("an endpoint this run spoke to has a digest");
    assert_eq!(
        digest,
        evm_chain::endpoint_id(&url),
        "the session's digest and the RPC trace's are the same function on the same URL"
    );
    assert!(
        digest.starts_with("rpc-") && digest.len() == 20,
        "a digest prefix, not a URL and not a whole hash: {digest}"
    );
    assert!(
        quiet["ws_endpoint_id"].is_null(),
        "this run opens no socket, so there is no digest for one"
    );
    assert!(quiet["flashblocks_endpoint_id"].is_null());

    // The declared run: a loopback endpoint declared *public*, and a second endpoint this
    // run reaches under the flashblocks role.
    let mut loud = config_at(&url, "endpoints-declared", Some(1));
    loud.canonical_purpose = evm_chain::EndpointPurpose::PublicCanonicalRpc;
    loud.flashblocks_url = Some(format!("{url}/flashblocks"));
    loud.flashblocks_purpose = evm_chain::EndpointPurpose::LocalFlashblocksRpc;
    let declared = run(&loud)
        .await
        .expect("a declaration is a label, not a new check the node has to pass");
    let stated = &declared.session["endpoints"];

    // §4.5's record half: a local-looking URL declared public stays public. The word
    // travelled from the flag to the file and nothing about the host entered the copy.
    assert_eq!(stated["rpc_purpose"], "public_canonical_rpc");
    assert_eq!(stated["flashblocks_purpose"], "local_flashblocks_rpc");
    let stated_detail = stated["purpose_detail"]
        .as_str()
        .expect("the same sentence");
    assert!(
        stated_detail.contains("canonical: public_canonical_rpc")
            && stated_detail.contains("flashblocks: local_flashblocks_rpc"),
        "{stated_detail}"
    );

    // One endpoint, one digest, whatever it is called; and two URLs under one host are
    // still two digests, because the record is about endpoints, not about words.
    assert_eq!(
        stated["rpc_endpoint_id"], quiet["rpc_endpoint_id"],
        "a label does not change which endpoint an endpoint is"
    );
    assert_ne!(
        stated["flashblocks_endpoint_id"], stated["rpc_endpoint_id"],
        "the same host asked for two roles is two identities, not one label swapped"
    );

    // §10's no-behaviour-change, read off the endpoint rather than asserted: each of the
    // two runs asked the gate its one question, and a label cost no read.
    assert_eq!(
        stub.count("eth_syncing"),
        2,
        "two runs, two asks — the label added nothing to the wire: {:?}",
        stub.received()
    );

    // §4.4 last, and across the whole directory rather than across the keys this milestone
    // knows about: the credential-shaped fragment reaches exactly one file per run — the
    // session record — and inside it only ever as one of the endpoint URLs the operator
    // configured. Those URLs already travelled before M12-B (`endpoints`, the capability
    // table, and each source's own capability line), and §4.3 asks for that to stay the
    // whole of it: a purpose, a digest or the sentence under them must not pick the
    // fragment up.
    let flashblocks_url = format!("{url}/flashblocks");
    for (name, report, configured) in [
        ("endpoints-undeclared", &undeclared, vec![url.clone()]),
        (
            "endpoints-declared",
            &declared,
            vec![url.clone(), flashblocks_url.clone()],
        ),
    ] {
        assert_eq!(
            files_holding(&report.evidence_dir, "abc123"),
            vec!["live-session.json".to_string()],
            "{name}: a fragment that looks like a key belongs to the configured URL and \
             to no other file the run wrote"
        );
        let mut carriers = strings_holding(&report.session, "abc123");
        carriers.sort();
        carriers.dedup();
        assert_eq!(
            carriers, configured,
            "{name}: every string in the record that holds the fragment is a whole \
             endpoint the operator named — not a label, not a digest, not a detail"
        );
        for key in [
            "rpc_purpose",
            "flashblocks_purpose",
            "rpc_endpoint_id",
            "ws_endpoint_id",
            "flashblocks_endpoint_id",
            "purpose_detail",
        ] {
            let written = report.session["endpoints"][key].to_string();
            assert!(
                !written.contains("abc123") && !written.contains("127.0.0.1"),
                "{name}: the new `{key}` key carries {written}"
            );
        }
    }
}

/// Every file in a run's directory whose text holds `needle`, sorted by name.
fn files_holding(dir: &std::path::Path, needle: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a run leaves an evidence directory") {
        let path = entry.expect("an entry").path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains(needle) {
            names.push(
                path.file_name()
                    .expect("a file")
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    names.sort();
    names
}

/// Every string value in a JSON document that holds `needle`, as it stands there. A
/// fragment that had travelled into a digest or a detail sentence would show up as
/// something other than the URL it came from.
fn strings_holding(value: &Value, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    match value {
        Value::String(text) => {
            if text.contains(needle) {
                found.push(text.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                found.extend(strings_holding(item, needle));
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                found.extend(strings_holding(item, needle));
            }
        }
        _ => {}
    }
    found
}

/// The hot path, scanned rather than sampled. §3's rule 「不要在每次行情事件上调用」 is a
/// statement about where the wire call lives, so the check is structural: the method name
/// appears in a request position in exactly one production line, and the gate that makes
/// it is constructed in exactly one production line, in the bootstrap.
///
/// The scan deliberately does not count bare mentions. `runner.rs` says `eth_syncing` three
/// times — two evidence keys that report the tally §3 asks for, and one doc comment — and a
/// scan that called those "call sites" would either fail on the record it is meant to
/// protect or be relaxed until it proved nothing. A request position is the thing that can
/// cost a run an RPC call, so that is the thing that is counted, and the planted line below
/// is the control that says the counter counts.
#[test]
fn the_ask_lives_in_the_bootstrap_and_nowhere_else() {
    // The counter's own control: a mention that is not a call is not counted, and a call
    // written the way a second call site would write it is.
    assert_eq!(call_sites("// nothing asks eth_syncing here"), 0);
    assert_eq!(call_sites("json!({\"eth_syncing_asks\": 1})"), 0);
    assert_eq!(
        call_sites(r#"let r = c.request_raw("eth_syncing", params).await?;"#),
        1
    );

    let mut sites: Vec<(PathBuf, usize)> = Vec::new();
    let mut gates: Vec<(PathBuf, usize)> = Vec::new();
    for crate_name in [
        "chain",
        "pipeline",
        "live",
        "execution",
        "simulation",
        "opportunity",
        "graph",
        "state",
        "discovery",
        "pathfinder",
        "risk",
        "metrics",
        "replay",
        "protocol",
        "core",
    ] {
        let root = workspace_root().join("crates").join(crate_name).join("src");
        let Ok(files) = collect_rs(&root) else {
            continue;
        };
        for file in files {
            let text = std::fs::read_to_string(&file).expect("a source file");
            // Production only: a unit test that builds a gate to exercise it is not a run
            // that builds one. The same cut the `latest`-pinning guard uses, for the same
            // reason — a scan that counted test scaffolding would have to allow it, and an
            // allowance is how a real second call site starts passing.
            let production = match text.find("\n#[cfg(test)]") {
                Some(at) => &text[..at],
                None => &text[..],
            };
            for (n, line) in production.lines().enumerate() {
                if call_sites(line) > 0 {
                    sites.push((file.clone(), n + 1));
                }
                if line.contains("ReadinessGate::new")
                    || line.contains("ReadinessGate::with_budget")
                {
                    gates.push((file.clone(), n + 1));
                }
            }
        }
    }

    assert_eq!(
        sites.len(),
        1,
        "exactly one production line sends `eth_syncing`: {sites:?}"
    );
    assert!(
        sites[0].0.ends_with("chain/src/readiness.rs"),
        "and it is the adapter method the gate calls, not a market handler, a source \
         loop or an evidence writer: {sites:?}"
    );
    assert_eq!(
        gates.len(),
        1,
        "exactly one place builds a gate, and it is the bootstrap: {gates:?}"
    );
    assert!(gates[0].0.ends_with("pipeline/src/runner.rs"), "{gates:?}");
}

/// How many times one line puts `eth_syncing` in a request position — that is, sends it.
fn call_sites(line: &str) -> usize {
    ["request_raw(\"eth_syncing\"", "request(\"eth_syncing\""]
        .iter()
        .filter(|needle| line.contains(*needle))
        .count()
}

fn collect_rs(dir: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}
