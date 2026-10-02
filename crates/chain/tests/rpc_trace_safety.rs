//! M8.2 §18, §19, §27: the safety proof for the RPC instrumentation, run against a server
//! this test can count the requests to.
//!
//! §18 is a hard rule with a soft shape — "instrumentation must not itself issue a
//! request" is easy to intend and hard to *observe*, because the observer that would check
//! it is usually the thing being checked. So the check here is a second account of the same
//! facts: a JSON-RPC endpoint of this test's own, which tallies every POST it receives, and
//! the sink, which tallies every call the adapter says it made. If tracing had added a
//! request, the two numbers would differ. If tracing had reordered one, the two *sequences*
//! would differ. Both are asserted, on the same calls, through the same adapter.
//!
//! ```text
//! what the stub is        std::net::TcpListener on 127.0.0.1:0, one thread, answers every
//!                         request it is configured for and remembers their method names in
//!                         the order they arrived
//! why it is not a mock    it stands in for a node's wire behaviour only. Nothing in here
//!                         pretends to be a market, a reserve, an opportunity or a block:
//!                         no pipeline runs, no decision is made, and no figure from this
//!                         file enters an evidence directory (§34's no-mock rule, which
//!                         these tests honour by measuring the observer, not the market)
//! what it costs           no new dependency: plain std for the server, the tokio runtime
//!                         the workspace already has for the client
//! ```
//!
//! The four behaviours below are the four the choke point can end a call in and that a
//! summary groups by: answered, retried-then-answered, node-rejected, and answered-with-nothing-
//! to-decode. `send_failed` and `non_json_response` are left out because both depend on the
//! transport or a malformed frame rather than on a classification this test could pin; the
//! sink's own handling of them, including the endpoint scrub, is tested in
//! `crates/chain/src/rpc_trace.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use alloy_primitives::{address, Address};

use evm_chain::{ChainAdapter, HttpChainAdapter, RpcTraceSink, RpcTraceSource};
use evm_core::{BlockNumber, ChainId};

const CHAIN: ChainId = ChainId(91_342);
/// The chain id as a node reports it: 91 342 is `0x164ce`.
const CHAIN_HEX: &str = "0x164ce";
const POOL: Address = address!("0x3978e57bbceb7666d54a03551c03691f897f6092");

/// What the stub does with a *state* request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    /// Answer with a payload the adapter can decode.
    Answer,
    /// Fail the first state request with a 500, answer the second. This is the shape of a
    /// public node that dropped a connection, and the only way to see whether the
    /// instrumentation counts a retried *call* or two calls.
    Flaky,
    /// Answer 200 with a JSON-RPC error object: the node replied, and replied "no".
    Rejected,
    /// Answer 200 with a body that carries no `result`: nothing to decode.
    Empty,
}

/// A stub endpoint and the record of what it was asked for.
struct Stub {
    url: String,
    /// The method names in arrival order — the endpoint's own account of the sequence.
    received: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn spawn(behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port is available");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        let received = Arc::new(Mutex::new(Vec::new()));
        let counted = received.clone();
        std::thread::spawn(move || {
            // The requests that carry state, in arrival order. `eth_chainId` is excluded from
            // this count because it is what `connect` asks for to learn the chain id: a
            // behaviour keyed off "the first request" would fail the connect instead of the
            // call the test is about.
            let mut state_requests = 0usize;
            for stream in listener.incoming().flatten() {
                let mut names = counted.lock().expect("the tally");
                let method = Self::serve(stream, behaviour, state_requests);
                if method != "eth_chainId" {
                    state_requests += 1;
                }
                names.push(method);
                drop(names);
            }
        });
        Self { url, received }
    }

    /// The methods this endpoint has answered so far, in order.
    fn received(&self) -> Vec<String> {
        self.received.lock().expect("the tally").clone()
    }

    /// Read one JSON-RPC request, answer it, and report which method it asked for.
    ///
    /// `eth_chainId` is always answered the same way, whatever the behaviour: it is the
    /// request `connect` makes to learn the chain id, and a stub that refused it would not
    /// be a node with a state policy, it would be a stub no test could even reach.
    ///
    /// The reply is written before the connection closes, and every response says
    /// `connection: close`: a test that counts requests has to be sure a pooled keep-alive
    /// connection cannot make two logical calls look like one arrival.
    fn serve(stream: TcpStream, behaviour: Behaviour, state_requests: usize) -> String {
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return "unreadable".to_string();
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
            return "unreadable".to_string();
        }
        let request: serde_json::Value =
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();

