//! M12-B §3's readiness gate measured against an endpoint, not against this module's own
//! word for what a node would say.
//!
//! The unit tests in `crates/chain/src/readiness.rs` feed `judge` decoded shapes. That is
//! the right place for the decision table, and the wrong place for §3's list, which names
//! the failures a *wire* produces: a JSON-RPC error, a timeout, a connection dropped
//! mid-request, a body that is not JSON, a progress object whose fields are the wrong type.
//! Those are facts about reqwest and about a node, and only an endpoint can say them.
//!
//! ```text
//! what the stub is        std::net::TcpListener on 127.0.0.1:0, one thread per connection,
//!                         answers eth_chainId so a connect can finish and eth_syncing
//!                         however the test's shape says, and remembers every method name
//!                         with the params it arrived in, in arrival order
//! why it is not a mock    it stands in for a node's wire behaviour only. No pipeline runs,
//!                         no market is invented, and no number here goes into an evidence
//!                         directory. Per M12-A §13.2 this file exists because the four
//!                         behaviours rpc_trace_safety.rs serves do not include a timeout,
//!                         a dropped stream, or a non-JSON frame.
//! what it costs           no new dependency. One test — `a_node_that_never_answers…` —
//!                         waits out the adapter's real 20 s client timeout twice, because
//!                         a timeout measured shorter than it is would not be a timeout.
//! ```
//!
//! Every test here asserts two things the task book asks for together: the verdict the gate
//! returned, and the number of requests the endpoint actually saw. A gate that reached the
//! right answer by asking the node five times would pass a verdict-only test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use evm_chain::{
    HeadFreshnessPolicy, HeadReader, HttpChainAdapter, Readiness, ReadinessGate, SyncProgress,
    SyncStatus,
};

/// The chain id a stub answers `eth_chainId` with. The gate never reads it; it is here only
/// so `HttpChainAdapter::connect` can finish and hand the test an adapter.
const CHAIN_HEX: &str = "0x164ce";

/// What the stub does with `eth_syncing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The legal pass answer.
    False,
    /// The legal hold answer, with the node's three numbers.
    Progress,
    /// 200 OK carrying a JSON-RPC error — the node replied, and replied "no".
    Rejected,
    /// 200 OK whose body is not JSON at all.
    NonJson,
    /// A progress object whose quantities are JSON numbers rather than hex strings.
    WrongType,
    /// A `null` result — an illegal response shape, not "not syncing".
    Null,
    /// Accept the request, then close the stream without a response.
    Dropped,
    /// Accept the request and answer nothing for as long as the test lasts.
    Hanging,
    /// §6's pair of pending reads, answered by shape: `eth_getBlockByNumber` with
    /// `full: true` returns transaction *objects*, with `false` the bare hashes — so a
    /// test can tell which parameter the adapter actually put on the wire.
    PendingShapes,
}

struct Stub {
    url: String,
    /// One entry per arrival, in order: the method name and the params it arrived with.
    /// §3's tests read the names; §6.1's test reads the params, because the defect it
    /// closes was a `full` flag that did not match the decoder.
    received: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

impl Stub {
    fn spawn(shape: Shape) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port is available");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        let received = Arc::new(Mutex::new(Vec::new()));
        let counted = received.clone();
        std::thread::spawn(move || {
            // One thread per connection, unlike `rpc_trace_safety.rs`: a `Hanging` stub has
            // to hold its first connection open *while still hearing the retry*, or the test
            // would count one arrival where the adapter made two.
            for stream in listener.incoming().flatten() {
                let counted = counted.clone();
                std::thread::spawn(move || {
                    let arrival = Self::serve(stream, shape);
                    counted.lock().expect("the tally").push(arrival);
                });
            }
        });
        Self { url, received }
    }

    fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .expect("the tally")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    /// The params each `eth_getBlockByNumber` arrived with, in arrival order.
    fn pending_asks(&self) -> Vec<serde_json::Value> {
        self.received
            .lock()
            .expect("the tally")
            .iter()
            .filter(|(method, _)| method == "eth_getBlockByNumber")
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// Read one JSON-RPC request, answer it, and report what it asked for.
    fn serve(stream: TcpStream, shape: Shape) -> (String, serde_json::Value) {
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return ("unreadable".to_string(), serde_json::Value::Null);
            }
            let lowered = line.to_ascii_lowercase();
            if let Some(rest) = lowered.strip_prefix("content-length:") {
                length = rest.trim().parse().unwrap_or(0);
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return ("unreadable".to_string(), serde_json::Value::Null);
        }
        let request: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let params = request
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new()));

        // `eth_chainId` is always answered, whatever the shape: a stub that refused it would
        // not be an unsynced node, it would be a stub no test could reach.
        let Some((status, payload)) = reply(&method, shape, &params) else {
            // `Dropped` and `Hanging` send nothing. Dropping the stream closes it; hanging
            // keeps it open by leaking the handle, which is the point — the client's own
            // timeout has to be what ends the call.
            if shape == Shape::Hanging {
                std::mem::forget(stream);
            }
            return (method, params);
        };
        // A non-JSON body is only non-JSON in its bytes; giving it a JSON content type would
        // test reqwest's leniency rather than the gate's refusal, so this shape says HTML.
        let content_type = if shape == Shape::NonJson && method == "eth_syncing" {
            "text/html"
        } else {
            "application/json"
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\
             \r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        (method, params)
    }
}

/// The stub's answer for one method, or `None` when it sends nothing back.
fn reply(method: &str, shape: Shape, params: &serde_json::Value) -> Option<(&'static str, String)> {
    if method == "eth_chainId" {
        return Some((
            "200 OK",
            format!(r#"{{"jsonrpc":"2.0","id":1,"result":"{CHAIN_HEX}"}}"#),
        ));
    }
    match shape {
        Shape::PendingShapes => Some(("200 OK", pending_payload(method, params))),
        Shape::False => Some((
            "200 OK",
            r#"{"jsonrpc":"2.0","id":1,"result":false}"#.to_string(),
        )),
        Shape::Progress => Some((
            "200 OK",
            r#"{"jsonrpc":"2.0","id":1,"result":{"startingBlock":"0x0","currentBlock":"0x24c","highestBlock":"0x3e8"}}"#
                .to_string(),
        )),
        Shape::Rejected => Some((
            "200 OK",
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"the method eth_syncing does not exist"}}"#
                .to_string(),
        )),
        Shape::NonJson => Some((
            "200 OK",
            "<html><body>502 from the intermediary</body></html>".to_string(),
        )),
        Shape::WrongType => Some((
            "200 OK",
            r#"{"jsonrpc":"2.0","id":1,"result":{"startingBlock":0,"currentBlock":588,"highestBlock":1000}}"#
                .to_string(),
        )),
        Shape::Null => Some(("200 OK", r#"{"jsonrpc":"2.0","id":1,"result":null}"#.to_string())),
        Shape::Dropped | Shape::Hanging => None,
    }
}

/// The `PendingShapes` answer: one pending block, in whichever of the two transaction
/// shapes the requested `full` flag selects. Both halves are the shapes M9.4 measured on
/// the real endpoint; the flag is the only difference, which is what §6.1 asks to be
/// checked against the wire rather than against this file's own expectation.
fn pending_payload(method: &str, params: &serde_json::Value) -> String {
    const HASH_A: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const HASH_B: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
    const ADDRESS: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let result = if method != "eth_getBlockByNumber" {
        serde_json::Value::Bool(false)
    } else {
        let full = params
            .get(1)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let transactions = if full {
            serde_json::json!([
                {"hash": HASH_A, "from": ADDRESS, "to": ADDRESS, "transactionIndex": "0x0"},
                {"hash": HASH_B, "from": ADDRESS, "to": ADDRESS, "transactionIndex": "0x1"},
            ])
        } else {
            serde_json::json!([HASH_A, HASH_B])
        };
        serde_json::json!({
            "number": "0x2433f23",
            "hash": HASH_A,
            "gasUsed": "0x5208",
            "transactions": transactions,
        })
    };
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{}}}"#,
        serde_json::to_string(&result).expect("a built value serializes")
    )
}

/// The node's syncing numbers as the `Progress` shape sends them.
const PROGRESS: SyncProgress = SyncProgress {
    starting_block: 0,
    current_block: 588,
    highest_block: 1000,
};

