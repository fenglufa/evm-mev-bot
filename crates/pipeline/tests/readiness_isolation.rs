//! M12-D §3's isolation proofs at the two entries that can *spend*: the route run
//! (`evm_pipeline::arbitrage::run_once`) and the single-transaction validation run
//! (`run_validation` in `evm-cli`, covered by `crates/cli/tests/validation_gate.rs`).
//!
//! M12-B put the readiness gate in the discovery bootstrap and proved it there
//! (`readiness_startup.rs`). The audit this milestone starts from found that the two
//! spending entries never went through it: a route run connected to the endpoint, checked
//! the chain id, and read a block, and only then built a sequence lane — which is what
//! constructs a `Signer`. A syncing or broken node was therefore able to be the price input
//! of a real execution path. M12-D §3's invariant 1 (「节点未就绪时不允许进入交易执行路径」)
//! was true of the run that only watches and false of the runs that act.
//!
//! What this file proves, in the order §3 lists it:
//!
//! * a syncing node holds the route before it reads a block, and the held run leaves no
//!   session directory behind — the Signer below the gate is never constructed, because
//!   nothing below the gate is reached;
//! * a node that rejects `eth_syncing`, answers a shape that is not one of the two legal
//!   ones, or never answers at all is held the same way, and is never read as "ready";
//! * a refusal is an error out of `run_once`, not an `Ok` run with no candidates — which the
//!   admitted control shows is a real distinction, since the same stub family with a ready
//!   node gets past the gate and stops later, somewhere else;
//! * the only endpoint the run touches is the one it was configured with (§3's 「不会静默
//!   回退到未经授权的公共 RPC」, witnessed by a second listener that receives nothing);
//! * recovery is a *new* ask, not an inherited pass: the next run against a node that has
//!   caught up is admitted, having asked exactly once for itself.
//!
//! Plus two structural gates that the behavioural tests cannot reach: every production file
//! that constructs a signing or submitting stage calls the gate above that line, and this fix
//! adds no RPC method and no second gate — the ask stays in `crates/chain/src/readiness.rs`,
//! one line, as M12-B left it.
//!
//! Nothing here opens a real endpoint. The only addresses bound are 127.0.0.1 on ephemeral
//! ports owned by this process; the stub serves reads only, and never serves anything in the
//! submission family. No key is read: every run that gets that far is stopped by the gate or
//! by a method the stub does not answer.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, U256};
use serde_json::{json, Value};

use evm_core::Fee;
use evm_execution::{ExecutionMode, ExecutionSetup, MarketKind, Tolerance, PRIVATE_KEY_ENV};
use evm_pipeline::arbitrage::{run_once, ArbitrageConfig, ArbitrageRun, RouteCandidate};
use evm_pipeline::config::RiskConfig;
use evm_pipeline::PipelineError;

const CHAIN_HEX: &str = "0x164ce";
/// The head the admitted control is served, so its first block read succeeds and the run's
/// stop lands one method further on — past the gate, which is the point.
const HEAD: u64 = 37_191_199;

/// What the stub answers `eth_syncing` with. §3 names five shapes for the check itself; the
/// chain crate's `readiness_gate.rs` holds the decoder's branches, and this file holds the
/// four that a *route run* has to refuse on: a progress object, a rejection, an illegal
/// shape, and silence. `Idle` is the fifth, and it is the control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sync {
    /// `false` — the node says it has no sync in progress. The admitted case.
    Idle,
    /// A progress object — behind, so held.
    Behind,
    /// A JSON-RPC error, which is terminal at the transport layer: one ask, no retry.
    Rejected,
    /// `true` with no progress object: legal JSON, illegal answer.
    Undecodable,
    /// The connection closes with no reply, so nothing arrives at all. Transport failures
    /// are retried once, which is the adapter's pre-existing behaviour and shows up as two
    /// asks in the endpoint's own tally.
    Silent,
}

