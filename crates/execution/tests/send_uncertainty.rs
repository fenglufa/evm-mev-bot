//! M12-E §7: what the send path does when it does not get an answer it can read.
//!
//! The task book's one sentence — 「一次交易提交请求没有得到明确结果，不等于交易没有提交成功」
//! — is a request-count claim before it is a classification claim. A client that re-sends
//! after a lost response has not asked a question twice, it has *bought* twice; so every
//! test in this file counts arrivals at a socket, and the count it reports is the stub's own
//! tally rather than a call counter inside our process.
//!
//! Three facts from the code make this file necessary, and all three were measured before
//! the code was touched (M12-E §2):
//!
//! ```text
//! the send rode the read path       `GiwaSequencerDirect::submit` asked `request_raw`, the
//!                                   loop that tries twice when a transport error is
//!                                   non-terminal (send failure, non-JSON body, HTTP
//!                                   status). A signed transaction handed to that loop can
//!                                   be posted twice from one call.
//! any error field was a refusal     a JSON-RPC `error` member became
//!                                   `ChainError::RpcRejected`, which became
//!                                   `SubmissionOutcome::Rejected`, which is the one answer
//!                                   the lifecycle reads as "nothing is in flight" and frees
//!                                   the nonce for. `nonce too low` travelled that road, and
//!                                   it is an answer about a transaction that may be ours.
//! an acknowledgement could be about `Accepted` carried a `hash_matches_local` flag nobody
//! another transaction               read: a node naming someone else's hash still satisfied
//!                                   `matches!(outcome, Accepted { .. })` downstream and
//!                                   advanced to receipt tracking.
//! ```
//!
//! What the fix is, at the same three places: a send goes out on
//! [`evm_chain::rpc::HttpChainAdapter::request_raw_once`], and the wire policy is an argument
//! on the call rather than a guess from the method name; `Rejected` requires
//! [`read_send_refusal`] to say why *these bytes* could never have been taken; and the hash
//! comparison happens before the variant is built, so `Accepted` is only reachable by an
//! answer that names them.
//!
//! ```text
//! what the stub is        std::net::TcpListener on 127.0.0.1:0, one thread per arrival,
//!                         always answers eth_chainId with 91 342 so a connect can finish,
//!                         and answers every other method from a script of shapes — one
//!                         shape consumed per arrival, in arrival order
//! why it is not a mock    it stands in for a node's wire behaviour only. No pipeline runs,
//!                         no market is invented, no number here enters an evidence
//!                         directory as a chain fact, and nothing in this file is evidence
//!                         that a broadcast worked or that a transaction entered a block.
//! what it costs           no new dependency (tokio is this crate's own; the socket is std).
//!                         `a_response_that_comes_after_the_clients_own_timeout…` waits out
//!                         the adapter's own 20-second request timeout on purpose: it is the
//!                         only way to measure the shape the defect was worst in, and it is
//!                         the one test here that is not instant.
//! ```
//!
//! §8 holds throughout: the synthetic scalar-1 key (§40's, whose address M6 proved
//! unfunded), zero value, four bytes of calldata, and every byte going to a socket this test
//! process spawned on loopback. No node was deployed, no real RPC or Flashblocks endpoint was
//! contacted, nothing was signed with a real key and nothing was broadcast.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use serde_json::{json, Value};

use evm_execution::giwa::{parse_rpc_error, read_send_refusal, RefusalRead};
use evm_execution::{
    bind, EndpointKind, ExecutionKey, ExecutionLane, ExecutionMode, ExpectedTransaction,
    GiwaSequencerDirect, LaneRelease, NonceReading, Receipt, SignedTransaction, Signer,
    SubmissionEvidence, SubmissionOutcome, TransactionSubmitter, TransactionType,
    UnsignedTransaction,
};

/// §40's synthetic key: the scalar one, never the operator's wallet.
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
/// The same number on the wire, which is what the stub answers `eth_chainId` with.
const CHAIN_HEX: &str = "0x164ce";
const BASE_FEE: u64 = 371;
const TIP: u64 = 1_000_000;
const BLOCK: u64 = 37_486_792;
const TARGET: Address = Address::new([0x6bu8; 20]);

/// The credential-shaped thing planted in the stub's URL path, so a reason line that repeats
/// the configured endpoint is caught rather than trusted (§5, M12-B's scrub rule).
const TOKEN: &str = "abc123";

