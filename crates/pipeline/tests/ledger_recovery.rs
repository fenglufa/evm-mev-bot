//! M12-F §7 and §8, at the entries rather than inside the lane.
//!
//! `crates/execution/tests/crash_recovery.rs` proves the nine crash shapes (§8's F1–F9) against
//! the stage and the sequence executor with an in-process scripted endpoint. That proves what a
//! lane does with a ledger; it does not prove that a *process start* goes through one. §7 says
//! the recovery has to complete before any entry that may sign or send, and says the standard is
//! an audit of the real code rather than one call added to the CLI's main entry — so this file
//! has two halves:
//!
//! * the behavioural half runs the route entry ([`run_once`]) against a localhost stub node over a
//!   ledger directory the test controls, and shows that a damaged ledger is an error out of that
//!   entry with no submission request ever reaching the socket (§4.1's 「测试必须证明 HTTP
//!   发送次数为 0」), that the entry is what opens and reads the file, and that two starts over
//!   one file leave the same bytes (§8's F7);
//! * the structural half is the audit §7 asks for: every production line that builds a signing or
//!   submitting lane, and the ledger read that has to sit above it, found by walking
//!   `crates/*/src` rather than by naming files that happen to be remembered.
//!
//! The stub is the same family `readiness_isolation.rs` and `readiness_startup.rs` use — an
//! ephemeral localhost port with the endpoint's own ordered tally of methods — so the three files
//! count requests the same way. It answers `eth_chainId` and `eth_syncing` and nothing else, which
//! is exactly enough for a route run to reach the ledger read: §7 puts that read ahead of the
//! pricing reads and ahead of the lane, so a run that stops on an unserved read has still proved
//! where the ledger sits.
//!
//! No real network, no signing key, no submission. The mode is `BuildOnly` throughout, and the
//! method the ledger exists to protect — `eth_sendRawTransaction` — is counted, never answered.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, B256, U256};
use serde_json::{json, Value};

use evm_core::Fee;
use evm_execution::{
    journal_file_name, ExecutionJournal, ExecutionMode, ExecutionSetup, JournalFact, JournalRecord,
    MarketKind, Tolerance,
};
use evm_pipeline::arbitrage::{run_once, ArbitrageConfig, RouteCandidate};
use evm_pipeline::config::RiskConfig;
use evm_pipeline::PipelineError;

const CHAIN: u64 = 91_342;
const CHAIN_HEX: &str = "0x164ce";
const HEAD: u64 = 37_191_199;
/// The sender the fixture route names. A damage row uses it in its own record so the file reads
/// like a ledger this entry wrote, not like a ledger about somebody else.
const SENDER: [u8; 20] = [0x11u8; 20];

// ---------------------------------------------------------------------------
// The stub node.
// ---------------------------------------------------------------------------

/// A stub node on an ephemeral localhost port, tallying the methods it was asked for in arrival
/// order. Reads it does not pretend to serve get a JSON-RPC error, so a run that wanted one had
/// to stop and say so rather than invent an answer.
struct Stub {
    url: String,
    received: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a localhost port");
        let port = listener.local_addr().expect("an address").port();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let received = received.clone();
            move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let received = received.clone();
                    std::thread::spawn(move || serve(stream, received));
                }
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            received,
        }
    }

    fn count(&self, method: &str) -> usize {
        self.received
            .lock()
            .expect("the tally")
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }

    fn received(&self) -> Vec<String> {
        self.received.lock().expect("the tally").clone()
    }
}