impl Sync {
    /// Whether this shape costs the run one ask or two: only a transport failure is retried.
    const fn asks(self) -> usize {
        match self {
            Self::Silent => 2,
            _ => 1,
        }
    }

    /// §3's words for this answer, as the refusal has to carry them.
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

/// A stub node on an ephemeral localhost port, with the endpoint's own ordered tally of what
/// arrived — the same recipe `readiness_startup.rs` proves the bootstrap with, so the two
/// files count the same thing the same way.
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
    // `crates/pipeline` → the repository root, spelled without a `..` in it: the lane scan
    // reports every hit relative to this root, and a path built by joining `..` onto the
    // manifest directory would name the same file yet never compare equal to one the scan
    // produced.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("a crate sits two levels under the workspace root")
        .to_path_buf()
}

fn registries() -> Vec<PathBuf> {
    vec![
        workspace_root().join("data/protocols"),
        workspace_root().join("data/protocols-m3"),
    ]
}

/// The answer for one request, or `None` for a method this stub does not pretend to be.
///
/// Reads only, and fewer reads than the run wants: `eth_blockNumber` is answered so the
/// admitted control's stop lands *after* the gate rather than at the same call a held run
/// stops at, and everything past that — `eth_getBlockByNumber`, the reserves, the fee, the
/// nonce — is refused. A route run that reached the execution lane would ask for at least
/// one of those, so the tally being short of them is the witness that it did not.
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

/// Serve one connection: read the body, tally the method, answer it.
///
/// `connection: close` on every reply, as in the sibling file, so the client's pool and this
/// stub's threads do not have to agree about keep-alive for the tally to be complete. The
/// `Silent` shape closes without a byte of reply — that is the shape, not a shortcut.
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

/// One route run's config, on the stub's URL, with the mode named by the caller.
///
/// The candidate is a fixture: `ControlledFixture` says so in the record, and no run built
/// here gets far enough to price anything. The evidence directory is per test and emptied
/// first, so a directory left by an earlier build cannot be read as this run's record.
fn route(stub: &Stub, name: &str, mode: ExecutionMode) -> ArbitrageConfig {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    ArbitrageConfig {
        rpc_url: stub.url.clone(),
        registry_dirs: registries(),
        sender: Address::from_slice(&[0x11u8; 20]),
        sender_label: "a fixture sender; no key is read on a held run",
        candidate: RouteCandidate {
            chain_id: 91_342,
            input_token: Address::from_slice(&[0x11u8; 20]),
            mid_token: Address::from_slice(&[0x22u8; 20]),
            venues: [
                Address::from_slice(&[0x33u8; 20]),
                Address::from_slice(&[0x44u8; 20]),
            ],
            input_amount: U256::from(1_000_000_000_000_000u64),
            fee: Fee::new(3, 1000).expect("a fee of 3/1000 exists"),
            fee_evidence: "fixtures/live-m5/arbitrage-window".to_string(),
        },
        market: MarketKind::ControlledFixture {
            proves: "that a node the readiness gate refuses never reaches the lane".to_string(),
        },
        setup: ExecutionSetup {
            mode,
            ..ExecutionSetup::default()
        },
        tolerance: Tolerance::new(1, 100),
        risk: RiskConfig {
            minimum_net_profit_wei: 0,
            maximum_gas: None,
        },
        evidence_dir: dir,
        latency_dir: None,
        diagnosis_dir: None,
        state_read_reuse: true,
        state_read_concurrency: 1,
        state_acquisition_diagnosis: false,
        storage_dependency_diagnosis: false,
        cross_stage_diagnosis: false,
    }
}

fn evidence_dir(name: &str) -> PathBuf {
    workspace_root().join("target/pipeline-tests").join(name)
}