async fn gate_over(shape: Shape) -> (Stub, HttpChainAdapter) {
    let stub = Stub::spawn(shape);
    let adapter = HttpChainAdapter::connect(&stub.url)
        .await
        .expect("the stub answers eth_chainId, which is all a connect does");
    (stub, adapter)
}

/// §3's pass case, measured where the cost of a gate is visible: one logical call, one
/// request, and nothing else asked alongside it. A readiness check that also fetched a head
/// height or a chain id "for the evidence line" would fail the exact-vector assert below.
#[tokio::test]
async fn a_false_answer_readies_the_run_for_one_ask_and_nothing_more() {
    let (stub, adapter) = gate_over(Shape::False).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    assert_eq!(verdict, Readiness::Ready);
    assert!(verdict.is_ready());
    assert_eq!(verdict.withheld_because(), None);
    assert_eq!(gate.checks(), 1, "one check, one ask counted");
    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()],
        "the gate asked the node exactly one question, and the node's own answer is what \
         decided"
    );
}

/// §3's hold case with the node's numbers kept: the verdict says 588 of 1000 because the
/// node said so, not because anything here estimated it.
#[tokio::test]
async fn a_progress_object_holds_the_run_with_the_numbers_the_node_gave() {
    let (stub, adapter) = gate_over(Shape::Progress).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    assert_eq!(verdict, Readiness::Syncing(PROGRESS));
    assert!(!verdict.is_ready());
    let reason = verdict.withheld_because().expect("a hold has a reason");
    assert!(
        reason.contains("588") && reason.contains("1000"),
        "the reason quotes the node's progress: {reason}"
    );
    assert_eq!(gate.checks(), 1);
    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_syncing".to_string()]
    );
}

/// A JSON-RPC error is the node replying "no", which §3 puts among the answers that must
/// never count as ready. It is terminal at the wire layer, so it costs one request: the
/// gate must not turn a refusal into a retry loop.
#[tokio::test]
async fn a_rejected_call_holds_the_run_and_is_not_retried() {
    let (stub, adapter) = gate_over(Shape::Rejected).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    match &verdict {
        Readiness::Unverified(detail) => {
            assert!(
                detail.starts_with("the node rejected the call"),
                "the class word names a rejection: {detail}"
            );
            assert!(
                detail.contains("-32601"),
                "and the node's own error object is kept: {detail}"
            );
        }
        other => panic!("a rejected call must hold the run as Unverified, got {other:?}"),
    }
    assert!(!verdict.is_ready());
    assert_eq!(gate.checks(), 1);
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        1,
        "the node said no once, and was asked once"
    );
}

/// A body that is not JSON ends the logical call with two HTTP attempts, because the wire
/// layer's single transport retry applies to it. The two counts are the point: §3's
/// "unbounded retry" is bounded *per call*, and this is the same retry every method in the
/// repository already pays — the gate adds no policy of its own.
#[tokio::test]
async fn a_non_json_body_holds_the_run_with_the_existing_single_retry() {
    let (stub, adapter) = gate_over(Shape::NonJson).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    match &verdict {
        Readiness::Unverified(detail) => {
            assert!(
                detail.starts_with("the call produced no usable answer"),
                "a body nothing can read is not an answer: {detail}"
            );
            assert!(
                detail.contains("non-json"),
                "the wire's own words are kept: {detail}"
            );
        }
        other => panic!("a non-JSON body must hold the run, got {other:?}"),
    }
    assert_eq!(
        gate.checks(),
        1,
        "one logical call, whatever the transport did underneath it"
    );
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        2,
        "two HTTP attempts is the adapter's existing single retry, not a gate loop"
    );
}

/// A progress object whose quantities are JSON numbers rather than hex strings is a wrong
/// type, and §3's fail-closed rule means it holds the run rather than being coerced.
#[tokio::test]
async fn a_progress_object_of_the_wrong_type_holds_the_run() {
    let (stub, adapter) = gate_over(Shape::WrongType).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    match &verdict {
        Readiness::Unverified(detail) => {
            assert!(
                detail.starts_with("the node's answer could not be decoded"),
                "{detail}"
            );
            assert!(
                detail.contains("startingBlock"),
                "the rejection names the field it choked on, which is the first one it \
                 reads: {detail}"
            );
        }
        other => panic!("an undecodable progress object must hold the run, got {other:?}"),
    }
    assert_eq!(gate.checks(), 1);
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        1
    );
}