fn answer(request: &Value) -> Option<Value> {
    let method = request["method"].as_str()?;
    let result = match method {
        "eth_chainId" => json!(CHAIN_HEX),
        "eth_syncing" => json!(false),
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
}

/// Serve one connection, replying `connection: close` so the client's pool and this stub's
/// threads need not agree about keep-alive for the tally to be complete.
fn serve(mut stream: std::net::TcpStream, received: Arc<Mutex<Vec<String>>>) {
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
                let reply = match answer(&request) {
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

// ---------------------------------------------------------------------------
// The scratch ledger and the one route run.
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
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

/// The ledger directory for one test, emptied first. A sibling of the evidence directory rather
/// than inside it, for the reason `readiness_isolation.rs` gives: opening a journal makes its
/// directory, and a nested path would turn 「this run wrote a session record」 into 「this run
/// wrote a file」.
fn ledger_dir(name: &str) -> PathBuf {
    let dir = workspace_root()
        .join("target/pipeline-tests")
        .join(format!("{name}-ledger"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn journal_path(dir: &Path) -> PathBuf {
    dir.join(journal_file_name(CHAIN))
}

fn read_ledger(dir: &Path) -> String {
    std::fs::read_to_string(journal_path(dir)).unwrap_or_else(|error| {
        panic!(
            "{}: the ledger this run was configured with is not readable: {error}",
            journal_path(dir).display()
        )
    })
}

fn write_ledger(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir).expect("the ledger directory");
    std::fs::write(journal_path(dir), text).expect("the damaged ledger");
}

/// A ledger a real handle wrote: the opening line plus one record line, returned as text so a
/// damage row can edit what a crash could actually have left rather than an invented shape.
fn seeded_ledger(dir: &Path) -> String {
    {
        let mut journal = ExecutionJournal::open(dir, CHAIN, 1_000)
            .expect("a fresh ledger opens on a scratch dir");
        journal
            .append(
                1_001,
                JournalRecord {
                    fact: JournalFact::SendDispatched,
                    chain_id: CHAIN,
                    execution_id: "exec-entry".to_string(),
                    idempotency_key: "key-entry".to_string(),
                    opportunity_id: "opp-entry".to_string(),
                    sender: Address::from_slice(&SENDER),
                    target: Address::from_slice(&[0x22u8; 20]),
                    nonce: Some(7),
                    transaction_hash: Some(B256::with_last_byte(3)),
                    pinned_block: Some(HEAD),
                    pinned_block_hash: Some(B256::with_last_byte(4)),
                    endpoint_class: None,
                    position: None,
                    status: None,
                    detail: String::new(),
                },
            )
            .expect("the record line appends");
    }
    read_ledger(dir)
}

/// One route run's config on the stub, in `BuildOnly`, with the ledger directory the test owns.
///
/// `BuildOnly` matters twice over: no key is read, so a run that got as far as a lane would still
/// not sign, and the §4.1 boundary this file is about is the *request* count at the socket, which
/// a mode gate cannot hide.
fn route(stub: &Stub, name: &str, ledger: PathBuf) -> ArbitrageConfig {
    ArbitrageConfig {
        rpc_url: stub.url.clone(),
        registry_dirs: registries(),
        sender: Address::from_slice(&SENDER),
        sender_label: "a fixture sender; no key is read in build-only",
        candidate: RouteCandidate {
            chain_id: CHAIN,
            input_token: Address::from_slice(&SENDER),
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
            proves: "that a ledger the entry cannot read stops the run before the lane".to_string(),
        },
        setup: ExecutionSetup {
            mode: ExecutionMode::BuildOnly,
            ..ExecutionSetup::default()
        },
        tolerance: Tolerance::new(1, 100),
        risk: RiskConfig {
            minimum_net_profit_wei: 0,
            maximum_gas: None,
        },
        ledger_dir: ledger,
        evidence_dir: workspace_root()
            .join("target/pipeline-tests")
            .join(format!("{name}-evidence")),
        latency_dir: None,
        diagnosis_dir: None,
        state_read_reuse: true,
        state_read_concurrency: 1,
        state_acquisition_diagnosis: false,
        storage_dependency_diagnosis: false,
        cross_stage_diagnosis: false,
    }
}

/// The error a refused run has to return, with the §6 requirement that the refusal names the
/// damage: [`PipelineError::Execution`] is the arm `run_once` returns for a ledger it will not
/// read, and the fault word has to be in its text rather than inferred from which arm fired.
fn ledger_refusal(error: PipelineError) -> String {
    let PipelineError::Execution(detail) = error else {
        panic!("an unreadable ledger has to refuse as a ledger error, got {error}");
    };
    detail
}

// ---------------------------------------------------------------------------
// The behavioural half: §8's F6, F8 and F7 at the entry.
// ---------------------------------------------------------------------------

/// One row of the damage table: a tag, the damage applied to a healthy ledger's text, and the
/// fault word §6 requires in the refusal.
type Damage = (&'static str, fn(&str) -> String, &'static str);

/// §8's F6 and §6, seen from the entry: four damages a crash or an edit can actually leave, each
/// applied to a ledger a real handle wrote. The route run refuses all four as an error out of
/// `run_once`, the socket is never asked to send, and the file is left exactly as it was found —
/// neither repaired nor cleared (§7's 「不得清空台账后继续运行」).
#[tokio::test]
async fn a_damaged_ledger_refuses_the_route_run_and_the_run_leaves_the_bytes_alone() {
    let rows: [Damage; 4] = [
        (
            "torn",
            |text: &str| -> String {
                // The last line cut before its checksum and its newline: what a process killed
                // mid-write leaves, and the shape §6 says must never be skipped as noise.
                let (genesis, last) = text.split_once('\n').expect("two lines");
                let last = last.trim_end_matches('\n');
                let cut = last.len() - 74;
                assert!(
                    cut > 0 && last.is_char_boundary(cut),
                    "the cut is inside the line"
                );
                format!("{}\n{}", genesis, &last[..cut])
            },
            "torn_tail",
        ),
        (
            "edited",
            |text: &str| -> String {
                // A field of a written line changed after the fact: the record's own detail,
                // which is the one field an operator might reasonably think is free to edit.
                let forged = text.replace("\"detail\":\"\"", "\"detail\":\"settled\"");
                assert_ne!(forged, text, "the edit has to change a byte");
                forged
            },
            "checksum_mismatch",
        ),
        (
            "schema",
            |text: &str| -> String {
                let genesis = text.lines().next().expect("the opening line");
                let rest = &text[genesis.len() + 1..];
                let edited = genesis.replace("\"schema_version\":1", "\"schema_version\":99");
                assert_ne!(edited, genesis, "the edit has to change a byte");
                format!("{edited}\n{rest}")
            },
            "unsupported_schema",
        ),
        (
            "fact",
            |text: &str| -> String {
                let edited = text.replace(
                    "\"fact\":\"send_dispatched\"",
                    "\"fact\":\"nonce_released_by_timeout\"",
                );
                assert_ne!(edited, text, "the edit has to change a byte");
                edited
            },
            "unknown_fact",
        ),
    ];

    for (tag, damage, fault) in rows {
        let name = format!("m12f-entry-{tag}");
        let ledger = ledger_dir(&name);
        let healthy = seeded_ledger(&ledger);
        let damaged = damage(&healthy);
        write_ledger(&ledger, &damaged);

        let stub = Stub::spawn();
        let outcome = run_once(&route(&stub, &name, ledger.clone())).await;
        // `ArbitrageRun` is not `Debug`, so the arm is matched rather than unwrapped — and a run
        // that completed is reported as what it is: the entry did not refuse the damage.
        let error = match outcome {
            Err(error) => error,
            Ok(_) => panic!("{tag}: a damaged ledger let the route run finish (§6)"),
        };
        let detail = ledger_refusal(error);
        assert!(
            detail.contains(fault),
            "{tag}: the refusal has to name the damage it found (§6), got {detail}"
        );

        // §4.1's count, taken at the socket rather than from a return value.
        assert_eq!(
            stub.count("eth_sendRawTransaction"),
            0,
            "{tag}: a run that refused its ledger sent nothing: {:?}",
            stub.received()
        );
        // The file is what the refusal said it was: not rewritten, not truncated to fix it, not
        // cleared so the next start could begin fresh (§6's fail-closed and §7's prohibition on
        // emptying the ledger to keep trading).
        assert_eq!(
            read_ledger(&ledger),
            damaged,
            "{tag}: a refused ledger is left as its own evidence"
        );
        assert!(
            journal_path(&ledger).exists(),
            "{tag}: the refused file is still there"
        );
    }
}

/// §7's steps 1–2 wired at the entry rather than only inside the crate: the route run opens and
/// reads the directory it was *configured* with, and the line it writes there names the schema and
/// the chain before any read of the node's pricing or lane work. The run then stops on a read this
/// stub does not serve — which is the point: the ledger line exists while the lane does not.
#[tokio::test]
async fn a_route_run_opens_the_ledger_it_was_configured_with() {
    let name = "m12f-entry-opens";
    let ledger = ledger_dir(name);
    assert!(!ledger.exists(), "the run makes its own ledger directory");

    let stub = Stub::spawn();
    let outcome = run_once(&route(&stub, name, ledger.clone())).await;
    // Either way, the stop is not a ledger stop: this is the shape where recovery succeeded and
    // the run went on to ask for something the stub refuses.
    if let Err(error) = &outcome {
        assert!(
            !matches!(error, PipelineError::Execution(_)),
            "a healthy ledger must not be the reason this run stopped: {error}"
        );
    }

    let text = read_ledger(&ledger);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "one line, the opening one: {text}");
    assert!(
        lines[0].contains("\"schema_version\":1"),
        "the line names the schema version it writes: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("\"fact\":\"journal_opened\""),
        "and it names itself as the opening: {}",
        lines[0]
    );
    assert!(
        lines[0].contains(&format!("\"chain_id\":{CHAIN}")),
        "and the chain this file is for, so a second chain's file cannot be confused with it: {}",
        lines[0]
    );
    assert_eq!(stub.count("eth_sendRawTransaction"), 0);
}

/// §8's F7 at the entry: the same ledger directory, two process starts. The second start reads
/// what the first wrote, adds no line of its own, and reports the same recovery — and neither
/// start asked the node to send anything. A restart that re-wrote its opening line, or that
/// counted one recovered hold twice, would show up here as different bytes rather than as a
/// promise.
#[tokio::test]
async fn two_route_starts_on_one_ledger_leave_the_same_file() {
    let name = "m12f-entry-twice";
    let ledger = ledger_dir(name);

    let first = Stub::spawn();
    let _ = run_once(&route(&first, name, ledger.clone())).await;
    let after_first = read_ledger(&ledger);
    let recovered_first = ExecutionJournal::reload(&ledger, CHAIN)
        .expect("a healthy ledger reloads")
        .summary();

    let second = Stub::spawn();
    let _ = run_once(&route(&second, name, ledger.clone())).await;
    let after_second = read_ledger(&ledger);
    let recovered_second = ExecutionJournal::reload(&ledger, CHAIN)
        .expect("the same ledger still reloads")
        .summary();

    assert_eq!(
        after_first, after_second,
        "a second start on one ledger writes nothing the first did not already write"
    );
    assert_eq!(
        recovered_first, recovered_second,
        "and it recovers the same answer twice: {recovered_first} vs {recovered_second}"
    );
    assert_eq!(
        after_second.lines().count(),
        1,
        "the opening line is not duplicated by a restart: {after_second}"
    );
    for stub in [&first, &second] {
        assert_eq!(
            stub.count("eth_sendRawTransaction"),
            0,
            "recovery is a read, not a send: {:?}",
            stub.received()
        );
    }
}

// ---------------------------------------------------------------------------
// The structural half: §7's audit of every entry that can sign or send.
// ---------------------------------------------------------------------------

/// The constructors that end in a `Signer`, as they appear at a call site. A lane built any other
/// way does not exist: these are the only four in the build.
const LANE_SITES: [&str; 4] = [
    "ExecutionStage::new(",
    "ExecutionStage::connect(",
    "SequenceStage::new(",
    "SequenceStage::connect",
];

/// The one way a handle gets made: a durable ledger opened or read back from the operator's
/// directory. `ExecutionJournal::volatile` is deliberately *not* on this list — §11 forbids a
/// memory-only fallback in a production entry, and [`no_production_journal_is_memory_only`] holds
/// that line.
const LEDGER_READS: [&str; 2] = ["ExecutionJournal::open(", "ExecutionJournal::reload("];

/// Where the ledger read sits relative to one lane construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coverage {
    /// A ledger read earlier in the same function region, on the line named here.
    SameFunction(usize),
    /// Nothing above this line reads a ledger. Reported, never assumed away.
    Ungated,
}

/// The production region of one source file: everything above `#[cfg(test)]`. A test that builds a
/// lane to exercise it is not a run that builds one — the same cut `readiness_isolation.rs` makes,
/// for the same reason.
fn production_lines(rel: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(rel).unwrap_or_else(|error| panic!("{rel:?}: {error}"));
    let production = match text.find("\n#[cfg(test)]") {
        Some(at) => &text[..at],
        None => &text[..],
    };
    production.lines().map(str::to_string).collect()
}

/// Drop the comment lines: a doc sentence that names a constructor is not a call of it, and
/// without this the crate's own explanation of the rule would satisfy the rule.
fn code_only(lines: &[String]) -> Vec<(usize, String)> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(n, line)| (n + 1, line.clone()))
        .collect()
}