/// The error a held run has to return.
///
/// `expect_err` would need [`ArbitrageRun`] to be `Debug`, and it is a run's whole record —
/// headers, legs, simulation, plan, report — with the trait never derived for it. A run that
/// completed when the gate should have held it is reported by its own identity and its own
/// directory, which is more useful than a debug dump of the record either way.
fn refusal(outcome: Result<ArbitrageRun, PipelineError>) -> PipelineError {
    match outcome {
        Err(error) => error,
        Ok(run) => panic!(
            "a run the gate should have held completed instead: {} wrote {}",
            run.session_id,
            run.evidence_dir.display()
        ),
    }
}

/// §3's first proof, on the entry that actually spends: the node says it is still syncing,
/// and the route stops at the gate — before the head read, before the pin, and therefore
/// before `SequenceStage`, which is the only thing further down that builds a `Signer`.
#[tokio::test]
async fn a_syncing_node_holds_the_route_before_it_reads_a_block() {
    let stub = Stub::spawn(Sync::Behind);
    let config = route(&stub, "isolation-syncing", ExecutionMode::BuildOnly);
    let error = refusal(run_once(&config).await);
    let PipelineError::NodeNotReady { endpoint, detail } = &error else {
        panic!("a syncing node has to be refused as not ready, got {error}");
    };
    assert_eq!(
        endpoint, &stub.url,
        "the refusal names the endpoint it asked"
    );
    assert!(detail.contains(Sync::Behind.detail()), "{detail}");

    // The endpoint's own tally: the connect and the gate, and nothing after them.
    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "a held route gets no further than the gate"
    );
    for banned in [
        "eth_getBlockByNumber",
        "eth_call",
        "eth_getBalance",
        "eth_getTransactionCount",
        "eth_gasPrice",
        "eth_maxPriorityFeePerGas",
    ] {
        assert_eq!(
            stub.count(banned),
            0,
            "{banned} is a read the pricing path or the lane makes, and a held run makes neither"
        );
    }
    assert!(
        !evidence_dir("isolation-syncing").exists(),
        "a held run opens no session directory: {}",
        evidence_dir("isolation-syncing").display()
    );
}

/// §3's 「未就绪时，Signer 不被调用」, in the form that can only fail: the run is configured
/// for a mode that *must* have a key, and no key is in the environment. Without the gate the
/// next thing this run does after the head read is build a signer, and the error would be the
/// signer's own complaint; the error that arrives is readiness's, and it carries the node's
/// answer rather than a key story.
#[tokio::test]
async fn a_held_route_stops_with_readiness_rather_than_with_a_key_problem() {
    let key_present = std::env::var(PRIVATE_KEY_ENV).is_ok();
    let stub = Stub::spawn(Sync::Behind);
    let config = route(&stub, "isolation-sign-only", ExecutionMode::SignOnly);
    let error = refusal(run_once(&config).await);
    let PipelineError::NodeNotReady { detail, .. } = &error else {
        panic!("the gate holds this run, so nothing downstream decides its outcome: {error}");
    };
    assert!(detail.contains(Sync::Behind.detail()), "{detail}");
    assert_eq!(stub.count("eth_syncing"), 1);
    assert_eq!(
        stub.count("eth_blockNumber"),
        0,
        "the head read the pin is built from sits behind the gate"
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
        !evidence_dir("isolation-sign-only").exists(),
        "and nothing was written about a run that never started"
    );
}