/// One arrival's answer. A script is consumed in arrival order, so a test can say "this is
/// what the endpoint did when it was asked, and this is what it would have done if it had
/// been asked again" — and that second entry sitting unconsumed *is* the proof, counted at
/// the socket, that no second post happened.
#[derive(Clone, Debug)]
enum Shape {
    /// 200 OK with this JSON-RPC `result`, given as JSON text: a hash, `null`, anything.
    Result(String),
    /// 200 OK whose body is not JSON at all.
    NotJson,
    /// 200 OK with an `error` object of this code and message.
    Error(i64, String),
    /// 200 OK whose `error` member is not the object §4 can read — here, a bare string.
    ErrorText(&'static str),
    /// A JSON body at HTTP 502, the status a gateway in front of a node answers with.
    BadGateway,
    /// Read the request to its last byte, then close the socket without a response.
    Silent,
    /// Sleep, then answer with the ack. Past the adapter's 20s timeout is how the timeout
    /// case is reached without changing the client's configuration.
    Slow(Duration, String),
}

struct Stub {
    url: String,
    /// One entry per arrival: the method and the `params[0]` text it carried. Recorded
    /// *before* the response is written, so a caller that has already read an answer is
    /// guaranteed to see the arrival in the tally — a tally appended after the write would
    /// turn every "no second send happened" assertion into a race with this thread.
    arrivals: Arc<Mutex<Vec<(String, String)>>>,
    script: Arc<Mutex<VecDeque<Shape>>>,
}

impl Stub {
    fn spawn(script: Vec<Shape>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port is available");
        let url = format!(
            "http://127.0.0.1:{}/{TOKEN}",
            listener.local_addr().expect("addr").port()
        );
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(script.into()));
        let counting = arrivals.clone();
        let scripted = script.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let counting = counting.clone();
                let scripted = scripted.clone();
                std::thread::spawn(move || Self::serve(stream, scripted, counting));
            }
        });
        Self {
            url,
            arrivals,
            script,
        }
    }

    /// Every method this socket was asked for, in arrival order.
    fn methods(&self) -> Vec<String> {
        self.arrivals
            .lock()
            .expect("the tally")
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    /// How many times `eth_sendRawTransaction` arrived. This is §7.A's number.
    fn sends(&self) -> usize {
        self.count("eth_sendRawTransaction")
    }

    fn count(&self, method: &str) -> usize {
        self.methods()
            .iter()
            .filter(|seen| seen.as_str() == method)
            .count()
    }

    /// The `params[0]` of the first send — the raw transaction bytes as the node saw them.
    fn first_send_payload(&self) -> String {
        self.arrivals
            .lock()
            .expect("the tally")
            .iter()
            .find(|(method, _)| method == "eth_sendRawTransaction")
            .map(|(_, param)| param.clone())
            .expect("the stub recorded a send payload")
    }

    /// Shapes still unconsumed.
    fn unconsumed(&self) -> usize {
        self.script.lock().expect("the script").len()
    }

    /// Replace the next scripted answer, once the socket's own URL is known. The gateway
    /// cases need a message that quotes the endpoint serving it, and a URL only exists after
    /// [`Stub::spawn`] has bound a port, so the shape cannot be written at spawn time.
    fn set_next(&self, shape: Shape) {
        let mut queue = self.script.lock().expect("an unlocked queue");
        queue.pop_front();
        queue.push_back(shape);
    }

    /// Read one JSON-RPC request, count it, and answer it with the next shape in the script.
    fn serve(
        stream: TcpStream,
        script: Arc<Mutex<VecDeque<Shape>>>,
        arrivals: Arc<Mutex<Vec<(String, String)>>>,
    ) {
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = rest.trim().parse().unwrap_or(0);
            }
            if line == "\r\n" {
                break;
            }
        }
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let param = request
            .get("params")
            .and_then(Value::as_array)
            .and_then(|args| args.first())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `eth_chainId` is answered from the wiring, not from the script: a stub that refused
        // it would not be an endpoint, it would be a socket no test could reach, and a connect
        // would consume the shape a test meant for the send.
        let shape = (method != "eth_chainId")
            .then(|| script.lock().expect("the script").pop_front())
            .flatten();
        arrivals
            .lock()
            .expect("the tally")
            .push((method.clone(), param));

        let response = match shape {
            None if method == "eth_chainId" => json_ok(format!(
                r#"{{"jsonrpc":"2.0","id":1,"result":"{CHAIN_HEX}"}}"#
            )),
            None => json_ok(
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"rpc method is not whitelisted"}}"#
                    .to_string(),
            ),
            Some(Shape::Result(text)) => json_ok(format!(r#"{{"jsonrpc":"2.0","id":1,"result":{text}}}"#)),
            Some(Shape::NotJson) => {
                let payload = "upstream connect error or disconnect/reset before headers";
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\
                     \r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                )
            }
            Some(Shape::Error(code, message)) => json_ok(format!(
                r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":{code},"message":"{message}"}}}}"#
            )),
            Some(Shape::ErrorText(text)) => json_ok(format!(
                r#"{{"jsonrpc":"2.0","id":1,"error":"{text}"}}"#
            )),
            Some(Shape::BadGateway) => format!(
                "{}\r\nconnection: close\r\n\r\n",
                json_body(
                    "502 Bad Gateway",
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"bad gateway"}}"#,
                )
            ),
            Some(Shape::Silent) => return,
            Some(Shape::Slow(after, text)) => {
                std::thread::sleep(after);
                json_ok(format!(r#"{{"jsonrpc":"2.0","id":1,"result":{text}}}"#))
            }
        };
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

/// A 200 OK carrying this JSON body.
fn json_ok(payload: String) -> String {
    envelope("200 OK", "application/json", payload)
}

/// The body of a JSON response, without its HTTP envelope.
fn json_body(_status: &'static str, payload: &str) -> String {
    payload.to_string()
}

fn envelope(status: &'static str, content_type: &str, payload: String) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\
         \r\nconnection: close\r\n\r\n{payload}",
        payload.len()
    )
}

/// One worth-nothing transaction, built and signed by the crate's own code over the
/// synthetic key, so the bytes the stub receives are the shape a real send takes.
fn signed(nonce: u64) -> SignedTransaction {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    let unsigned = UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id: CHAIN,
        nonce,
        to: Some(TARGET),
        value: U256::ZERO,
        gas_limit: 21_000,
        input: Bytes::from(vec![0x12u8, 0x34, 0x56, 0x78]),
        access_list: Vec::new(),
        max_priority_fee_per_gas: Some(U256::from(TIP)),
        max_fee_per_gas: Some(U256::from(BASE_FEE * 2 + TIP)),
    };
    Signer::from_key(ExecutionMode::SignOnly, key)
        .sign(&unsigned)
        .expect("the synthetic key signs")
}

/// The address the synthetic key signs as — M6 proved it unfunded, which is why nothing here
/// could spend anything even if it were broadcast, which it is not.
fn sender() -> Address {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(ExecutionMode::SignOnly, key)
        .address()
        .expect("a signer built from a key knows its address")
}

/// A hash that is well-formed and not this transaction's, for the shape where the node
/// answers about somebody else's bytes.
fn other_hash() -> B256 {
    B256::left_padding_from(&[0xeeu8; 20])
}