/// The line span of each top-level function, as `(name, start, end)`, where `end` is the next
/// top-level `fn` line rather than a brace-matched close. The three spending entries are all
/// column-0 functions, and the asymmetry is stated rather than hidden: an indented method after a
/// top-level function's start is credited that function's region.
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

fn ledger_read_lines(lines: &[(usize, String)]) -> Vec<usize> {
    lines
        .iter()
        .filter(|(_, line)| {
            LEDGER_READS.iter().any(|read| line.contains(read)) && !line.contains("fn open")
        })
        .map(|(n, _)| *n)
        .collect()
}

/// How the ledger read sits above one construction line, in one file's code lines.
fn coverage(lines: &[(usize, String)], reads: &[usize], ctor: usize) -> Coverage {
    let region = fn_regions(lines)
        .into_iter()
        .find(|(_, start, end)| *start <= ctor && ctor < *end);
    let Some((_, start, _)) = region else {
        return Coverage::Ungated;
    };
    match reads.iter().find(|read| **read >= start && **read < ctor) {
        Some(read) => Coverage::SameFunction(*read),
        None => Coverage::Ungated,
    }
}

/// Every production lane construction in the workspace, as `(file, line, code, coverage)`.
fn scanned_sites() -> Vec<(PathBuf, usize, String, Coverage)> {
    let mut out: Vec<(PathBuf, usize, String, Coverage)> = Vec::new();
    for file in production_sources() {
        let code = code_only(&production_lines(&file));
        let reads = ledger_read_lines(&code);
        for (n, line) in code.iter() {
            if LANE_SITES.iter().any(|site| line.contains(site)) {
                out.push((
                    file.clone(),
                    *n,
                    line.trim().to_string(),
                    coverage(&code, &reads, *n),
                ));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out
}

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

/// §7's 「必须覆盖所有生产入口…以实际代码审计为准」 as a property of the repository: every place
/// a signing or submitting lane is built in production code has a ledger read above it in the same
/// function, and the four entries the task book names all turn up in the scan.
///
/// The behavioural tests above prove this for the one entry they start. This proves it for the
/// entries nobody starts — `runner::run`'s lane arm and `Deployer`'s included — which is the case a
/// behavioural test cannot reach, and the reason an entry added next week has to be caught by the
/// scan rather than by a memory of which files were named.
#[test]
fn every_production_entry_that_builds_a_lane_reads_a_ledger_above_it() {
    let sites = scanned_sites();

    // The scan's own positive control, before its verdict. A walk that read nothing would satisfy
    // `all` vacuously, and a vacuous pass is how an unprotected entry is born.
    for (rel, needle) in [
        ("crates/pipeline/src/runner.rs", "ExecutionStage::connect"),
        (
            "crates/pipeline/src/arbitrage.rs",
            "SequenceStage::connect_with_trace",
        ),
        ("crates/cli/src/lib.rs", "ExecutionStage::connect"),
    ] {
        let want = workspace_root().join(rel);
        assert!(
            sites
                .iter()
                .any(|(f, _, code, _)| *f == want && code.contains(needle)),
            "{rel} builds a lane and the scan does not see it: {sites:?}"
        );
    }
    assert!(
        !sites.is_empty(),
        "a scan that found no lane construction is not a pass"
    );

    let unprotected: Vec<String> = sites
        .iter()
        .filter(|(_, _, _, cov)| matches!(cov, Coverage::Ungated))
        .map(|(f, line, code, _)| format!("{}:{line}: {code}", f.display()))
        .collect();
    assert!(
        unprotected.is_empty(),
        "these lanes are built with no ledger read above them, so §7's recovery does not \
         complete before them: {unprotected:?}"
    );

    // And where each protected one gets its handle, printed rather than asserted away: the audit
    // is only worth reading if a reader can check the pairing.
    for (f, line, code, cov) in &sites {
        let Coverage::SameFunction(read) = cov else {
            continue;
        };
        println!(
            "{}:{line}: {code} — ledger read above it at :{read}",
            f.strip_prefix(workspace_root()).unwrap_or(f).display()
        );
    }
}

/// The same scan on a source that does the wrong thing: a lane built with no ledger read above it.
/// Without this row the audit above cannot tell a protected entry from a scanner that matches
/// nothing, which is the failure mode §11's 「不得降低门禁要求」 is about.
#[test]
fn the_scan_reports_a_lane_built_without_a_ledger_read() {
    let source: Vec<String> = [
        "use evm_execution::ExecutionStage;",
        "pub async fn run(url: &str) -> Result<()> {",
        "    let adapter = Head::connect(url).await?;",
        "    let mut stage = ExecutionStage::connect(url, chain_id, setup, clock).await?;",
        "    stage.on_intent(&intent).await",
        "}",
    ]
    .iter()
    .map(|line| line.to_string())
    .collect();
    let code = code_only(&source);
    let reads = ledger_read_lines(&code);
    assert!(reads.is_empty(), "this synthetic file reads no ledger");
    let ctor = code
        .iter()
        .find(|(_, line)| line.contains("ExecutionStage::connect("))
        .expect("the synthetic lane")
        .0;
    assert_eq!(
        coverage(&code, &reads, ctor),
        Coverage::Ungated,
        "a lane with nothing above it has to be reported as unprotected"
    );

    // The same file with the read added, in the wrong order: below the lane. Recovery that runs
    // after the lane exists is §7's 「恢复必须在…之前完成」 violated, so the scan has to keep
    // calling it unprotected rather than crediting any read in the function.
    let mut reordered = source.clone();
    reordered.push("    let journal = ExecutionJournal::open(dir, chain, at)?;".to_string());
    let code = code_only(&reordered);
    let reads = ledger_read_lines(&code);
    assert_eq!(
        coverage(&code, &reads, ctor),
        Coverage::Ungated,
        "a ledger read that comes after the lane does not recover before it"
    );

    // And the shape the real entries have: the read above the construction, in the same function.
    let mut fixed = source.clone();
    fixed.insert(
        3,
        "    let journal = ExecutionJournal::open(dir, chain, at)?;".to_string(),
    );
    let code = code_only(&fixed);
    let reads = ledger_read_lines(&code);
    let moved = code
        .iter()
        .find(|(_, line)| line.contains("ExecutionStage::connect("))
        .expect("the same lane, one line later")
        .0;
    assert!(
        matches!(coverage(&code, &reads, moved), Coverage::SameFunction(_)),
        "a read above the lane has to be credited, or the real entries would fail this too"
    );
}

/// §11's 「memory-only 不能作为生产回退」 and §4.1's 「不得静默降级为仅内存模式」 as a fact about
/// the build: the handle that writes nothing to disk exists for tests, and no production source
/// names it. Were this to go red, an entry would be trading with no ledger at all.
#[test]
fn no_production_source_holds_a_memory_only_journal() {
    let hits: Vec<String> = production_sources()
        .iter()
        .flat_map(|file| {
            code_only(&production_lines(file))
                .into_iter()
                .map(move |(n, line)| (file.clone(), n, line))
        })
        .filter(|(_, _, line)| line.contains("ExecutionJournal::volatile"))
        .map(|(f, n, line)| format!("{}:{n}: {}", f.display(), line.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "a production entry that builds a memory-only journal trades with no ledger to recover \
         from (§11): {hits:?}"
    );
}

/// The other half of §7's coverage: the five lane constructors themselves take the handle as a
/// required argument, so a caller cannot build a lane without having read a ledger. The scan of
/// call sites above shows what the entries do today; this shows that adding an entry which skips
/// the ledger is a compile error rather than a review miss.
#[test]
fn every_lane_constructor_takes_the_journal_as_a_required_argument() {
    for (rel, constructor) in [
        ("crates/execution/src/stage.rs", "pub fn new("),
        ("crates/execution/src/stage.rs", "pub async fn connect("),
        ("crates/execution/src/sequence.rs", "pub fn new("),
        (
            "crates/execution/src/sequence.rs",
            "pub async fn connect_with_trace(",
        ),
        ("crates/execution/src/deploy.rs", "pub fn new("),
    ] {
        let path = workspace_root().join(rel);
        let code = code_only(&production_lines(&path));
        let start = code
            .iter()
            .position(|(_, line)| line.trim() == constructor)
            .unwrap_or_else(|| {
                panic!(
                    "{rel}: `{constructor}` is not spelled that way any more, so this check would \
                     be vacuous"
                )
            });
        let signature: String = code
            .iter()
            .skip(start)
            .take(22)
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            signature.contains("journal: ExecutionJournal"),
            "{rel}: the lane constructor `{constructor}` does not take the ledger handle, so a \
             caller could build a lane without reading one:\n{signature}"
        );
    }
}