        // The one thing a Flaky stub does: the first *state* request it is given gets a 500
        // and the next one an answer. `eth_chainId` is answered outside this rule because a
        // stub that refused the connect would not be a busy node, it would be unreachable.
        let dropped =
            behaviour == Behaviour::Flaky && method != "eth_chainId" && state_requests == 0;
        let (status, payload) = if method == "eth_chainId" {
            (
                "200 OK",
                format!(r#"{{"jsonrpc":"2.0","id":1,"result":"{CHAIN_HEX}"}}"#),
            )
        } else if dropped {
            (
                "500 Internal Server Error",
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"node is busy"}}"#
                    .to_string(),
            )
        } else {
            match behaviour {
                Behaviour::Answer | Behaviour::Flaky => (
                    "200 OK",
                    r#"{"jsonrpc":"2.0","id":1,"result":"0xdeadbeef"}"#.to_string(),
                ),
                Behaviour::Rejected => (
                    "200 OK",
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"state not available at this height"}}"#
                        .to_string(),
                ),
                Behaviour::Empty => ("200 OK", r#"{"jsonrpc":"2.0","id":1}"#.to_string()),
            }
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\
             \r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        method
    }
}

/// A connected adapter and its stub, so a test never has to say "and then remember the url".
async fn connected(behaviour: Behaviour) -> (Stub, HttpChainAdapter) {
    let stub = Stub::spawn(behaviour);
    let adapter = HttpChainAdapter::connect(&stub.url)
        .await
        .expect("the stub answers eth_chainId, which is all a connect does");
    (stub, adapter)
}

fn sink() -> RpcTraceSink {
    sink_named("one-simulation-under-test")
}

fn sink_named(simulation_id: &str) -> RpcTraceSink {
    RpcTraceSink::new(
        std::time::Instant::now(),
        simulation_id,
        RpcTraceSource::Live,
        Some(CHAIN.0),
    )
}

/// §18 and §19 together, in the strongest form available to a test: the endpoint's count of
/// what arrived equals the sink's count of what was made, in the same order, and an untraced
/// adapter against a second stub of the same shape produces the same two numbers. Tracing
/// added no request, removed none, and moved none.
#[tokio::test]
async fn tracing_records_the_calls_a_run_makes_and_adds_none_of_its_own() {
    let (stub, base) = connected(Behaviour::Answer).await;
    let observed = sink();
    let traced = ChainAdapter::with_rpc_trace(&base, observed.clone())
        .expect("an http source has calls to record");

    traced.get_code(BlockNumber(100), POOL).await.expect("code");
    traced
        .get_balance(BlockNumber(100), POOL)
        .await
        .expect("balance");
    traced
        .get_storage_at(BlockNumber(100), POOL, alloy_primitives::U256::from(7u64))
        .await
        .expect("storage");

    // One connect plus three reads. A traced path that had added anything would show here as
    // a length the sink cannot account for.
    assert_eq!(
        stub.received(),
        vec![
            "eth_chainId".to_string(),
            "eth_getCode".to_string(),
            "eth_getBalance".to_string(),
            "eth_getStorageAt".to_string(),
        ],
        "the endpoint saw exactly the requests this run asked for"
    );
    let events = observed.events();
    assert_eq!(
        events.len(),
        3,
        "one record per call, and no record of a call nobody made"
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.method.clone())
            .collect::<Vec<_>>(),
        vec![
            "eth_getCode".to_string(),
            "eth_getBalance".to_string(),
            "eth_getStorageAt".to_string(),
        ],
        "§19's order: the records arrive in the order the calls did"
    );
    assert_eq!(
        events.iter().map(|e| e.rpc_id).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the ids are the sink's own call order"
    );
    for event in &events {
        assert!(
            event.success,
            "the stub answered, so no call is a failure here"
        );
        assert!(event.error_class.is_none());
        assert_eq!(event.attempts.len(), 1, "one try, one attempt record");
        assert!(
            event.duration_ns > 0,
            "a whole provider call is never instant"
        );
        assert!(event.finished_ns >= event.started_ns);
        assert!(
            event.dedup_key.is_some(),
            "§12 keys every one of these three methods"
        );
    }
    // §12's key is the *call*, not the stage: the same address at the same height by three
    // different methods is three different states asked for.
    let keys: Vec<&str> = events
        .iter()
        .map(|event| event.dedup_key.as_deref().expect("a key"))
        .collect();
    assert!(keys[0].starts_with("code|91342|100|"), "{keys:?}");
    assert!(keys[1].starts_with("balance|91342|100|"), "{keys:?}");
    assert!(keys[2].starts_with("storage|91342|100|"), "{keys:?}");
    assert_eq!(
        keys.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );

    // The same three reads through an untraced clone of the same adapter, against a second
    // stub: the request sequence is identical, which is §19's equality stated as a
    // measurement rather than as a promise in a comment.
    let (untouched, plain_base) = connected(Behaviour::Answer).await;
    let plain: std::sync::Arc<dyn ChainAdapter> = std::sync::Arc::new(plain_base);
    plain.get_code(BlockNumber(100), POOL).await.expect("code");
    plain
        .get_balance(BlockNumber(100), POOL)
        .await
        .expect("balance");
    plain
        .get_storage_at(BlockNumber(100), POOL, alloy_primitives::U256::from(7u64))
        .await
        .expect("storage");
    assert_eq!(
        untouched.received(),
        stub.received(),
        "traced and untraced ask for the same things in the same order"
    );
    assert_eq!(observed.dropped_events(), 0);
    assert!(observed.refusals().is_empty());
}