/// §3's 「RPC/WS 断连与重连」 and 「`eth_syncing` 返回错误、超时、无效响应」 at this entry:
/// every answer that is not one of the two legal shapes is a hold, in the node's own words.
/// The budget is named per case because the three refusals cost different numbers of asks —
/// a JSON-RPC error and an illegal shape are answers, so they are not retried, while a
/// connection that dies is the transport failure the adapter retries once (behaviour this
/// milestone did not touch).
#[tokio::test]
async fn a_node_that_will_not_or_cannot_answer_is_not_ready() {
    for (case, sync) in [
        ("rejected", Sync::Rejected),
        ("undecodable", Sync::Undecodable),
        ("silent", Sync::Silent),
    ] {
        let stub = Stub::spawn(sync);
        let name = format!("isolation-{case}");
        let config = route(&stub, &name, ExecutionMode::BuildOnly);
        let error = refusal(run_once(&config).await);
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
            "{case}: the gate asks once per run, and only a transport failure is retried"
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

/// §3's 「readiness 检查失败不会被转换成空候选或零机会」, and the control that makes it
/// mean something: a held run is an error out of `run_once`, while a run the gate admits
/// keeps going and stops later, on a different error, having asked the node for a block.
/// Both branches use the same stub family and the same candidate, so the only variable is
/// the readiness answer.
#[tokio::test]
async fn a_refusal_is_an_error_and_not_a_route_that_found_nothing() {
    let held = Stub::spawn(Sync::Behind);
    let held_config = route(&held, "isolation-refused", ExecutionMode::BuildOnly);
    let held_outcome = run_once(&held_config).await;
    assert!(
        matches!(held_outcome, Err(PipelineError::NodeNotReady { .. })),
        "a held run has to be an error, not a record: {:?}",
        held_outcome.map(|_| "Ok(run)")
    );

    let admitted = Stub::spawn(Sync::Idle);
    let admitted_config = route(&admitted, "isolation-admitted", ExecutionMode::BuildOnly);
    let admitted_outcome = run_once(&admitted_config).await;
    let error = match &admitted_outcome {
        Err(error) => error,
        Ok(run) => panic!(
            "this stub serves no reserves, so the run cannot complete: {} wrote {}",
            run.session_id,
            run.evidence_dir.display()
        ),
    };
    assert!(
        !matches!(error, PipelineError::NodeNotReady { .. }),
        "the ready node was not held: {error}"
    );
    // The distinction is only real if the two outcomes differ in *where* they stopped, and
    // the endpoint says they did: the admitted run asked for a block, the held one did not.
    assert_eq!(
        admitted.count("eth_blockNumber"),
        1,
        "past the gate, the run read its head: {:?}",
        admitted.received()
    );
    assert_eq!(held.count("eth_blockNumber"), 0);
    assert_eq!(
        admitted.count("eth_syncing"),
        1,
        "one ask for the entry, and the ask is not per candidate"
    );
}

/// §3's 「不会静默回退到未经授权的公共 RPC」, witnessed rather than asserted: a second,
/// perfectly ready endpoint is bound and left unreferenced. The run talks to the one it was
/// configured with — and the refusal names that one, so an operator reading the error can
/// tell which endpoint was refused.
#[tokio::test]
async fn a_held_route_contacts_no_other_endpoint() {
    let configured = Stub::spawn(Sync::Behind);
    let bystander = Stub::spawn(Sync::Idle);
    let config = route(
        &configured,
        "isolation-no-fallback",
        ExecutionMode::BuildOnly,
    );
    let error = refusal(run_once(&config).await);
    let PipelineError::NodeNotReady { endpoint, .. } = &error else {
        panic!("got {error}");
    };
    assert_eq!(endpoint, &configured.url);
    assert_eq!(
        bystander.received(),
        Vec::<String>::new(),
        "an endpoint this run was never told about received nothing — there is no fallback \
         path to fall back on"
    );
    assert_eq!(
        configured.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()]
    );
}

/// §3's 「readiness 恢复后的重新放行」 at this entry: the node that was behind is now caught
/// up, and the *next* run is admitted — by asking again for itself, not by inheriting the
/// earlier verdict. The two runs are two processes' worth of state in one test, which is
/// exactly the recovery shape this repository has: one defined recovery point per process,
/// the next run.
#[tokio::test]
async fn a_route_run_after_recovery_asks_again_and_is_admitted() {
    let behind = Stub::spawn(Sync::Behind);
    let first = route(&behind, "isolation-retry-held", ExecutionMode::BuildOnly);
    assert!(matches!(
        run_once(&first).await,
        Err(PipelineError::NodeNotReady { .. })
    ));

    let caught_up = Stub::spawn(Sync::Idle);
    let second = route(
        &caught_up,
        "isolation-retry-admitted",
        ExecutionMode::BuildOnly,
    );
    let outcome = run_once(&second).await;
    if let Err(error) = &outcome {
        assert!(
            !matches!(error, PipelineError::NodeNotReady { .. }),
            "the recovered node answered `false`, so this run is not a readiness refusal: \
             {error}"
        );
    }
    assert_eq!(
        caught_up.count("eth_syncing"),
        1,
        "the recovered run made its own ask: {:?}",
        caught_up.received()
    );
    let methods = caught_up.received();
    let sync_at = methods
        .iter()
        .position(|m| m == "eth_syncing")
        .expect("the gate asked");
    let block_at = methods
        .iter()
        .position(|m| m == "eth_blockNumber")
        .expect("the admitted run read its head");
    assert!(
        sync_at < block_at,
        "the ask sits ahead of the head read: {methods:?}"
    );
    assert!(
        !evidence_dir("isolation-retry-admitted").exists(),
        "and it still stopped before opening a session, because this stub serves no reserves \
         — which is what §3 asks of the *next* read, not of the gate"
    );
}

// ---------------------------------------------------------------------------
// The two structural gates.
// ---------------------------------------------------------------------------

/// The production region of one source file: everything above `#[cfg(test)]`, as lines.
///
/// The same cut the M12-B scans use, for the same reason — a test that builds a lane to
/// exercise it is not a run that builds one, and a scan that counted test scaffolding would
/// have to allow it, and an allowance is how a real ungated entry starts passing.
fn production_lines(rel: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(rel).unwrap_or_else(|error| panic!("{rel:?}: {error}"));
    let production = match text.find("\n#[cfg(test)]") {
        Some(at) => &text[..at],
        None => &text[..],
    };
    production.lines().map(str::to_string).collect()
}

/// Drop the comment lines: a doc sentence that *names* a constructor is not a call of it.
/// Without this, `runner.rs`'s own explanation of the rule would satisfy the rule.
fn code_only(lines: &[String]) -> Vec<(usize, String)> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(n, line)| (n + 1, line.clone()))
        .collect()
}