/// `null` is what a node answers when it has no sync story at all. It is not `false`, so it
/// is not a pass: §3's "非法响应" covers an answer that is merely empty.
#[tokio::test]
async fn a_null_result_holds_the_run_rather_than_reading_as_not_syncing() {
    let (stub, adapter) = gate_over(Shape::Null).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    assert!(
        matches!(verdict, Readiness::Unverified(_)),
        "a null result is an illegal shape, got {verdict:?}"
    );
    assert!(!verdict.is_ready());
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        1
    );
}

/// A connection closed before the response is a transport failure, and the cheapest of the
/// three holds: it costs two attempts and no waiting.
#[tokio::test]
async fn a_dropped_connection_holds_the_run() {
    let (stub, adapter) = gate_over(Shape::Dropped).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let verdict = gate.check(&adapter, Some(1_000)).await;

    match &verdict {
        Readiness::Unverified(detail) => {
            assert!(
                detail.starts_with("the call produced no usable answer"),
                "{detail}"
            );
        }
        other => panic!("a dropped stream must hold the run, got {other:?}"),
    }
    assert_eq!(gate.checks(), 1);
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        2,
        "the retry of a dropped connection is the adapter's existing one"
    );
}

/// The timeout case, at the cost it actually costs. The adapter's client timeout is 20 s and
/// the transport retry runs once, so a node that accepts the connection and says nothing
/// holds the gate for two full timeouts. Asserting the elapsed floor is what distinguishes
/// this from a test that merely got an error quickly for some other reason.
#[tokio::test]
async fn a_node_that_never_answers_is_a_timeout_and_holds_the_run() {
    let (stub, adapter) = gate_over(Shape::Hanging).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    let started = Instant::now();
    let verdict = gate.check(&adapter, Some(1_000)).await;
    let elapsed = started.elapsed();

    assert!(
        matches!(verdict, Readiness::Unverified(_)),
        "a node that never answers is never ready, got {verdict:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(20),
        "the client's own 20 s timeout is what ended this call; a shorter wait would mean \
         the stub answered and the test measured something else ({elapsed:?})"
    );
    assert_eq!(gate.checks(), 1);
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        2,
        "two timed-out attempts inside one logical ask"
    );
}

/// §3's bound on the loop, measured as requests rather than as prose: a gate asked four more
/// times than it has budget can only make its calls stop, never grow.
#[tokio::test]
async fn a_spent_budget_holds_the_run_and_asks_the_node_nothing() {
    let (stub, adapter) = gate_over(Shape::Progress).await;
    let mut gate = ReadinessGate::with_budget(HeadFreshnessPolicy::NotJudged, 2);

    let first = gate.check(&adapter, None).await;
    let second = gate.check(&adapter, None).await;
    let third = gate.check(&adapter, None).await;
    let fourth = gate.check(&adapter, None).await;

    assert_eq!(first, Readiness::Syncing(PROGRESS));
    assert_eq!(second, Readiness::Syncing(PROGRESS));
    for verdict in [&third, &fourth] {
        match verdict {
            Readiness::Unverified(detail) => {
                assert!(
                    detail.contains("budget"),
                    "the refusal says why it refused: {detail}"
                );
            }
            other => panic!("a spent budget must hold the run, got {other:?}"),
        }
    }
    assert_eq!(gate.checks(), 2, "a refusal spends nothing it did not do");
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        2,
        "the endpoint saw two asks for four calls, which is the whole of §3's no-unbounded-\
         retry rule"
    );
}

/// §3's other half: the gate is asked at recovery points, and one ask per recovery is what
/// one ask means on the wire. Four checks, four requests — nothing here multiplies per
/// market event, because the gate has no per-event path to be on.
#[tokio::test]
async fn every_recovery_recheck_costs_exactly_one_ask() {
    let (stub, adapter) = gate_over(Shape::False).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::NotJudged);

    for _ in 0..4 {
        assert_eq!(gate.check(&adapter, None).await, Readiness::Ready);
    }

    assert_eq!(gate.checks(), 4);
    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        4
    );
    assert_eq!(
        gate.remaining(),
        evm_chain::DEFAULT_CHECK_BUDGET - 4,
        "the budget is the only thing that shrinks"
    );
}