/// §25's B-versus-E distinction rests entirely on this: a call that burned one failed try
/// and then succeeded is *one call* with two attempts, not two calls. Count it as two and a
/// node that dropped a connection looks like a slow node; count it as one and the retry
/// becomes visible as itself.
#[tokio::test]
async fn a_retried_call_is_one_record_with_two_attempts() {
    let (stub, base) = connected(Behaviour::Flaky).await;
    let observed = sink();
    let traced = ChainAdapter::with_rpc_trace(&base, observed.clone()).expect("a source to watch");

    traced
        .get_code(BlockNumber(100), POOL)
        .await
        .expect("the second attempt answers");

    assert_eq!(
        stub.received(),
        vec![
            "eth_chainId".to_string(),
            "eth_getCode".to_string(),
            "eth_getCode".to_string(),
        ],
        "the retry loop ran once, so the endpoint saw one call asked twice — and no more"
    );
    let events = observed.events();
    assert_eq!(events.len(), 1, "one logical call");
    let event = &events[0];
    assert_eq!(event.attempts.len(), 2, "two HTTP tries inside it");
    assert_eq!(
        event
            .attempts
            .iter()
            .map(|attempt| attempt.outcome)
            .collect::<Vec<_>>(),
        vec![evm_chain::CLASS_HTTP_STATUS, evm_chain::CLASS_OK],
        "the first try's own class is kept, not rewritten by the try that followed"
    );
    assert!(event.success, "the call as a whole answered");
    assert!(event.error_class.is_none());
    // The call's duration spans both tries, because that is what the caller waited for.
    assert!(event.duration_ns >= event.attempts[0].duration_ns);
    assert_eq!(
        event.finished_ns.saturating_sub(event.started_ns),
        event.duration_ns,
        "the record's own arithmetic is stated, not left to a reader"
    );
}

/// A node that answers "no" is a different finding from a node that is slow, and the run's
/// error must not change because the call was also being watched: same variant, same text,
/// plus a record of which class it was.
#[tokio::test]
async fn a_node_that_refuses_is_one_record_and_the_same_error_as_before() {
    let (stub, base) = connected(Behaviour::Rejected).await;
    let observed = sink();
    let traced = ChainAdapter::with_rpc_trace(&base, observed.clone()).expect("a source to watch");

    let error = traced
        .get_block_context(BlockNumber(37_594_591))
        .await
        .expect_err("the node said no");
    assert!(
        matches!(error, evm_chain::ChainError::RpcRejected(_)),
        "watching a call must not turn a refusal into a different failure: {error:?}"
    );

    let events = observed.events();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert!(!event.success);
    assert_eq!(event.error_class, Some(evm_chain::CLASS_NODE_REJECTED));
    assert!(event
        .error_detail
        .as_deref()
        .is_some_and(|detail| detail.contains("state not available")));
    assert_eq!(
        event.attempts.len(),
        1,
        "a node's refusal is an answer: the adapter does not retry it, and the record says \
         it was asked once"
    );
    assert_eq!(
        stub.received().len(),
        2,
        "connect plus the one refused call"
    );
}