/// Every production call of a lane constructor, and how the gate sits above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gating {
    /// A gate call in the same function, on the line named here.
    SameFunction(usize),
    /// A call in the same function, on the line named here, to another top-level function of
    /// the same file that gates before it returns — `run`'s arm, where the gate lives in
    /// `build_canonical`.
    ThroughHelper(usize),
    /// Nothing above this line gates. The scan reports it rather than assuming it away, and
    /// the test fails on it.
    Ungated,
}

/// Every `crates/*/src/**/*.rs` file, sorted — production only, since `src` never holds an
/// integration test and this file's own text is full of the needles below.
fn production_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for package in std::fs::read_dir(workspace_root().join("crates")).expect("the crates dir") {
        let src = package.expect("a package").path().join("src");
        if src.is_dir() {
            walk(&src, &mut out);
        }
    }
    out.sort();
    assert!(out.len() > 50, "the walk found {} sources", out.len());
    out
}

/// The line span of each top-level function, as `(name, start, end)`, where `end` is the next
/// top-level `fn` line rather than a brace-matched close.
///
/// The three entries this scan is about all start at column 0 — `run`, `run_once`,
/// `run_validation`, `build_canonical` — and the asymmetry is stated rather than hidden: a
/// lane constructor that sits in an indented method *after* a top-level function's start is
/// credited that function's region, so [`classify`] can only ever be asked about a line it
/// can name, and the test prints the pairing it concluded from.
fn fn_regions(lines: &[(usize, String)]) -> Vec<(String, usize, usize)> {
    let starts: Vec<(String, usize)> = lines
        .iter()
        .filter(|(_, line)| !line.starts_with([' ', '\t']) && line.contains("fn "))
        .map(|(n, line)| {
            let after = line[line.find("fn ").expect("the keyword") + 3..]
                .split(['(', '<'])
                .next()
                .expect("a name")
                .trim_end()
                .to_string();
            (after, *n)
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, (name, start))| {
            let end = starts.get(i + 1).map(|(_, n)| *n).unwrap_or(usize::MAX);
            (name.clone(), *start, end)
        })
        .collect()
}