/// The freshness policy's two ends, on a node that says it is not syncing: the reference and
/// the tolerance come from configuration, and this is the run where the node's head is
/// further behind the reference than the operator agreed to allow.
#[tokio::test]
async fn a_configured_reference_head_holds_a_node_that_says_it_is_not_syncing() {
    let (stub, adapter) = gate_over(Shape::False).await;
    let mut gate = ReadinessGate::new(HeadFreshnessPolicy::AgainstReference {
        reference_head: 1_000,
        tolerance_blocks: 5,
    });

    let stale = gate.check(&adapter, Some(900)).await;
    assert_eq!(
        stale,
        Readiness::HeadBehindReference {
            reference_head: 1_000,
            tolerance_blocks: 5,
            observed_head: 900,
            lag_blocks: 100,
        }
    );
    assert!(!stale.is_ready());

    let fresh = gate.check(&adapter, Some(998)).await;
    assert_eq!(fresh, Readiness::Ready);

    assert_eq!(
        stub.received()
            .iter()
            .filter(|m| *m == "eth_syncing")
            .count(),
        2,
        "the freshness check reused the head number the caller already had and asked the \
         node for nothing extra"
    );
}

/// The gate's decode of the two legal shapes is the same function the wire path uses, so a
/// test can name the answer the adapter returned rather than only the verdict. That is the
/// difference between "the gate said ready" and "the node said `false`".
#[tokio::test]
async fn the_adapter_call_returns_the_decoded_answer_itself() {
    let (_stub, adapter) = gate_over(Shape::False).await;
    assert!(matches!(
        adapter.syncing_status().await,
        Ok(SyncStatus::NotSyncing)
    ));

    let (_stub, adapter) = gate_over(Shape::Progress).await;
    assert_eq!(
        adapter.syncing_status().await.expect("a progress object"),
        SyncStatus::Syncing(PROGRESS)
    );
}

/// §6.1, measured where the only copy of the answer lives: on the wire.
///
/// Every other guard this milestone added sits above a stub that implements *both*
/// pending methods, so flipping the `full` flag inside [`HttpChainAdapter`] would leave
/// all of them green while the radar went back to decoding a hash list against an
/// object-shaped decoder — which is defect D5 itself. This test asks the production
/// adapter twice and reads what the endpoint was actually sent.
#[tokio::test]
async fn the_two_pending_reads_ask_the_node_for_the_shape_each_needs() {
    let (stub, adapter) = gate_over(Shape::PendingShapes).await;
    let mut adapter = adapter;

    let light = HeadReader::pending_raw(&mut adapter)
        .await
        .expect("the stub answers the light shape")
        .expect("the stub has a pending block");
    let heavy = HeadReader::pending_full_transactions(&mut adapter)
        .await
        .expect("the stub answers the heavy shape")
        .expect("the stub has a pending block");

    // The params as the endpoint received them, in arrival order — one flag apart.
    assert_eq!(
        stub.pending_asks(),
        vec![
            serde_json::json!(["pending", false]),
            serde_json::json!(["pending", true]),
        ],
        "the light read stays `full: false` and only the radar's read pays for objects"
    );

    let light_list = light["transactions"]
        .as_array()
        .expect("the light answer carries a transaction list");
    assert!(
        light_list.iter().all(serde_json::Value::is_string),
        "`full: false` selects the bare-hash shape: {light_list:?}"
    );
    let heavy_list = heavy["transactions"]
        .as_array()
        .expect("the heavy answer carries a transaction list");
    assert!(
        heavy_list.iter().all(serde_json::Value::is_object),
        "`full: true` selects the shape the M9.4 decoder reads: {heavy_list:?}"
    );

    // And the two answers are the same block, so the flag is the only difference between
    // them. Were it not, this test would be comparing two different payloads and saying
    // nothing about the parameter.
    assert_eq!(light["number"], heavy["number"]);
    assert_eq!(light_list.len(), heavy_list.len());

    // Control: the shape the radar needs does not arrive when the flag is not sent. A
    // node answering `full: false` cannot be blamed for the decoder's refusal.
    let hashes_only = stub.pending_asks()[0]
        .get(1)
        .and_then(serde_json::Value::as_bool);
    assert_eq!(hashes_only, Some(false));
}