/// A body with nothing in it is a decode failure at this choke point, and the block argument
/// still gets its §12 key: the key describes what was asked, so an unanswered call is not
/// excluded from the duplicate tally on the strength of its outcome.
#[tokio::test]
async fn a_response_with_no_result_is_recorded_as_a_decode_failure() {
    let (_stub, base) = connected(Behaviour::Empty).await;
    let observed = sink();
    let traced = ChainAdapter::with_rpc_trace(&base, observed.clone()).expect("a source to watch");

    let error = traced
        .call(
            BlockNumber(200),
            &evm_chain::CallRequest {
                to: POOL,
                data: alloy_primitives::Bytes::from(vec![0xaa, 0xbb]),
            },
        )
        .await
        .expect_err("there is no result to read");
    assert!(
        matches!(error, evm_chain::ChainError::Decode(_)),
        "{error:?}"
    );

    let events = observed.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].error_class, Some(evm_chain::CLASS_DECODE_FAILED));
    assert_eq!(events[0].method, "eth_call");
    assert_eq!(
        events[0].block.as_deref(),
        Some("200"),
        "the hex height the wire carried is stored as the number it means"
    );
    assert!(events[0]
        .dedup_key
        .as_deref()
        .is_some_and(|key| key.starts_with("call|91342|200|")));
}

/// §6's attribution requirement, tested where it can actually break: two simulations sharing
/// one connection pool. Each sink must hold only its own simulation's calls, and neither may
/// see the other's — which is why the handle is attached to a derived clone rather than
/// installed once per process (§31).
#[tokio::test]
async fn two_sinks_on_one_endpoint_keep_their_calls_apart() {
    let (stub, base) = connected(Behaviour::Answer).await;
    let first = sink_named("simulation-a");
    let second = sink_named("simulation-b");
    let a = ChainAdapter::with_rpc_trace(&base, first.clone()).expect("a source to watch");
    let b = ChainAdapter::with_rpc_trace(&base, second.clone()).expect("a second source");

    a.get_code(BlockNumber(1), POOL).await.expect("a's read");
    b.get_balance(BlockNumber(1), POOL).await.expect("b's read");
    a.get_nonce(BlockNumber(1), POOL)
        .await
        .expect("a's second read");

    assert_eq!(
        first
            .events()
            .iter()
            .map(|event| event.method.clone())
            .collect::<Vec<_>>(),
        vec![
            "eth_getCode".to_string(),
            "eth_getTransactionCount".to_string()
        ],
        "the first simulation's own two calls, and nothing belonging to the second"
    );
    assert_eq!(
        second
            .events()
            .iter()
            .map(|event| event.method.clone())
            .collect::<Vec<_>>(),
        vec!["eth_getBalance".to_string()],
        "the second simulation sees only its own call"
    );
    assert_eq!(first.simulation_id(), "simulation-a");
    assert_eq!(second.simulation_id(), "simulation-b");
    assert_eq!(
        stub.received().len(),
        4,
        "connect plus three reads — two sinks did not mean two requests each"
    );
}

/// The untraced adapter must still be the adapter it was: `with_rpc_trace` is additive, and
/// a caller that never asks for a sink gets the behaviour every earlier milestone had. This
/// is the same claim as the first test's, read from the other side — the *source*, not the
/// count: a recorded directory has no hook, and saying so is the honest answer rather than a
/// zero.
#[test]
fn a_source_with_no_calls_to_record_says_so_instead_of_offering_a_sink() {
    let dir = std::env::temp_dir().join(format!(
        "evm-chain-rpc-trace-recorded-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("an empty recorded directory");
    let recorded = evm_chain::RecordedChainAdapter::load(&dir, CHAIN).expect("loads");
    assert!(
        ChainAdapter::with_rpc_trace(&recorded, sink()).is_none(),
        "a replay directory answers from disk: there is nothing on the wire to record"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Requests counted, so the tally above means something: a stub that never saw a POST would
/// make every assertion about "no extra request" pass by being deaf. This is the control.
#[tokio::test]
async fn the_stub_hears_every_request_it_is_given() {
    let (stub, base) = connected(Behaviour::Answer).await;
    // `connect` itself is one POST; a test that asserted on a count of three and quietly
    // forgot it would be asserting on a number nobody measured.
    assert_eq!(stub.received(), vec!["eth_chainId".to_string()]);
    let plain: std::sync::Arc<dyn ChainAdapter> = std::sync::Arc::new(base);
    plain.latest_block().await.expect("head");
    assert_eq!(
        stub.received(),
        vec!["eth_chainId".to_string(), "eth_blockNumber".to_string()],
    );
}