/// Lines that run the gate. The open paren excludes a `use` item naming the function;
/// excluding `fn ` excludes its definition, which gates nothing.
fn gate_lines(lines: &[(usize, String)]) -> Vec<usize> {
    lines
        .iter()
        .filter(|(_, line)| line.contains("gate_readiness(") && !line.contains("fn gate_readiness"))
        .map(|(n, _)| *n)
        .collect()
}

/// How the gate sits above one constructor line, per [`Gating`].
fn classify(
    lines: &[(usize, String)],
    regions: &[(String, usize, usize)],
    gates: &[usize],
    ctor: usize,
) -> Gating {
    let Some((name, start, end)) = regions.iter().find(|(_, s, e)| *s < ctor && ctor < *e) else {
        return Gating::Ungated;
    };
    let (name, start, end) = (name.as_str(), *start, *end);
    if let Some(at) = gates.iter().find(|g| **g > start && **g < ctor) {
        return Gating::SameFunction(*at);
    }
    // The helper arm, in this file only: a function that gates inside itself, called here
    // before the lane is built. `build_canonical` is that function for `run`.
    for (helper, hstart, hend) in regions.iter().filter(|(h, _, _)| h != name) {
        let (hstart, hend) = (*hstart, *hend);
        if !gates.iter().any(|g| *g > hstart && *g < hend) {
            continue;
        }
        let needle = format!("{helper}(");
        if let Some((at, _)) = lines.iter().find(|(n, line)| {
            *n > start && *n < end && *n < ctor && *n != hstart && line.contains(&needle)
        }) {
            return Gating::ThroughHelper(*at);
        }
    }
    Gating::Ungated
}

/// The two stage constructors that can sign or submit, as production lines of any file.
fn lane_sites(file: &Path) -> Vec<(usize, String, Gating)> {
    let lines = code_only(&production_lines(file));
    let regions = fn_regions(&lines);
    let gates = gate_lines(&lines);
    lines
        .iter()
        .filter(|(_, line)| {
            line.contains("ExecutionStage::connect") || line.contains("SequenceStage::connect")
        })
        .map(|(n, line)| {
            (
                *n,
                line.trim().to_string(),
                classify(&lines, &regions, &gates, *n),
            )
        })
        .collect()
}