fn hash_text(hash: B256) -> String {
    format!("\"{hash:#x}\"")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One lane pointed at one stub, the stub kept alive so its tally can be read afterwards.
async fn lane(script: Vec<Shape>) -> (Stub, GiwaSequencerDirect) {
    let stub = Stub::spawn(script);
    let direct = GiwaSequencerDirect::connect(
        &stub.url,
        CHAIN,
        ExecutionMode::Submit,
        EndpointKind::Unknown,
    )
    .await
    .expect("the stub answers eth_chainId with the configured chain, so a connect finishes");
    (stub, direct)
}

fn reading(confirmed: u64, pending: u64) -> NonceReading {
    NonceReading {
        address: sender(),
        confirmed,
        pending,
        at_block: BLOCK,
        source: "scripted read in the M12-E §7 matrix".to_string(),
    }
}

/// What every answer owes §5: the socket is named by its digest, and neither the URL nor the
/// credential in its path travelled into the line.
fn assert_names_the_socket_and_no_credential(line: &str, digest: &str) {
    assert!(
        line.contains(digest),
        "the line has to name the endpoint the send went through, or two runs over two \
         sockets are indistinguishable: {line}"
    );
    for secret in [TOKEN, "127.0.0.1"] {
        assert!(
            !line.contains(secret),
            "`{secret}` is part of the endpoint URL, and §5 keeps URLs and credentials out of \
             a submission line, which is a line in evidence: {line}"
        );
    }
}

// ---------------------------------------------------------------------------
// §11's evidence channel
// ---------------------------------------------------------------------------

/// Append one measured row to `measured-rows.jsonl` in the directory `M12E_EVIDENCE_DIR`
/// names. Unset — which is every ordinary gate run — and nothing is written and no directory
/// is made; the variable is the whole of the file's mandate.
///
/// Three properties the committed tables depend on:
///
/// - every field is read off the same objects the case's own assertions just used —
///   [`Stub`]'s arrival tally and the [`SubmissionOutcome`] the send returned — so a table
///   cannot carry a figure no test observed;
/// - each `record` call sits *after* that case's assertions, so a wrong number panics the case
///   before it can be written down;
/// - a row describes one case, identified by the name of the `#[test]` function that wrote it,
///   which `send_uncertainty_evidence.rs` re-resolves against this file's source.
fn record(row: Value) {
    let Some(dir) = std::env::var_os("M12E_EVIDENCE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("measured-rows.jsonl"))
        .unwrap_or_else(|error| panic!("the evidence file could not be opened: {error}"));
    writeln!(file, "{row}").expect("an evidence row was appended");
}

/// The line the node's answer became, whichever variant it became. This is the text §5's
/// rules were graded against, so it is also the text the tables quote.
fn answer_line(outcome: &SubmissionOutcome) -> String {
    match outcome {
        SubmissionOutcome::Accepted { detail, .. } => detail.clone(),
        SubmissionOutcome::Rejected { reason, .. } => reason.clone(),
        SubmissionOutcome::Unknown { reason, .. } => reason.clone(),
    }
}

/// What the lifecycle did with that answer, in the one word the tables need.
fn release_word(release: &LaneRelease) -> &'static str {
    match release {
        LaneRelease::Released => "released",
        LaneRelease::Held { .. } => "held",
    }
}

/// One send case: what the socket counted, what the answer became, what the local hash did.
/// `measured` is the case's own extra reading — an error payload, a lane release, a leak
/// check — and may override `table` to put the row in the classification table instead.
fn record_send(
    section: &str,
    case: &str,
    fault: &str,
    stub: &Stub,
    outcome: &SubmissionOutcome,
    local: B256,
    measured: Value,
) {
    let answer = answer_line(outcome);
    let mut row = json!({
        "table": "request_counts",
        "section": section,
        "case": case,
        "fault": fault,
        "total_arrivals": stub.methods().len(),
        "send_arrivals": stub.sends(),
        "methods": stub.methods(),
        "unconsumed_shapes": stub.unconsumed(),
        "status_word": outcome.status_word(),
        "proven_not_in_flight": outcome.proven_not_in_flight(),
        "tracked_hash_equals_local": outcome.tracked_hash(local) == local,
        "local_hash": format!("{local:#x}"),
        "answer_line": answer,
    });
    if let Value::Object(extra) = measured {
        if let Some(map) = row.as_object_mut() {
            for (key, value) in extra {
                map.insert(key, value);
            }
        }
    }
    record(row);
}

/// A classification row measured without a socket: the answer [`read_send_refusal`] gives a
/// code and message, which is the row §4's table is made of.
fn classification_row(case: &str, fault: &str, code: i64, message: &str, read: &str) -> Value {
    json!({
        "table": "classification",
        "section": "7C",
        "case": case,
        "fault": fault,
        "graded_at": "classifier",
        "code": code,
        "message": message,
        "refusal_read": read,
    })
}

/// §7.A, case 1: the node took the bytes and named them. One arrival, and the only shape in
/// this matrix that reaches `Accepted`.
#[tokio::test]
async fn an_acknowledgement_of_these_bytes_is_one_post_and_the_only_accepted_answer() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![
        Shape::Result(hash_text(local)),
        Shape::Result(hash_text(local)),
    ])
    .await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("the stub answered the send");
    let SubmissionOutcome::Accepted {
        transaction_hash,
        detail,
        ..
    } = &outcome
    else {
        panic!(
            "the node named these exact bytes, so a `{}` answer is the defect",
            outcome.status_word()
        );
    };
    assert_eq!(*transaction_hash, Some(local));
    assert!(
        detail.contains(&format!("{local:#x}")),
        "the node's own answer is quoted: {detail}"
    );
    assert_names_the_socket_and_no_credential(detail, &digest);
    assert!(!outcome.proven_not_in_flight());
    assert_eq!(outcome.tracked_hash(local), local);

    assert_eq!(stub.sends(), 1, "one submit, one post");
    assert_eq!(
        stub.first_send_payload(),
        format!("0x{}", hex(transaction.raw().as_ref())),
        "the bytes the stub tallied are the bytes this build signed"
    );
    assert_eq!(
        stub.unconsumed(),
        1,
        "the second reply in the script was never asked for"
    );
    record_send(
        "7A",
        "an_acknowledgement_of_these_bytes_is_one_post_and_the_only_accepted_answer",
        "§7.A case 1: 200 OK whose result is the hash of these bytes",
        &stub,
        &outcome,
        local,
        json!({
            "signed_bytes_are_what_arrived": stub.first_send_payload()
                == format!("0x{}", hex(transaction.raw().as_ref())),
        }),
    );
}

/// §7.A, case 2: the endpoint read every byte of the request and then closed the socket
/// without a byte of response. This is §3.2's sentence in wire form.
#[tokio::test]
async fn a_socket_closed_after_reading_every_byte_is_one_post_and_an_unknown_answer() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![Shape::Silent, Shape::Result(hash_text(local))]).await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("no answer is not an error at this layer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "a closed connection proves nothing about what the node did with the bytes, so it \
             cannot be `{}`",
            outcome.status_word()
        );
    };
    assert!(
        !outcome.proven_not_in_flight(),
        "and the lane cannot be freed"
    );
    assert_eq!(
        outcome.tracked_hash(local),
        local,
        "the local hash stays tracked"
    );
    assert_names_the_socket_and_no_credential(reason, &digest);

    assert_eq!(
        stub.sends(),
        1,
        "the transport's second try is what this count exists to catch"
    );
    assert_eq!(
        stub.unconsumed(),
        1,
        "the answer that was waiting for a second post was never fetched"
    );
    record_send(
        "7A",
        "a_socket_closed_after_reading_every_byte_is_one_post_and_an_unknown_answer",
        "§7.A case 2: every request byte read, then the socket closed with no response",
        &stub,
        &outcome,
        local,
        json!({
            "payload_was_fully_read": stub.first_send_payload()
                == format!("0x{}", hex(transaction.raw().as_ref())),
        }),
    );
}

