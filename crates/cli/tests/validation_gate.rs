//! M12-D §3's isolation proof at the entry that puts **one transaction** on chain:
//! `evm_cli::run_validation`, the code behind `evm-mev-bot validate`.
//!
//! This is the second of the two spending entries — the route run is proved in
//! `crates/pipeline/tests/readiness_isolation.rs`, and the structural scan there covers both.
//! M12-B's gate sat in the discovery bootstrap only, so a validation run connected to the
//! endpoint, checked the chain id, read a head and a header, and *then* built an
//! `ExecutionStage`, which is what constructs a `Signer`. A syncing or broken node could
//! therefore be the fee-and-nonce input of a transaction this repository was about to submit.
//!
//! What this file proves, in §3's own terms:
//!
//! * a node reporting sync in progress is refused before the head read, so the pin, the
//!   header context and the stage are never built;
//! * so is a node that rejects `eth_syncing`, answers an illegal shape, or never answers —
//!   and each is refused in the node's own words, not as a timeout the run recovered from;
//! * a run configured `sign-only`, on a machine with no signing key in the environment, stops
//!   with the readiness refusal rather than the signer's complaint, which is the difference
//!   between 「Signer 不被调用」 and a run that merely failed later;
//! * the refusal names the endpoint it asked, and a second, perfectly ready endpoint bound in
//!   the same test receives nothing — there is no fallback to a public RPC;
//! * the admitted control gets *past* the gate and stops on the next read the stub does not
//!   serve, so the four refusals above are a distinction and not the only outcome a stub can
//!   produce.
//!
//! Nothing here signs or broadcasts. The stub serves `eth_chainId`, `eth_blockNumber` and the
//! readiness answer only, and refuses everything else — including the whole submission
//! family — so a run that reached a lane would fail on a method rather than get as far as a
//! network. No key is read: the tests that could read one are held by the gate first, and
//! `GIWA_EXECUTION_PRIVATE_KEY` is never set by this file. Every address bound is
//! 127.0.0.1 on an ephemeral port owned by this process.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alloy_primitives::Address;
use serde_json::{json, Value};

use evm_cli::{run_validation, ValidationPlan};
use evm_execution::{ExecutionMode, PRIVATE_KEY_ENV};
use evm_pipeline::PipelineError;

const CHAIN_HEX: &str = "0x164ce";
/// The head the admitted control is served, so its stop lands *after* the gate rather than at
/// the same call a held run stops at.
const HEAD: u64 = 37_191_199;

/// What the stub answers `eth_syncing` with. `Idle` is the control; the other four are the
/// answers §3 lists as never counting as ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sync {
    Idle,
    Behind,
    Rejected,
    Undecodable,
    /// The connection dies with no reply. The adapter retries a transport failure once, so
    /// this shape costs two asks — pre-existing behaviour, untouched by this milestone.
    Silent,
}

impl Sync {
    const fn asks(self) -> usize {
        match self {
            Self::Silent => 2,
            _ => 1,
        }
    }

    /// The words the refusal has to carry, as the chain crate's `answer_class` renders them.
    const fn detail(self) -> &'static str {
        match self {
            Self::Behind => "sync still in progress",
            Self::Rejected => "the node rejected the call",
            Self::Undecodable => "the node's answer could not be decoded",
            Self::Silent => "the call produced no usable answer",
            Self::Idle => "not a hold",
        }
    }
}

/// A stub node on an ephemeral localhost port, tallying the methods it was asked for, in
/// arrival order — the same recipe `readiness_isolation.rs` proves the route entry with.
struct Stub {
    url: String,
    received: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn spawn(sync: Sync) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("an address").port();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let received = received.clone();
            move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let received = received.clone();
                    std::thread::spawn(move || serve(stream, sync, received));
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            received,
        }
    }

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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("a crate sits two levels under the workspace root")
        .to_path_buf()
}

/// The answer for one request, or `None` for a method this stub does not pretend to be.
///
/// Deliberately fewer reads than a validation run wants: everything past `eth_blockNumber` —
/// the header context, the fee, the nonce, the reserves, and the whole submission family — is
/// refused, so the tally being short of them is the witness that a held run never built a
/// lane.
fn answer(request: &Value, sync: Sync) -> Option<Value> {
    let method = request["method"].as_str()?;
    let result = match method {
        "eth_chainId" => json!(CHAIN_HEX),
        "eth_blockNumber" => json!(format!("0x{HEAD:x}")),
        "eth_syncing" => match sync {
            Sync::Idle => json!(false),
            Sync::Behind => json!({
                "startingBlock": "0x0",
                "currentBlock": "0x237b5e1",
                "highestBlock": "0x237b621"
            }),
            Sync::Undecodable => json!(true),
            Sync::Rejected => {
                return Some(json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                    "code": -32601,
                    "message": "the method eth_syncing does not exist/is not available"
                }}))
            }
            Sync::Silent => json!(false),
        },
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
}