/// §3's invariant 1 as a property of the repository rather than of one test: every place a
/// signing or submitting stage is built in production code has a readiness gate above it, and
/// the only things that build a `Signer` are those two stages.
///
/// The behavioural tests above prove this for the runs they start. This proves it for the
/// entries nobody started — including an entry added next week, which is the case a
/// behavioural test cannot reach and the reason the audit found the two spending entries
/// ungated in the first place.
#[test]
fn every_production_entry_that_builds_a_lane_passes_the_gate_above_it() {
    let sites: Vec<(PathBuf, usize, String, Gating)> = production_sources()
        .iter()
        .flat_map(|file| {
            lane_sites(file)
                .into_iter()
                .map(move |(line, code, gating)| (file.clone(), line, code, gating))
        })
        .collect();

    // The scan's own positive control, before its verdict: the three entries this milestone
    // is about have to turn up. A walk that read nothing would satisfy `all` vacuously, and a
    // vacuous pass is how an ungated entry is born.
    for (rel, ctor) in [
        ("crates/pipeline/src/runner.rs", "ExecutionStage::connect"),
        ("crates/pipeline/src/arbitrage.rs", "SequenceStage::connect"),
        ("crates/cli/src/lib.rs", "ExecutionStage::connect"),
    ] {
        let want = workspace_root().join(rel);
        assert!(
            sites
                .iter()
                .any(|(f, _, code, _)| *f == want && code.contains(ctor)),
            "{rel} builds a lane and the scan does not see it: {sites:?}"
        );
    }

    let ungated: Vec<String> = sites
        .iter()
        .filter(|(_, _, _, gating)| matches!(gating, Gating::Ungated))
        .map(|(f, line, code, _)| format!("{}:{line}: {code}", f.display()))
        .collect();
    assert!(
        ungated.is_empty(),
        "a production entry reaches a signing or submitting stage with no readiness gate \
         above it:\n{}",
        ungated.join("\n")
    );
    // Each pass is stated with the line it rests on, so a reader can check the ordering in
    // the source rather than trust the verdict.
    for (file, line, code, gating) in &sites {
        let rel = file
            .strip_prefix(workspace_root())
            .unwrap_or(file.as_path());
        match gating {
            Gating::SameFunction(at) => println!("{rel:?}:{line} gated at :{at} — {code}"),
            Gating::ThroughHelper(at) => {
                println!("{rel:?}:{line} gated through a helper called at :{at} — {code}")
            }
            Gating::Ungated => unreachable!("the assertion above rejects this"),
        }
    }

    // The other half of the closure: a `Signer` is constructed in exactly the two stage
    // `connect`s, in the execution crate, and nowhere else — so gating the call sites of
    // those `connect`s is gating every way a key can be read.
    let signer_sites: Vec<(PathBuf, usize)> = production_sources()
        .iter()
        .flat_map(|file| {
            code_only(&production_lines(file))
                .into_iter()
                .filter(|(_, line)| line.contains("Signer::from_env("))
                .map(move |(line, _)| (file.clone(), line))
        })
        .collect();
    let expected_signer_sites: Vec<&str> = vec![
        "crates/execution/src/sequence.rs",
        "crates/execution/src/stage.rs",
    ];
    let mut seen: Vec<String> = signer_sites
        .iter()
        .map(|(f, _)| {
            f.strip_prefix(workspace_root())
                .unwrap_or(f.as_path())
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen, expected_signer_sites,
        "a third production place that reads a signing key would put this milestone's claim \
         of isolation in doubt"
    );
}

/// The same classifier, run on lines that do not come from the repository: an ungated lane
/// has to read as ungated, and the helper arm has to need the call.
#[test]
fn the_lane_scan_reports_an_ungated_entry_instead_of_passing_it() {
    let lines: Vec<(usize, String)> = [
        "async fn opener() {",
        "    gate_readiness(&policy, &http, url, None).await?;",
        "}",
        "pub async fn entry() {",
        "    let stage = ExecutionStage::connect(url).await?;",
        "}",
    ]
    .iter()
    .map(|line| line.to_string())
    .enumerate()
    .map(|(i, line)| (i + 1, line))
    .collect();
    let regions = fn_regions(&lines);
    let gates = gate_lines(&lines);
    // The lane in `entry`, whose own region has no gate and calls no gated helper.
    assert_eq!(classify(&lines, &regions, &gates, 5), Gating::Ungated);
    // The identical shape, with the helper call added — the arm `run` is proved by. Without
    // this second half the assertion above would only show that a scan which never looks for
    // a helper reports everything as ungated.
    let with_helper: Vec<(usize, String)> = [
        "async fn opener() {",
        "    gate_readiness(&policy, &http, url, None).await?;",
        "}",
        "pub async fn entry() {",
        "    opener().await?;",
        "    let stage = ExecutionStage::connect(url).await?;",
        "}",
    ]
    .iter()
    .map(|line| line.to_string())
    .enumerate()
    .map(|(i, line)| (i + 1, line))
    .collect();
    assert_eq!(
        classify(
            &with_helper,
            &fn_regions(&with_helper),
            &gate_lines(&with_helper),
            6
        ),
        Gating::ThroughHelper(5)
    );
    // And the direct arm, so `SameFunction` is a real branch rather than the only one the
    // repository happens to exercise.
    let direct: Vec<(usize, String)> = [
        "pub async fn entry() {",
        "    gate_readiness(&policy, &http, url, None).await?;",
        "    let stage = ExecutionStage::connect(url).await?;",
        "}",
    ]
    .iter()
    .map(|line| line.to_string())
    .enumerate()
    .map(|(i, line)| (i + 1, line))
    .collect();
    assert_eq!(
        classify(&direct, &fn_regions(&direct), &gate_lines(&direct), 3),
        Gating::SameFunction(2)
    );
}

// ---------------------------------------------------------------------------
// The RPC-budget and one-gate counts, taken from the source rather than promised.
// ---------------------------------------------------------------------------

/// Every production line of the workspace containing `needle`, as `relative/path:line`.
fn production_hits(needle: &str) -> Vec<String> {
    production_sources()
        .iter()
        .flat_map(|file| {
            code_only(&production_lines(file))
                .into_iter()
                .filter(move |(_, line)| line.contains(needle))
                .map(move |(line, _)| {
                    let rel = file
                        .strip_prefix(workspace_root())
                        .unwrap_or(file.as_path());
                    format!("{}:{line}", rel.to_string_lossy().replace('\\', "/"))
                })
        })
        .collect()
}

/// §3's 「不新增 RPC 方法」 and the one-gate rule M12-B §4 set, counted at the source: the
/// ask is one line in `readiness.rs`, the readiness read has one production caller, and that
/// caller is one function. Nothing here added a method to the node's budget or a second
/// place that decides whether a node may be used.
#[test]
fn the_fix_adds_no_rpc_method_and_no_second_gate() {
    // The method-sending site. One, in the chain crate's readiness module, as M12-B left it:
    // this milestone moved the *callers* of the gate, not the ask.
    let asks = production_hits("request_raw(\"eth_syncing\"");
    let ask_files: Vec<String> = asks
        .iter()
        .map(|hit| {
            hit.rsplit_once(':')
                .map(|(file, _)| file.to_string())
                .unwrap_or_else(|| hit.clone())
        })
        .collect();
    assert_eq!(
        ask_files,
        vec!["crates/chain/src/readiness.rs".to_string()],
        "the ask has to stay where M12-B put it — one site, in one file, and that file is the \
         readiness module: {asks:?}"
    );

    // The readiness read's own production callers: the gate in this file's terms, one line.
    let readers: Vec<String> = production_hits("syncing_status()")
        .into_iter()
        .filter(|hit| !hit.contains("pub async fn"))
        .collect();
    assert_eq!(
        readers.len(),
        1,
        "exactly one production place asks the node whether it is ready, and it is inside the \
         gate: {readers:?}"
    );
    assert!(
        readers[0].starts_with("crates/chain/src/readiness.rs"),
        "the ask is made by the readiness module, not by an entry: {readers:?}"
    );

    // One gate, by definition count and by the number of functions named for it.
    let gates = production_hits("fn gate_readiness");
    assert_eq!(gates.len(), 1, "no second gate: {gates:?}");
    assert!(
        gates[0].starts_with("crates/pipeline/src/runner.rs"),
        "{gates:?}"
    );

    // The planted control, so the counter above is a counter and not a fixed verdict: the
    // same needle in a second place has to show as a second hit.
    let planted = [
        "1: let a = adapter.request_raw(\"eth_syncing\", json!([])).await?;".to_string(),
        "2: let b = adapter.request_raw(\"eth_syncing\", json!([])).await?;".to_string(),
    ];
    let counted: Vec<&String> = planted
        .iter()
        .filter(|line| line.contains("request_raw(\"eth_syncing\""))
        .collect();
    assert_eq!(counted.len(), 2, "the counter counts what it is shown");
    // And the shape it must reject: an ask written without the method name attached to the
    // send is not an ask, which is why the needle is the call and not the word.
    let not_an_ask = ["1: // the eth_syncing answer is judged in readiness.rs".to_string()];
    assert!(
        not_an_ask
            .iter()
            .all(|line| !line.contains("request_raw(\"eth_syncing\"")),
        "a comment naming the method is not a request for it"
    );
}