/// §7.A, case 3: the endpoint answers, but later than the client's own 20-second request
/// timeout. The dangerous half of this shape is that the node really did receive the bytes —
/// only the way back was lost — so a re-send here is a duplicate buy, not a retry.
///
/// This test waits the timeout out on purpose and takes roughly 22 seconds.
#[tokio::test]
async fn a_response_that_comes_after_the_clients_own_timeout_is_one_post() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![
        // Past `HttpChainAdapter`'s 20-second request timeout, so the client has given up
        // before the answer exists.
        Shape::Slow(Duration::from_secs(21), hash_text(local)),
        Shape::Result(hash_text(local)),
    ])
    .await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let started = std::time::Instant::now();
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a timeout is an unknown answer, not an error the lane cannot carry");
    let elapsed = started.elapsed();

    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "the client gave up after {elapsed:?}, so `{}` would be a claim about an answer \
             this process never read",
            outcome.status_word()
        );
    };
    assert_eq!(
        stub.sends(),
        1,
        "the timeout produced no second post — this is the case the retry loop used to answer \
         with two transactions: {:?}",
        stub.methods()
    );
    assert_eq!(
        stub.unconsumed(),
        1,
        "and the scripted answer to a second post was not fetched"
    );
    assert!(
        elapsed >= Duration::from_secs(20),
        "the wait is the adapter's own timeout rather than this test sleeping: {elapsed:?}"
    );
    assert!(
        reason.contains("no answer from eth_sendRawTransaction"),
        "the line says which question went unanswered: {reason}"
    );
    assert_names_the_socket_and_no_credential(reason, &digest);
    assert_eq!(outcome.tracked_hash(local), local);
    record_send(
        "7A",
        "a_response_that_comes_after_the_clients_own_timeout_is_one_post",
        "§7.A case 3: the answer arrives after the client's own request timeout",
        &stub,
        &outcome,
        local,
        json!({
            "client_waited_ms": elapsed.as_millis() as u64,
            "wait_was_the_adapters_own_timeout": elapsed >= Duration::from_secs(20),
        }),
    );
}

/// §7.A, case 4: a 200 OK whose body is not JSON.
#[tokio::test]
async fn a_body_that_is_not_json_is_one_post_and_an_unknown_answer() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![Shape::NotJson, Shape::Result(hash_text(local))]).await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("an unreadable body is still a answered socket");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "a body this build cannot parse cannot be read as `{}` either",
            outcome.status_word()
        );
    };
    assert!(!outcome.proven_not_in_flight());
    assert_eq!(outcome.tracked_hash(local), local);
    assert_names_the_socket_and_no_credential(reason, &digest);

    assert_eq!(
        stub.sends(),
        1,
        "a non-JSON body used to be a retry, which is §3's case 4"
    );
    assert_eq!(stub.unconsumed(), 1);
    record_send(
        "7A",
        "a_body_that_is_not_json_is_one_post_and_an_unknown_answer",
        "§7.A case 4: 200 OK whose body is not JSON",
        &stub,
        &outcome,
        local,
        Value::Null,
    );
}

/// §7.A, case 5: an HTTP status that is not a refusal — 502 from the gateway in front of the
/// node, which cannot prove the node never took the bytes.
#[tokio::test]
async fn an_http_status_that_is_not_a_refusal_is_one_post_and_an_unknown_answer() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![Shape::BadGateway, Shape::Result(hash_text(local))]).await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a gateway status is an answer-shaped non-answer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "a 502 says the request did not reach a node that would answer for it, which is \
             the opposite of `{}`",
            outcome.status_word()
        );
    };
    assert!(!outcome.proven_not_in_flight());
    assert_eq!(outcome.tracked_hash(local), local);
    assert_names_the_socket_and_no_credential(reason, &digest);

    assert_eq!(stub.sends(), 1, "a gateway status used to be retried too");
    assert_eq!(stub.unconsumed(), 1);
    record_send(
        "7A",
        "an_http_status_that_is_not_a_refusal_is_one_post_and_an_unknown_answer",
        "§7.A case 5: HTTP 502 from the gateway in front of the node",
        &stub,
        &outcome,
        local,
        json!({}),
    );
}

/// §7.A, case 6 with §7.C: a JSON-RPC error object is one post whichever way it is read,
/// because an error is the node answering, and an answer was never the retry case. Both
/// classifications are graded on the same count.
#[tokio::test]
async fn a_json_rpc_error_is_one_post_whichever_way_it_is_read() {
    for (code, message) in [
        (-32003, "intrinsic gas too low"),
        (-32003, "nonce too low"),
        (-32003, "already known"),
    ] {
        let transaction = signed(7);
        let (stub, direct) = lane(vec![
            Shape::Error(code, message.to_string()),
            Shape::Result(hash_text(transaction.hash())),
        ])
        .await;
        let outcome = direct
            .submit(&transaction)
            .await
            .expect("a JSON-RPC error is an answer");
        assert_eq!(
            stub.sends(),
            1,
            "`{message}` was answered once and must be asked about once: {:?}",
            stub.methods()
        );
        assert_eq!(
            stub.unconsumed(),
            1,
            "`{message}` is terminal in both directions — neither a refusal that earns a \
             second try nor an unknown that earns one"
        );
        assert!(
            outcome.status_word() != "submitted",
            "`{message}` cannot be an acknowledgement"
        );
        record_send(
            "7A",
            "a_json_rpc_error_is_one_post_whichever_way_it_is_read",
            "§7.A case 6: 200 OK with a JSON-RPC error object, read both ways",
            &stub,
            &outcome,
            transaction.hash(),
            json!({ "code": code, "message": message }),
        );
    }
}

/// §9's sixth audit item, made a test rather than a reading of the code: a gateway that answers
/// with an error whose message quotes the very URL it was called through — the shape a
/// reverse proxy's "no route" line takes, and the one way a submission line can end up in
/// `submissions.jsonl` carrying an API key or a JWT path. Both classification branches are
/// graded, because both write the node's words into a reason.
#[tokio::test]
async fn an_error_message_that_quotes_the_endpoint_arrives_scrubbed() {
    let digest_of = |url: &str| evm_chain::endpoint_id(url);

    // Branch 1: an unclassifiable message, so the answer is Unknown.
    let transaction = signed(7);
    let (stub, direct) = lane(vec![Shape::Error(-32000, String::new())]).await;
    let echo = format!("no route for {} in this gateway", stub.url);
    stub.set_next(Shape::Error(-32000, echo));
    let digest = digest_of(&stub.url);
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a gateway's error object is an answer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "a message that names no payload property cannot be `{}`",
            outcome.status_word()
        );
    };
    assert_names_the_socket_and_no_credential(reason, &digest);
    assert!(
        reason.contains("no route for") && reason.contains("in this gateway"),
        "scrubbing replaces the URL, not the sentence — the node's own words stay readable: \
         {reason}"
    );
    assert_eq!(stub.sends(), 1, "and one post, as everywhere else here");
    record_send(
        "9.6",
        "an_error_message_that_quotes_the_endpoint_arrives_scrubbed",
        "§9 item 6, branch 1: an error message quoting the URL the send was made through",
        &stub,
        &outcome,
        transaction.hash(),
        json!({
            "classification_branch": "unknown",
            "names_endpoint_digest": reason.contains(&digest),
            "carries_the_url": reason.contains(&stub.url),
            "carries_the_credential": reason.contains(TOKEN),
            "keeps_the_nodes_words": reason.contains("no route for"),
        }),
    );

    // Branch 2: a message that starts with a listed payload fact, so the answer is Rejected
    // and still must not carry the socket's credential.
    let transaction = signed(8);
    let (stub, direct) = lane(vec![Shape::Error(-32003, String::new())]).await;
    stub.set_next(Shape::Error(
        -32003,
        format!("intrinsic gas too low, as reported at {}", stub.url),
    ));
    let digest = digest_of(&stub.url);
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a refusal is an answer");
    let SubmissionOutcome::Rejected { reason, .. } = &outcome else {
        panic!(
            "`intrinsic gas too low` is a property of the payload, so it is not `{}`",
            outcome.status_word()
        );
    };
    assert_names_the_socket_and_no_credential(reason, &digest);
    assert_eq!(stub.sends(), 1);
    record_send(
        "9.6",
        "an_error_message_that_quotes_the_endpoint_arrives_scrubbed",
        "§9 item 6, branch 2: a refusal whose message quotes the URL as well",
        &stub,
        &outcome,
        transaction.hash(),
        json!({
            "classification_branch": "rejected",
            "names_endpoint_digest": reason.contains(&digest),
            "carries_the_url": reason.contains(&stub.url),
            "carries_the_credential": reason.contains(TOKEN),
            "keeps_the_nodes_words": reason.contains("intrinsic gas too low"),
        }),
    );
}