/// Serve one connection: read the body, tally the method, answer it. `connection: close` on
/// every reply, so the client's pool and this stub's threads need not agree about keep-alive.
fn serve(mut stream: std::net::TcpStream, sync: Sync, received: Arc<Mutex<Vec<String>>>) {
    use std::io::{Read, Write};

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
                if sync == Sync::Silent && method == "eth_syncing" {
                    return;
                }
                let reply = match answer(&request, sync) {
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

/// One validation run's plan, on the stub's URL, with the mode named by the caller.
///
/// `--sender` is passed explicitly so a `build-only` run never needs a signer address from a
/// lane; `to` is the same fixture address, and `value` is zero. No run built here gets far
/// enough to build a transaction, let alone sign one.
fn plan(stub: &Stub, name: &str, mode: ExecutionMode) -> ValidationPlan {
    let dir = workspace_root().join("target/cli-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    ValidationPlan {
        rpc_url: stub.url.clone(),
        mode,
        sender: Some(Address::from_slice(&[0x11u8; 20])),
        to: Some(Address::from_slice(&[0x22u8; 20])),
        value_wei: 0,
        gas_limit: 21_000,
        registry_dirs: vec![
            workspace_root().join("data/protocols"),
            workspace_root().join("data/protocols-m3"),
        ],
        evidence_dir: dir,
    }
}

fn evidence_dir(name: &str) -> PathBuf {
    workspace_root().join("target/cli-tests").join(name)
}

/// The error a held run has to return, without asking [`evm_cli::ValidationOutcome`] to be
/// `Debug` — it is a finished transaction's record, and the trait was never derived for it.
fn refusal(outcome: Result<evm_cli::ValidationOutcome, PipelineError>) -> PipelineError {
    match outcome {
        Err(error) => error,
        Ok(run) => panic!(
            "a validation the gate should have held completed instead: {} wrote {}",
            run.session_id,
            run.evidence_dir.display()
        ),
    }
}

/// §3's invariant 1 at this entry, and the ordering it depends on: the node says it is still
/// syncing, and the run stops before the head read that the pin — and therefore the lane — is
/// built from.
#[tokio::test]
async fn a_syncing_node_holds_the_validation_before_it_reads_a_head() {
    let stub = Stub::spawn(Sync::Behind);
    let error =
        refusal(run_validation(plan(&stub, "validation-syncing", ExecutionMode::BuildOnly)).await);
    let PipelineError::NodeNotReady { endpoint, detail } = &error else {
        panic!("a syncing node has to be refused as not ready, got {error}");
    };
    assert_eq!(
        endpoint, &stub.url,
        "the refusal names the endpoint it asked"
    );
    assert!(detail.contains(Sync::Behind.detail()), "{detail}");

    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "a held validation gets no further than the gate: {:?}",
        stub.received()
    );
    for banned in [
        "eth_blockNumber",
        "eth_getBlockByNumber",
        "eth_getTransactionCount",
        "eth_gasPrice",
        "eth_maxPriorityFeePerGas",
        "eth_estimateGas",
        "eth_sendRawTransaction",
    ] {
        assert_eq!(
            stub.count(banned),
            0,
            "{banned} is asked by a run that passed the gate, and this one did not"
        );
    }
    assert!(
        !evidence_dir("validation-syncing").exists(),
        "a held run opens no session directory: {}",
        evidence_dir("validation-syncing").display()
    );
}

/// §3's 「`eth_syncing` 返回错误、超时、无效响应」 at this entry: an answer that is neither
/// `false` nor a progress object is a hold, in the node's own words, and it is not retried
/// into a pass. The ask budget is named per shape because only a dead connection is retried.
#[tokio::test]
async fn a_node_that_will_not_or_cannot_answer_holds_this_entry_too() {
    for (case, sync) in [
        ("rejected", Sync::Rejected),
        ("undecodable", Sync::Undecodable),
        ("silent", Sync::Silent),
    ] {
        let stub = Stub::spawn(sync);
        let name = format!("validation-{case}");
        let error = refusal(run_validation(plan(&stub, &name, ExecutionMode::BuildOnly)).await);
        let PipelineError::NodeNotReady { detail, .. } = &error else {
            panic!("{case}: got {error}");
        };
        assert!(
            detail.contains(sync.detail()),
            "{case}: the refusal should carry the node's own class of failure, got {detail}"
        );
        assert_eq!(
            stub.count("eth_syncing"),
            sync.asks(),
            "{case}: one gate ask, and only a transport failure is retried"
        );
        assert_eq!(
            stub.count("eth_blockNumber"),
            0,
            "{case}: nothing is read after a refusal"
        );
        assert!(
            !evidence_dir(&name).exists(),
            "{case}: no session directory"
        );
    }
}

/// §3's 「未就绪时，Signer 不被调用」 in the form that can only fail: the run is armed for a
/// mode that *must* have a key, and this file never puts one in the environment. Without the
/// gate the next thing this run does after the header read is build an `ExecutionStage`, and
/// the error would be the signer's own complaint about a missing key.
#[tokio::test]
async fn a_held_validation_stops_with_readiness_rather_than_with_a_key_problem() {
    let key_present = std::env::var(PRIVATE_KEY_ENV).is_ok();
    let stub = Stub::spawn(Sync::Behind);
    let error =
        refusal(run_validation(plan(&stub, "validation-sign-only", ExecutionMode::SignOnly)).await);
    let PipelineError::NodeNotReady { detail, .. } = &error else {
        panic!("the gate holds this run, so nothing downstream decides its outcome: {error}");
    };
    assert!(detail.contains(Sync::Behind.detail()), "{detail}");
    assert_eq!(stub.count("eth_syncing"), 1);
    assert_eq!(
        stub.count("eth_getTransactionCount"),
        0,
        "the nonce read the lane makes sits behind the stage, and the stage is behind the gate"
    );
    if !key_present {
        // The discriminator, stated rather than assumed: on a machine with no key the
        // unfixed ordering cannot reach the signer, so it cannot produce its message.
        assert!(
            !error.to_string().contains("signing key"),
            "the refusal is readiness's, not the signer's: {error}"
        );
    }
    assert!(
        !evidence_dir("validation-sign-only").exists(),
        "and nothing was written about a run that never started"
    );
}

/// §3's 「不会静默回退到未经授权的公共 RPC」, witnessed: a second, ready endpoint is bound and
/// left unreferenced, and receives nothing.
#[tokio::test]
async fn a_held_validation_contacts_no_other_endpoint() {
    let configured = Stub::spawn(Sync::Behind);
    let bystander = Stub::spawn(Sync::Idle);
    let error = refusal(
        run_validation(plan(
            &configured,
            "validation-no-fallback",
            ExecutionMode::BuildOnly,
        ))
        .await,
    );
    let PipelineError::NodeNotReady { endpoint, .. } = &error else {
        panic!("got {error}");
    };
    assert_eq!(endpoint, &configured.url);
    assert_eq!(
        bystander.received(),
        Vec::<String>::new(),
        "an endpoint this run was never told about received nothing"
    );
}

/// The control that makes the four refusals above a distinction: the same stub family with a
/// ready node gets *past* the gate, reads its head, and stops further on — on a method this
/// stub does not serve, which is what keeps this test off the signing and broadcasting paths.
#[tokio::test]
async fn an_admitted_validation_gets_past_the_gate_and_stops_on_the_next_read() {
    let stub = Stub::spawn(Sync::Idle);
    let error =
        refusal(run_validation(plan(&stub, "validation-admitted", ExecutionMode::BuildOnly)).await);
    assert!(
        !matches!(error, PipelineError::NodeNotReady { .. }),
        "the ready node was not held: {error}"
    );
    let methods = stub.received();
    assert_eq!(
        stub.count("eth_syncing"),
        1,
        "one gate ask for the entry, not one per read: {methods:?}"
    );
    let sync_at = methods
        .iter()
        .position(|m| m == "eth_syncing")
        .expect("the gate asked");
    let head_at = methods
        .iter()
        .position(|m| m == "eth_blockNumber")
        .expect("the admitted run read its head");
    assert!(
        sync_at < head_at,
        "the ask sits ahead of the head read: {methods:?}"
    );
    assert!(
        methods[..sync_at]
            .iter()
            .all(|m| m == "eth_chainId" || m == "eth_syncing"),
        "the only thing before the gate is the connect: {methods:?}"
    );
    assert_eq!(
        stub.count("eth_sendRawTransaction"),
        0,
        "and the run still never reached the submission family"
    );
}