/// §7.A, case 7: a 200 OK carrying a well-formed 32-byte hash that is *not* the hash of these
/// bytes. §5's 「不一致时不得继续走正常 Accepted 成功路径」.
#[tokio::test]
async fn a_hash_that_is_not_ours_is_one_post_and_never_an_acknowledgement() {
    let transaction = signed(7);
    let local = transaction.hash();
    let stranger = other_hash();
    let (stub, direct) = lane(vec![
        Shape::Result(hash_text(stranger)),
        Shape::Result(hash_text(local)),
    ])
    .await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("an answer about another transaction is still an answer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "the node named {stranger:#x} while these bytes hash to {local:#x}; `{}` is the \
             defect this case exists to keep out",
            outcome.status_word()
        );
    };
    assert!(
        reason.contains(&format!("{stranger:#x}")) && reason.contains(&format!("{local:#x}")),
        "both hashes belong in the line, so a reader can tell which one the node meant: \
         {reason}"
    );
    assert_names_the_socket_and_no_credential(reason, &digest);
    assert!(!outcome.proven_not_in_flight());
    assert_eq!(
        outcome.tracked_hash(local),
        local,
        "and the node's number never redirects what this process tracks"
    );

    let row = SubmissionEvidence::from_outcome(local, &outcome, 1_790_000_000_000);
    assert_eq!(
        row.transaction_hash, local,
        "the evidence row tracks the local hash"
    );
    assert_eq!(row.outcome, "unknown", "and does not call it submitted");

    assert_eq!(stub.sends(), 1);
    assert_eq!(
        stub.unconsumed(),
        1,
        "a mismatched answer is terminal, not a reason to ask again"
    );
    record_send(
        "7A",
        "a_hash_that_is_not_ours_is_one_post_and_never_an_acknowledgement",
        "§7.A case 7: 200 OK naming a hash that is not these bytes'",
        &stub,
        &outcome,
        local,
        json!({
            "node_returned_hash": format!("{stranger:#x}"),
            "hashes_agree": false,
            "evidence_outcome": row.outcome,
            "evidence_tracks_local_hash": row.transaction_hash == local,
        }),
    );
}

/// §7.B, the acceptance case: the endpoint reads and records the raw bytes, takes them, and
/// the answer never comes back. One post; the local hash retained; the nonce lane still held.
#[tokio::test]
async fn the_endpoint_took_the_bytes_and_the_answer_never_arrived_sends_once_and_holds_the_lane() {
    let transaction = signed(7);
    let local = transaction.hash();
    let (stub, direct) = lane(vec![
        Shape::Silent,
        // What the same socket would have said to a second post. Left unconsumed below, which
        // is the assertion that no second post was made — counted at the endpoint.
        Shape::Result(hash_text(local)),
    ])
    .await;

    let mut lane_state = ExecutionLane::new();
    let nonce = lane_state
        .allocate(&reading(7, 7))
        .expect("an idle lane takes the pending nonce");
    assert_eq!(nonce, 7);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("the send was handed over; whether it was answered is the open question");

    // 1. Exactly one request reached the endpoint inside this call.
    assert_eq!(
        stub.sends(),
        1,
        "one submit, one post — §3.2's whole property"
    );
    assert_eq!(
        stub.first_send_payload(),
        format!("0x{}", hex(transaction.raw().as_ref())),
        "and what it received is this transaction's own bytes, so the tally is about the \
         transaction this lane reserved a nonce for"
    );

    // 2. The local hash is what the lane keeps, with no answer to take it from.
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "`{}` would be a claim about an answer this process never read",
            outcome.status_word()
        );
    };
    assert_eq!(outcome.tracked_hash(local), local);
    assert_eq!(outcome.status_word(), "unknown");
    let row = SubmissionEvidence::from_outcome(local, &outcome, 1_790_000_000_000);
    assert_eq!(
        row.transaction_hash, local,
        "the row tracks the locally computed hash"
    );
    assert_eq!(
        row.outcome, "unknown",
        "the submission line answers the send question, not the inclusion question"
    );
    assert!(
        row.was_sent(),
        "and it is honest that the bytes did go over the wire — what remains open is whether \
         they landed, which only a receipt read answers"
    );
    assert!(
        !reason.contains(&format!("{:#x}", other_hash())),
        "no hash this process did not compute enters the line: {reason}"
    );

    // 3. The nonce lane is not released, and a second transaction on it is refused.
    let release = lane_state.resolve_submission(&outcome);
    let LaneRelease::Held { reason } = &release else {
        panic!("an unknown answer that frees a nonce is the duplicate-buy defect itself");
    };
    assert!(
        reason.contains("unknown"),
        "the held lane names the answer class it is waiting on: {reason}"
    );
    assert_eq!(lane_state.outstanding(), Some((sender(), 7)));
    let error = lane_state
        .allocate(&reading(7, 7))
        .expect_err("a lane still holding nonce 7 may not hand out 7 again");
    assert!(
        error.to_string().contains("holding nonce 7"),
        "the refusal names the nonce it is protecting: {error}"
    );
    assert_eq!(
        stub.sends(),
        1,
        "and holding the lane sent nothing on its own — §6 forbids an automatic re-send, and \
         this is that line read as a request count"
    );
    assert_eq!(
        stub.unconsumed(),
        1,
        "the scripted answer to a second post stays unconsumed"
    );
    record_send(
        "7B",
        "the_endpoint_took_the_bytes_and_the_answer_never_arrived_sends_once_and_holds_the_lane",
        "§7.B: the endpoint read and recorded the raw bytes, and no answer came back",
        &stub,
        &outcome,
        local,
        json!({
            "lane_release": release_word(&release),
            "outstanding_still_carries_the_nonce": lane_state.outstanding() == Some((sender(), 7)),
            "second_allocate_refused": error.to_string().contains("holding nonce 7"),
            "evidence_outcome": row.outcome,
            "evidence_tracks_local_hash": row.transaction_hash == local,
            "evidence_marks_bytes_as_sent": row.was_sent(),
        }),
    );
}

/// §7.C: the answers this build reads as a definite refusal of *these bytes*, graded at the
/// wire and at the lane. Each case also counts its own arrivals, because §4's rule that a
/// refusal is the only answer that may free a nonce is worthless if the freed lane arrives
/// with a second post attached.
#[tokio::test]
async fn only_a_refusal_of_the_payload_itself_clears_the_lane() {
    let cases = [
        (-32003, "intrinsic gas too low"),
        (-32000, "transaction type not supported"),
        (
            -32000,
            "max priority fee per gas higher than max fee per gas",
        ),
        (-32000, "negative value"),
        (-32000, "invalid chain id"),
        (-32601, "rpc method is not whitelisted"),
        // The code alone carries this one: an endpoint that does not serve the method has
        // refused the call, and no payload travels behind that refusal.
        (-32601, "the method is served nowhere here"),
    ];
    for (code, message) in cases {
        let transaction = signed(7);
        let local = transaction.hash();
        let (stub, direct) = lane(vec![Shape::Error(code, message.to_string())]).await;
        let mut lane_state = ExecutionLane::new();
        lane_state
            .allocate(&reading(7, 7))
            .expect("an idle lane takes the pending nonce");

        let outcome = direct
            .submit(&transaction)
            .await
            .expect("a refusal is an answer");
        let SubmissionOutcome::Rejected { reason, .. } = &outcome else {
            panic!(
                "`{message}` is a fact about this payload rather than about the account's \
                 state, so it has to be a refusal, not `{}`",
                outcome.status_word()
            );
        };
        assert!(
            outcome.proven_not_in_flight(),
            "and it is the only class that may prove it: {reason}"
        );
        assert!(
            reason.contains(message),
            "the node's own words are quoted: {reason}"
        );
        assert_eq!(
            outcome.tracked_hash(local),
            local,
            "even a refusal is tracked by the hash we computed, not by anything returned"
        );
        let release = lane_state.resolve_submission(&outcome);
        assert_eq!(
            release,
            LaneRelease::Released,
            "a refused nonce is spendable again"
        );
        assert_eq!(stub.sends(), 1, "one post for `{message}`");
        record_send(
            "7C",
            "only_a_refusal_of_the_payload_itself_clears_the_lane",
            "§7.C: a refusal this build can cite, graded at the wire and at the lane",
            &stub,
            &outcome,
            local,
            json!({
                "table": "classification",
                "graded_at": "wire",
                "code": code,
                "message": message,
                "lane_release": release_word(&release),
            }),
        );
    }
}

/// §7.C, the other side: the answers §4 names — and the ones this build cannot cite at all —
/// are unknown answers, and an unknown answer may not free a nonce.
#[tokio::test]
async fn the_state_dependent_answers_are_unknown_and_hold_the_lane() {
    let cases = [
        (-32003, "already known"),
        (-32003, "known transaction"),
        (-32003, "nonce too low"),
        (-32003, "replacement transaction underpriced"),
        // Not in §4's list, and refused for the same reason: every one of these is an answer
        // about the account's state or the pool's contents rather than about these bytes.
        (-32003, "nonce too high"),
        (-32003, "insufficient funds"),
        (-32000, "transaction gas limit exceeds block gas limit"),
        (
            4_242_424,
            "a code and message this repository has never seen",
        ),
        (-32603, "internal error"),
    ];
    for (code, message) in cases {
        let transaction = signed(7);
        let local = transaction.hash();
        let (stub, direct) = lane(vec![Shape::Error(code, message.to_string())]).await;
        let mut lane_state = ExecutionLane::new();
        lane_state
            .allocate(&reading(7, 7))
            .expect("an idle lane takes the pending nonce");

        let outcome = direct
            .submit(&transaction)
            .await
            .expect("an error payload is an answer");
        let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
            panic!(
                "`{message}` cannot be read as `{}`: this build cannot cite an interface \
                 guarantee that separates a refusal of a submission from an answer about one \
                 it already holds",
                outcome.status_word()
            );
        };
        assert!(
            !outcome.proven_not_in_flight(),
            "and it may not free the nonce: {reason}"
        );
        assert!(
            reason.contains(message),
            "the message is still quoted: {reason}"
        );
        assert_eq!(outcome.tracked_hash(local), local);
        let release = lane_state.resolve_submission(&outcome);
        let LaneRelease::Held { .. } = &release else {
            panic!("`{message}` released a lane that §4 says is still in question");
        };
        assert_eq!(stub.sends(), 1, "one post for `{message}`");
        record_send(
            "7C",
            "the_state_dependent_answers_are_unknown_and_hold_the_lane",
            "§7.C: an answer about state or the pool, graded at the wire and at the lane",
            &stub,
            &outcome,
            local,
            json!({
                "table": "classification",
                "graded_at": "wire",
                "code": code,
                "message": message,
                "lane_release": release_word(&release),
            }),
        );
    }

    // The shape §4's 「不得仅凭存在 error 字段就释放执行状态」 is literally about: an `error`
    // member that is not a readable object.
    let transaction = signed(7);
    let (stub, direct) = lane(vec![Shape::ErrorText("gateway said no in its own words")]).await;
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("an unreadable error is still an answer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "an `error` field this build cannot parse proves even less than one it can, so it \
             cannot be `{}`",
            outcome.status_word()
        );
    };
    assert!(!outcome.proven_not_in_flight());
    assert!(
        reason.contains("cannot parse"),
        "and the line says that rather than inventing a meaning: {reason}"
    );
    assert_eq!(stub.sends(), 1);
    record_send(
        "7C",
        "the_state_dependent_answers_are_unknown_and_hold_the_lane",
        "§7.C: an `error` member that is not a readable object at all",
        &stub,
        &outcome,
        transaction.hash(),
        json!({
            "table": "classification",
            "graded_at": "wire",
            "code": Value::Null,
            "message": "gateway said no in its own words",
        }),
    );
}

/// §7.C as a pure function, so the classification table is readable in one place and a wrong
/// row is a failing case rather than a lost lane. Two rules are being graded here, and they
/// point in opposite directions on purpose: a refusal is citable when the *message* describes
/// the payload, whichever number arrived with it, and a message that only describes the
/// node's state stays uncertain whichever number arrived with it. Prefix position is part of
/// the first rule — a real refusal buried in the middle of a gateway's own sentence is an
/// answer this build reads as uncertain, not as a licence to free a nonce.
#[test]
fn the_refusal_table_is_a_short_list_of_payload_facts_and_everything_else_is_unknown() {
    let definite = [
        (-32601, "rpc method is not whitelisted"),
        (-32003, "Intrinsic Gas Too Low"),
        (-32000, "intrinsic gas too low: have 21000, want 21204"),
        (-32000, "transaction type not supported"),
        (
            -32000,
            "max priority fee per gas higher than max fee per gas",
        ),
        (-32000, "negative value"),
        (-32000, "invalid chain id"),
        // A code this build has never measured, carrying a message that describes the payload
        // rather than the node's state. §4 asks for a *verifiable meaning*, and this one is
        // verifiable from the bytes alone — a gas limit under its own intrinsic cost is not a
        // transaction any node could have accepted at any block. The number the gateway chose
        // is quoted into the reason and decides nothing, which is what keeps this row honest
        // in both directions: it is why an unknown code cannot turn `nonce too low` into a
        // refusal either.
        (-32001, "intrinsic gas too low"),
    ];
    for (code, message) in definite {
        assert!(
            matches!(read_send_refusal(code, message), RefusalRead::Definite(_)),
            "`{message}` at code {code} is a property of the payload, so it is citable"
        );
        record(classification_row(
            "the_refusal_table_is_a_short_list_of_payload_facts_and_everything_else_is_unknown",
            "§7.C: the classifier asked directly, no socket in the way",
            code,
            message,
            "definite",
        ));
    }

    let uncertain = [
        (-32003, "nonce too low"),
        (-32003, "already known"),
        (-32003, "known transaction"),
        (-32003, "replacement transaction underpriced"),
        (-32003, "insufficient funds"),
        // The same words, not at the start: a wrapped answer is read as what it is — an
        // answer this build cannot hold a nonce release behind.
        (
            -32000,
            "internal: intrinsic gas too low, reported late in a wrapper sentence",
        ),
        // `method not found` is the one meaning this build reads from a number, and only from
        // JSON-RPC's own -32601; the same words under any other code are a wrapper's prose.
        (-32603, "method not found but under the wrong code"),
    ];
    for (code, message) in uncertain {
        assert!(
            matches!(read_send_refusal(code, message), RefusalRead::Uncertain(_)),
            "`{message}` at code {code} is an answer this build cannot stand a nonce release \
             behind"
        );
        record(classification_row(
            "the_refusal_table_is_a_short_list_of_payload_facts_and_everything_else_is_unknown",
            "§7.C: the classifier asked directly, no socket in the way",
            code,
            message,
            "uncertain",
        ));
    }

    // `parse_rpc_error`'s three rejections and one acceptance.
    assert_eq!(parse_rpc_error(r#""gateway said no""#), None);
    assert_eq!(parse_rpc_error(r#"{"code":-32003}"#), None);
    assert_eq!(parse_rpc_error("not json at all"), None);
    assert_eq!(
        parse_rpc_error(r#"{"code":-32003,"message":"nonce too low"}"#),
        Some((-32003, "nonce too low".to_string()))
    );
    record(json!({
        "table": "classification",
        "section": "7C",
        "case": "the_refusal_table_is_a_short_list_of_payload_facts_and_everything_else_is_unknown",
        "fault": "§7.C: the four shapes of an `error` member, read by parse_rpc_error",
        "graded_at": "payload reader",
        "rejects_a_bare_string": parse_rpc_error(r#""gateway said no""#).is_none(),
        "rejects_an_object_without_a_message": parse_rpc_error(r#"{"code":-32003}"#).is_none(),
        "rejects_a_body_that_is_not_json": parse_rpc_error("not json at all").is_none(),
        "reads_a_code_and_a_message": parse_rpc_error(
            r#"{"code":-32003,"message":"nonce too low"}"#
        ) == Some((-32003, "nonce too low".to_string())),
    }));
}

/// §7.D, the read side of E1: the same fault on a *read* still gets the transport's second
/// try. M8.1's latency numbers and M8.6's request census were measured against that \
/// behaviour, so the send policy is only a fix if it left the reads alone — and the pair of
/// tests here is the pair §3 asks for.
#[tokio::test]
async fn the_read_side_still_asks_twice_when_the_first_socket_dies() {
    let (stub, direct) = lane(vec![Shape::Silent, Shape::Result("null".to_string())]).await;
    let hash = signed(7).hash();

    let receipt = direct
        .receipt(hash)
        .await
        .expect("a read that lands on the second attempt is a successful read");
    assert_eq!(
        receipt, None,
        "and a null result is §26's legitimate not-yet"
    );

    assert_eq!(
        stub.count("eth_getTransactionReceipt"),
        2,
        "the read retry loop is untouched: {:?}",
        stub.methods()
    );
    assert_eq!(
        stub.sends(),
        0,
        "and no send was asked of this socket at all"
    );
    record(json!({
        "table": "request_counts",
        "section": "7D",
        "case": "the_read_side_still_asks_twice_when_the_first_socket_dies",
        "fault": "§7.D: a dead socket on a *read* (eth_getTransactionReceipt)",
        "total_arrivals": stub.methods().len(),
        "read_arrivals": stub.count("eth_getTransactionReceipt"),
        "send_arrivals": stub.sends(),
        "methods": stub.methods(),
        "unconsumed_shapes": stub.unconsumed(),
    }));
}

/// §7.D, the two halves side by side on one socket: the identical fault is counted once by a
/// send and twice by a read, and the difference is the policy argument on the call, not the
/// method name.
#[tokio::test]
async fn the_same_fault_counts_once_on_a_send_and_twice_on_a_read() {
    let transaction = signed(7);
    let (stub, direct) = lane(vec![
        Shape::Silent,
        Shape::Silent,
        Shape::Result("null".to_string()),
    ])
    .await;

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("the send was handed over");
    assert_eq!(outcome.status_word(), "unknown");
    assert_eq!(
        stub.sends(),
        1,
        "the send is one post, and the loop that would have made it two is still there for \
         the read: {:?}",
        stub.methods()
    );

    let receipt = direct
        .receipt(transaction.hash())
        .await
        .expect("the read takes the second shape and then the third");
    assert_eq!(receipt, None);
    assert_eq!(
        stub.count("eth_getTransactionReceipt"),
        2,
        "one death and one recovery, which is exactly what M8 measured on the read path: {:?}",
        stub.methods()
    );
    assert_eq!(
        stub.unconsumed(),
        0,
        "every scripted arrival was used, by the side that uses it"
    );
    record_send(
        "7D",
        "the_same_fault_counts_once_on_a_send_and_twice_on_a_read",
        "§7.D: one dead socket, asked first as a send then as a read",
        &stub,
        &outcome,
        transaction.hash(),
        json!({
            "read_arrivals": stub.count("eth_getTransactionReceipt"),
            "arrivals_in_order": stub.methods(),
        }),
    );
}

/// §7.D, and §5's last sentence: `Accepted` is the send answer and never an inclusion claim.
/// The receipt side is graded through the crate's own binding rule, which is the thing a
/// node-named hash would have fooled.
#[tokio::test]
async fn an_acknowledgement_is_not_an_inclusion_and_a_receipt_binds_to_the_local_hash_only() {
    let transaction = signed(7);
    let local = transaction.hash();
    let stranger = other_hash();
    let (stub, direct) = lane(vec![Shape::Result(hash_text(local))]).await;

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("the stub acknowledged these bytes");
    assert_eq!(
        outcome.status_word(),
        "submitted",
        "the highest word a send may say"
    );
    let row = SubmissionEvidence::from_outcome(local, &outcome, 1_790_000_000_000);
    assert!(
        row.receipt_block.is_none() && row.receipt_status.is_none(),
        "the submission line carries no block field, because none was read"
    );
    assert_eq!(stub.sends(), 1);

    let expected = ExpectedTransaction {
        transaction_hash: local,
        sender: sender(),
        target: Some(TARGET),
        nonce: 7,
        chain_id: CHAIN,
    };
    let local_binding = bind(&receipt(local), &expected);
    assert!(local_binding.is_ok(), "a receipt for the local hash binds");
    let error = bind(&receipt(stranger), &expected)
        .expect_err("a receipt for the hash a node named must not bind to these bytes");
    assert!(
        error.contains(&format!("{stranger:#x}")),
        "and the refusal says which hash it is talking about: {error}"
    );
    record_send(
        "7D",
        "an_acknowledgement_is_not_an_inclusion_and_a_receipt_binds_to_the_local_hash_only",
        "§7.D: an acknowledged send, then a receipt read bound to the local hash",
        &stub,
        &outcome,
        local,
        json!({
            "evidence_outcome": row.outcome.clone(),
            "evidence_receipt_block_present": row.receipt_block.is_some(),
            "evidence_receipt_status_present": row.receipt_status.is_some(),
            "local_receipt_bound": local_binding.is_ok(),
            "node_named_receipt_refused": error.contains(&format!("{stranger:#x}")),
        }),
    );
}

/// A receipt in the shape M6 measured on this chain, addressed to one hash.
fn receipt(transaction_hash: B256) -> Receipt {
    Receipt {
        transaction_hash,
        block_number: BLOCK + 1,
        block_hash: B256::left_padding_from(&[9u8; 20]),
        transaction_index: 2,
        success: true,
        gas_used: 21_000,
        effective_gas_price: U256::from(BASE_FEE * 2 + TIP),
        cumulative_gas_used: Some(U256::from(84_000u64)),
        from: sender(),
        to: Some(TARGET),
        contract_address: None,
        tx_type: Some(2),
        logs: Vec::new(),
        l1_fee: Some(U256::from(7_400_000_000u64)),
        l1_gas_price: Some(U256::from(1_088_519_061u64)),
        l1_gas_used: Some(U256::from(1_600u64)),
        l1_base_fee_scalar: Some(U256::from(1_368u64)),
        l1_blob_base_fee: None,
        l1_blob_base_fee_scalar: None,
        provenance: "eth_getTransactionReceipt in the M12-E §7 matrix".to_string(),
    }
}

/// §9's seventh audit item, answered with numbers rather than a reading of the loop: the
/// attempt list the trace records for a call equals the count of HTTP requests this socket
/// actually received for it — one line for a one-shot send, two for a retried read, on the
/// same socket in the same run. An `attempts` array that silently kept saying "2" after the
/// send policy changed would make every published latency table wrong about the send path,
/// and this is the case that stops that.
#[tokio::test]
async fn the_trace_records_one_attempt_per_request_the_socket_saw() {
    use evm_chain::rpc_trace::{RpcTraceSink, RpcTraceSource};

    // The send dies at the transport, then a read dies once and lands on its second try.
    let stub = Stub::spawn(vec![
        Shape::Silent,
        Shape::Silent,
        Shape::Result("null".to_string()),
    ]);
    let sink = RpcTraceSink::new(
        std::time::Instant::now(),
        "m12e-attempt-accounting",
        RpcTraceSource::Fixture,
        Some(CHAIN),
    )
    .with_endpoint(&stub.url);
    let direct = GiwaSequencerDirect::connect_with_trace(
        &stub.url,
        CHAIN,
        ExecutionMode::Submit,
        EndpointKind::Unknown,
        Some(sink.clone()),
    )
    .await
    .expect("the stub answers eth_chainId, so a traced connect finishes");

    let transaction = signed(7);
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a dead socket is an answer this call reports");
    assert_eq!(outcome.status_word(), "unknown");
    let _ = direct
        .receipt(transaction.hash())
        .await
        .expect("a read that recovers on its second try is a successful read");

    // What the socket counted, and what the trace wrote down, request for request.
    let seen = |method: &str| {
        sink.events()
            .iter()
            .filter(|event| event.method == method)
            .map(|event| event.attempts.len())
            .sum::<usize>()
    };
    assert_eq!(
        sink.dropped_events(),
        0,
        "and nothing was thrown away to make those numbers agree"
    );
    assert_eq!(
        seen("eth_sendRawTransaction"),
        stub.sends(),
        "one send arrival, one recorded attempt: {:?}",
        stub.methods()
    );
    assert_eq!(seen("eth_sendRawTransaction"), 1);
    assert_eq!(
        seen("eth_getTransactionReceipt"),
        stub.count("eth_getTransactionReceipt"),
        "two read arrivals, two recorded attempts"
    );
    assert_eq!(seen("eth_getTransactionReceipt"), 2);
    // The whole ledger: every method's attempt count equals this socket's arrival count,
    // including the connect's chain id check.
    let total_attempts: usize = sink.events().iter().map(|event| event.attempts.len()).sum();
    assert_eq!(
        total_attempts,
        stub.methods().len(),
        "the trace neither invents a request nor loses one: {:?}",
        stub.methods()
    );
    record(json!({
        "table": "trace_accounting",
        "section": "9.7",
        "case": "the_trace_records_one_attempt_per_request_the_socket_saw",
        "fault": "§9 item 7: one dead send and one read that dies then recovers, on one socket",
        "total_arrivals": stub.methods().len(),
        "arrivals_in_order": stub.methods(),
        "trace_send_attempts": seen("eth_sendRawTransaction"),
        "socket_send_arrivals": stub.sends(),
        "trace_receipt_attempts": seen("eth_getTransactionReceipt"),
        "socket_receipt_arrivals": stub.count("eth_getTransactionReceipt"),
        "trace_total_attempts": total_attempts,
        "dropped_events": sink.dropped_events(),
    }));
}
