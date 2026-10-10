//! M12-D §5: what a submission answer is allowed to claim about the endpoint it went through.
//!
//! The audit plan (§2) put the whole transaction path under this question — the read purpose
//! `--rpc-url` is configured for, the endpoint the lane simulates against, the one it signs
//! and submits against, and the label each of those ends up in evidence — and the code answer
//! is in `ExecutionStage::connect` and `SequenceStage::connect_with_trace`: one
//! `GiwaSequencerDirect` built once and cloned into the lane's four ability slots, so reads
//! and `eth_sendRawTransaction` share one socket and one URL. That part is right, and it is
//! not what this file is for.
//!
//! What it found is the *word* the submission line carried. `public_http_rpc` names who runs
//! the node, and nothing this process can read says that:
//!
//! ```text
//! the endpoint the lane posts to may forward the bytes onward under a node-side setting
//! (`--rollup.sequencerhttp`) that no read in this repository ever sees, so a socket can be
//! public at its edge and remote at its destination — and a localhost socket can be the
//! public one's front. The class word was a guess wearing a fact's clothes.
//! ```
//!
//! So the label became conservative and the identity became a digest: `unknown`
//! (`EndpointKind::Unknown`) beside `rpc-…` (`evm_chain::endpoint_id`) of the URL the lane
//! actually used, forward-only — every committed M6/M7 row keeps the word its run wrote, and
//! `real_validation_receipt.rs`'s `the_committed_rows_keep_the_labels_the_runs_actually_wrote`
//! is the gate for that; this file does not touch it.
//!
//! The four things asserted per shape are the four §5 checks this side of the path answers to:
//! the socket is named (`endpoint_id`), the class is not claimed (no `public`/`local`/
//! `flashblocks`/`sequencer`/`recorded` in the line), the read role's label did not cross over
//! (a *read* type appearing anywhere in this crate's code is the failure, scanned in
//! `the_read_sides_label_never_crosses_into_the_submission_path`), and no credential travelled
//! with it — which is why the stub's URL is shaped `http://127.0.0.1:{port}/token/abc123`: a
//! path token and an authority are both things a line must not repeat, and `127.0.0.1` is also
//! the standing proof of §5's first bullet, that a localhost socket is not automatically
//! labelled local.
//!
//! ```text
//! what the stub is        std::net::TcpListener on 127.0.0.1:0, one thread per connection,
//!                         always answers eth_chainId with 91 342 so a connect can finish,
//!                         and answers eth_sendRawTransaction in whichever of four shapes the
//!                         test asked for
//! why it is not a mock    it stands in for a node's wire behaviour only. No pipeline runs, no
//!                         market is invented, no number here goes into an evidence directory,
//!                         and no gate reads this file.
//! what it costs           no new dependency (tokio is this crate's own; the socket is std).
//!                         `an_answer_that_is_not_an_acknowledgement…` sees the dropped shape
//!                         once, because M12-E §3 put the send on a one-shot wire policy — that
//!                         count is measured here, not claimed as a design.
//! ```
//!
//! §8's constraint holds throughout: the synthetic scalar-1 key (§40's, whose address M6
//! proved unfunded), zero value, empty calldata, and every byte going to a socket this test
//! process spawned on loopback. No real node, no real chain, no broadcast — and no node was
//! deployed for this milestone (§9/§12).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alloy_primitives::{Address, Bytes, U256};
use serde_json::Value;

use evm_execution::{
    giwa::{receipt_provenance, submission_provenance},
    EndpointKind, ExecutionKey, ExecutionMode, GiwaSequencerDirect, SignedTransaction, Signer,
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

/// The credential-shaped thing planted in the stub's URL path. A configured endpoint may
/// carry a key or a JWT exactly here, and a submission line that repeats the URL would put
/// it in committed evidence, so every shape test asks that it does not.
const TOKEN: &str = "abc123";

/// Every class word a submission line may not say, because each one is a claim about who
/// runs the endpoint or what it is for rather than a fact a send can establish. `local` is
/// in the list for §5's first bullet: a localhost socket is where this test happens to run.
const CLASS_WORDS: [&str; 5] = ["public", "local", "flashblocks", "sequencer", "recorded"];

/// What the stub does with `eth_sendRawTransaction`. `eth_chainId` is always answered — a
/// stub that refused it would not be an endpoint, it would be a socket no test could reach.
#[derive(Clone, Debug)]
enum Send {
    /// The node accepted the bytes and named them: the hash the test computed locally.
    Ack(String),
    /// A JSON-RPC error object. The node answered, and said no (§25's only clean answer).
    Refused,
    /// 200 OK whose result is a hex string that is not 32 bytes.
    NotHash,
    /// Accept the request, then close the stream without a byte of response.
    Dropped,
}

struct Stub {
    url: String,
    /// One entry per arrival, in order. Recorded *before* the response is written, so a
    /// caller that has already read an answer is guaranteed to see the arrival in the tally
    /// — a tally appended after the write would turn every "no second send happened"
    /// assertion into a race with this thread.
    methods: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn spawn(send: Send) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port is available");
        let url = format!(
            "http://127.0.0.1:{}/{TOKEN}",
            listener.local_addr().expect("addr").port()
        );
        let methods = Arc::new(Mutex::new(Vec::new()));
        let counted = methods.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let counted = counted.clone();
                let send = send.clone();
                std::thread::spawn(move || {
                    Self::serve(stream, send, counted);
                });
            }
        });
        Self { url, methods }
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().expect("the tally").clone()
    }

    fn sends(&self) -> usize {
        self.methods()
            .iter()
            .filter(|method| method.as_str() == "eth_sendRawTransaction")
            .count()
    }

    /// Read one JSON-RPC request, count it, and answer it however the shape says.
    fn serve(stream: TcpStream, send: Send, counted: Arc<Mutex<Vec<String>>>) {
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
        counted.lock().expect("the tally").push(method.clone());
        let Some((status, content_type, payload)) = reply(&method, &send) else {
            // `Dropped` sends nothing: dropping the stream is the whole of the behaviour,
            // and the client's own reading of a closed connection is what the lane has to
            // classify.
            return;
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\
             \r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

/// The stub's answer for one method, or `None` when it sends nothing back.
fn reply(method: &str, send: &Send) -> Option<(&'static str, &'static str, String)> {
    if method == "eth_chainId" {
        return ok(format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":"{CHAIN_HEX}"}}"#
        ));
    }
    if method != "eth_sendRawTransaction" {
        // A method this lane does not call is answered the way M6 measured the real
        // endpoint answering an unwhitelisted one, so a stray request cannot be mistaken
        // for a success.
        return json_error(-32601, "rpc method is not whitelisted");
    }
    match send {
        Send::Ack(hash) => ok(format!(r#"{{"jsonrpc":"2.0","id":1,"result":"{hash}"}}"#,)),
        // The refusal has to be one a payload can be proven wrong for. M12-E §4 retired
        // `nonce too low` from this seat — it is a state-dependent answer, so the classifier
        // now files it as unknown and `send_uncertainty.rs` grades that — and what is left is
        // a fact about these bytes alone: the transaction carries less gas than its own
        // payload costs before it runs.
        Send::Refused => json_error(-32003, "intrinsic gas too low"),
        Send::NotHash => ok(r#"{"jsonrpc":"2.0","id":1,"result":"0xdeadbeef"}"#.to_string()),
        Send::Dropped => None,
    }
}

fn ok(payload: String) -> Option<(&'static str, &'static str, String)> {
    Some(("200 OK", "application/json", payload))
}

fn json_error(code: i64, message: &str) -> Option<(&'static str, &'static str, String)> {
    ok(format!(
        r#"{{"jsonrpc":"2.0","id":1,"error":{{"code":{code},"message":"{message}"}}}}"#
    ))
}

fn unsigned() -> UnsignedTransaction {
    UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id: CHAIN,
        nonce: 0,
        to: Some(Address::from_slice(&[0x6bu8; 20])),
        value: U256::ZERO,
        gas_limit: 21_000,
        input: Bytes::from(vec![0x12u8, 0x34, 0x56, 0x78]),
        access_list: Vec::new(),
        max_priority_fee_per_gas: Some(U256::from(TIP)),
        max_fee_per_gas: Some(U256::from(BASE_FEE * 2 + TIP)),
    }
}

/// One worth-nothing transaction, built and signed by the crate's own code over the
/// synthetic key, so the bytes the stub receives are the shape a real send takes.
fn signed() -> SignedTransaction {
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("the synthetic key is in range");
    Signer::from_key(ExecutionMode::SignOnly, key)
        .sign(&unsigned())
        .expect("the synthetic key signs")
}

/// One lane, pointed at one stub, with the stub kept alive so its tally can be read after.
async fn lane(
    send: Send,
    endpoint: EndpointKind,
    mode: ExecutionMode,
) -> (Stub, GiwaSequencerDirect) {
    let stub = Stub::spawn(send);
    let direct = GiwaSequencerDirect::connect(&stub.url, CHAIN, mode, endpoint)
        .await
        .expect("the stub answers eth_chainId with the configured chain, so a connect finishes");
    (stub, direct)
}

/// What every shape owes §5, whichever answer the node gave.
fn assert_names_the_socket_and_nothing_more(line: &str, digest: &str) {
    assert!(
        line.contains(digest),
        "the line has to name the endpoint the send went through, or two runs over two \
         sockets are indistinguishable: {line}"
    );
    let lowered = line.to_ascii_lowercase();
    for class in CLASS_WORDS {
        assert!(
            !lowered.contains(class),
            "`{class}` is a claim about who runs the endpoint and what it is for, and a send \
             cannot establish it: {line}"
        );
    }
    for credential in [TOKEN, "127.0.0.1"] {
        assert!(
            !line.contains(credential),
            "`{credential}` is part of the endpoint URL, and §5 keeps URLs and the \
             credentials inside them out of evidence: {line}"
        );
    }
    // Which question was asked is stated by the code that asks it, so a submission line
    // can only carry the send's method — never the read's (`receipt_provenance` is the one
    // sentence in this crate that names `eth_getTransactionReceipt`, and it reaches only
    // receipt rows). That is §5's 「不得把读取端点标签直接复用为提交端点标签」 enforced on
    // the composed line rather than on a label passed down.
    assert!(
        line.contains("eth_sendRawTransaction"),
        "the line has to say which method the lane called, or a receipt sentence and a \
         submission sentence are indistinguishable in the evidence: {line}"
    );
    assert!(
        !line.contains("eth_getTransactionReceipt"),
        "the read's method is describing a read; it does not belong in an answer about a \
         send: {line}"
    );
}

#[tokio::test]
async fn an_accepted_send_names_the_socket_and_not_who_runs_it() {
    let transaction = signed();
    let hash = format!("{:#x}", transaction.hash());
    let (stub, direct) = lane(
        Send::Ack(hash.clone()),
        EndpointKind::Unknown,
        ExecutionMode::Submit,
    )
    .await;
    let digest = evm_chain::endpoint_id(&stub.url);

    // The class the lane reports about itself, before anything goes over the wire.
    assert_eq!(direct.endpoint(), EndpointKind::Unknown);
    assert_eq!(EndpointKind::Unknown.name(), "unknown");
    assert!(
        direct.may_submit(),
        "not knowing who runs the node is a limit on what the line claims, not a brake on \
         the lane — a lane that refused to send for that reason would be guessing the other \
         way"
    );

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
            "the stub acknowledged these exact bytes, so a `{}` answer is the defect",
            outcome.status_word()
        );
    };
    assert_eq!(*transaction_hash, Some(transaction.hash()));
    // §27's comparison happens before the variant is built, which is why there is no
    // "accepted, but not of ours" field left to assert: an answer that names a different
    // hash is an `Unknown` (`send_uncertainty.rs`'s `a_hash_that_is_not_ours…`), and this
    // shape is only reachable by a node that named these very bytes.
    assert!(
        detail.contains(&format!("{:#x}", transaction.hash())),
        "so the hash quoted in the line is the local one: {detail}"
    );
    // The node's own words survive, and the socket is named beside them.
    assert!(
        detail.contains("eth_sendRawTransaction"),
        "the line says which method was answered: {detail}"
    );
    assert!(
        detail.contains(&hash),
        "and carries the answer itself: {detail}"
    );
    assert_names_the_socket_and_nothing_more(detail, &digest);

    // One acknowledgement, one send: the answer is terminal, so the retry the dropped shape
    // measures below cannot happen here.
    assert_eq!(
        stub.methods(),
        vec![
            "eth_chainId".to_string(),
            "eth_sendRawTransaction".to_string()
        ],
        "the lane asked the endpoint exactly what this test asked it to"
    );
}

#[tokio::test]
async fn a_refusal_names_the_socket_and_is_the_only_answer_that_clears_the_lane() {
    let transaction = signed();
    let (stub, direct) = lane(Send::Refused, EndpointKind::Unknown, ExecutionMode::Submit).await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("a refusal is an answer");
    let SubmissionOutcome::Rejected { reason, .. } = &outcome else {
        panic!(
            "the node said no, so the answer has to be a refusal rather than `{}`",
            outcome.status_word()
        );
    };
    assert_eq!(outcome.status_word(), "rejected");
    assert!(
        outcome.proven_not_in_flight(),
        "§25: only a definite refusal proves the bytes are not in flight"
    );
    assert!(
        reason.contains("intrinsic gas too low"),
        "the node's reason is quoted: {reason}"
    );
    assert_names_the_socket_and_nothing_more(reason, &digest);
    assert_eq!(stub.sends(), 1, "a refusal is never sent again");
}

#[tokio::test]
async fn an_answer_that_is_not_an_acknowledgement_names_the_socket_and_keeps_the_lane_busy() {
    let transaction = signed();

    // A 200 OK whose result is not a hash: the lane cannot tell whether the bytes were
    // accepted, and §25 forbids reading that as a refusal.
    let (stub, direct) = lane(Send::NotHash, EndpointKind::Unknown, ExecutionMode::Submit).await;
    let digest = evm_chain::endpoint_id(&stub.url);
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("an unclassifiable answer is still an answer");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "`0xdeadbeef` is not a transaction hash, so the honest word is unknown, not `{}`",
            outcome.status_word()
        );
    };
    assert_eq!(outcome.status_word(), "unknown");
    assert!(
        !outcome.proven_not_in_flight(),
        "and the lane stays reserved until a read resolves it"
    );
    assert!(
        reason.contains("0xdeadbeef"),
        "the answer is quoted verbatim: {reason}"
    );
    assert_names_the_socket_and_nothing_more(reason, &digest);
    assert_eq!(stub.sends(), 1, "a parsed answer ends the call");

    // No answer at all. This is the shape that used to leak: reqwest quotes the URL it was
    // sending to inside its own error text, and that text goes into the line — so the
    // digest replaces the URL here rather than the error being rewritten, because §25 still
    // forbids paraphrasing what the transport said.
    let (stub, direct) = lane(Send::Dropped, EndpointKind::Unknown, ExecutionMode::Submit).await;
    let digest = evm_chain::endpoint_id(&stub.url);
    let outcome = direct
        .submit(&transaction)
        .await
        .expect("no answer is not an error");
    let SubmissionOutcome::Unknown { reason, .. } = &outcome else {
        panic!(
            "a closed connection proves nothing about the node's answer, so it cannot be `{}`",
            outcome.status_word()
        );
    };
    assert!(!outcome.proven_not_in_flight());
    assert_names_the_socket_and_nothing_more(reason, &digest);
    assert_eq!(
        stub.sends(),
        1,
        "M12-E §3: a send that never came back is not sent a second time. The count is the \
         one the stub itself tallied — the read path's single transport retry \
         (`crates/chain/src/rpc.rs`'s `WirePolicy::RetryOnce`) still applies to reads, and this \
         line is where the send side is shown to be off it"
    );
}

#[tokio::test]
async fn the_evidence_row_for_a_send_says_unknown_and_carries_no_url() {
    let transaction = signed();
    let hash = format!("{:#x}", transaction.hash());
    let (stub, direct) = lane(
        Send::Ack(hash),
        EndpointKind::Unknown,
        ExecutionMode::Submit,
    )
    .await;
    let digest = evm_chain::endpoint_id(&stub.url);

    let outcome = direct
        .submit(&transaction)
        .await
        .expect("the stub answered");
    let row = SubmissionEvidence::from_outcome(transaction.hash(), &outcome, 1_790_000_000_000);
    let json = row.to_json();

    assert_eq!(
        json["submission_endpoint_type"],
        Value::String("unknown".to_string()),
        "§53's endpoint field is the conservative word, not a class"
    );
    assert_eq!(json["outcome"], Value::String("submitted".to_string()));
    assert_eq!(
        json["transaction_hash"],
        Value::String(format!("{:#x}", transaction.hash())),
        "the tracked hash stays the one computed locally"
    );
    assert!(row.was_sent());
    // The record's shape is untouched by the label change: a gate that reads these rows
    // still finds every field §53 lists.
    for field in SubmissionEvidence::required_fields() {
        assert!(
            json.get(field).is_some(),
            "`{field}` is one of §53's required fields and its absence would move the schema \
             under the milestone's own diff: {json}"
        );
    }
    let text = json.to_string();
    assert!(text.contains(&digest), "the row names the socket: {text}");
    for class in CLASS_WORDS {
        assert!(
            !text.to_ascii_lowercase().contains(class),
            "`{class}` would be a class claim inside the row: {text}"
        );
    }
    for credential in [TOKEN, "127.0.0.1"] {
        assert!(
            !text.contains(credential),
            "the row is written to submissions.jsonl, so `{credential}` would be published \
             evidence: {text}"
        );
    }
}

#[test]
fn both_halves_of_a_run_name_the_one_socket_and_a_second_socket_changes_only_the_digest() {
    let here = "http://127.0.0.1:8545/token/abc123";
    let there = "http://127.0.0.1:8546/token/abc123";
    let (here_id, there_id) = (evm_chain::endpoint_id(here), evm_chain::endpoint_id(there));

    let send = submission_provenance(here);
    let read = receipt_provenance(here);
    assert!(
        send.contains(&here_id) && read.contains(&here_id),
        "one socket, one digest, in both directions: {send} / {read}"
    );
    // Same socket, different questions. The read half is a whole sentence that names the
    // method it describes; the send half is deliberately only a socket phrase, because the
    // submission line is composed by `submit()` around it and would otherwise say the
    // method twice. Either way the read's wording does not travel to the send side — which
    // is §5's 「不得把读取端点标签直接复用为提交端点标签」, and the composed lines carry that
    // check for real answers (see [`an_accepted_send_names_the_socket_and_not_who_runs_it`],
    // whose helper requires the send's method and forbids the read's).
    assert!(
        read.contains("eth_getTransactionReceipt") && !read.contains("eth_sendRawTransaction"),
        "the read line describes a read: {read}"
    );
    assert!(
        send.contains("endpoint") && !send.contains("eth_getTransactionReceipt"),
        "the send phrase names the socket and borrows nothing from the read sentence: {send}"
    );
    // Neither side reaches for a class word, on either socket.
    for line in [
        send.as_str(),
        read.as_str(),
        submission_provenance(there).as_str(),
    ] {
        for class in CLASS_WORDS {
            assert!(
                !line.to_ascii_lowercase().contains(class),
                "`{class}` is not a fact a read or a send can establish: {line}"
            );
        }
    }
    // And repointing the lane is visible in the line, which is the whole reason the digest
    // replaced the class: a class word could not tell these two endpoints apart.
    assert_ne!(here_id, there_id);
    assert!(submission_provenance(there).contains(&there_id));
    assert!(!submission_provenance(there).contains(&here_id));
    assert_ne!(read, receipt_provenance(there));
}

/// The audit's cross-check, run against the source rather than against a runtime value: the
/// read side's purpose type stays on the read side, and the class word the lane retired is
/// retired at the wiring rather than deleted from the vocabulary.
///
/// Each zero has a non-zero control in the same test, because a needle that matches nothing
/// anywhere is a needle that proves nothing (§8's planted defects are the same idea one level
/// up: this scan goes red if a production site re-derives a class from a URL, re-adopts
/// `public_http_rpc` for itself, or imports the read role's label).
#[test]
fn the_read_sides_label_never_crosses_into_the_submission_path() {
    let root = workspace_root();
    let read = |relative: &str| {
        std::fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("{}: {error}", relative))
    };
    // Doc prose is skipped on purpose: this scan is about code that runs, and this crate's
    // own documentation is where the retirement is *explained*, including the words it
    // explains.
    let mentions = |text: &str, needle: &str| {
        text.lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains(needle))
            .count()
    };

    let stage = read("crates/execution/src/stage.rs");
    let sequence = read("crates/execution/src/sequence.rs");
    let submitter = read("crates/execution/src/submitter.rs");
    let lane_files = format!("{stage}\n{sequence}");

    // Zero: a read role's label, anywhere in this crate's code.
    let mut purposes = 0;
    for entry in std::fs::read_dir(root.join("crates/execution/src")).expect("src exists") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            purposes += mentions(
                &std::fs::read_to_string(&path).expect("a readable source"),
                "EndpointPurpose::",
            );
        }
    }
    assert_eq!(
        purposes, 0,
        "`EndpointPurpose` is the operator-declared *read* purpose (§4 of M12-B). Carrying one \
         onto a submission line is the same guess in the other endpoint's clothes, which §5 \
         forbids outright"
    );
    // …and the positive control that the needle is a real one: the type exists and is used
    // where it belongs.
    assert!(
        mentions(&read("crates/chain/src/endpoint.rs"), "EndpointPurpose::") > 0,
        "the scan above would also pass on a crate that never heard of the type, and this \
         repository has heard of it"
    );

    // Zero: the class word picked by either wiring site.
    assert_eq!(
        mentions(&lane_files, "EndpointKind::PublicHttpRpc"),
        0,
        "the lane's two connect sites state no class about the endpoint they were given"
    );
    // …and the word is still in the vocabulary, so its absence above is the fix rather than
    // a rename that moved it out of reach of the scan.
    assert!(
        mentions(&submitter, "\"public_http_rpc\"") > 0,
        "`PublicHttpRpc` stays for a caller that was told who runs its endpoint"
    );
    assert!(
        mentions(&submitter, "EndpointKind::PublicHttpRpc") > 0,
        "and is still constructible — its own unit tests build outcomes carrying it"
    );

    // Exactly two: the one conservative label the lane can use, at the two wiring sites.
    assert_eq!(
        mentions(&lane_files, "EndpointKind::Unknown"),
        2,
        "`stage::ExecutionStage::connect` and `sequence::SequenceStage::connect_with_trace` \
         are the whole of the lane's construction; a third site would mean a new path that \
         chose its own endpoint class"
    );

    // The retired wording is still findable in committed evidence, which is the second
    // positive control: the class-word loops above would also pass if the word had gone
    // missing everywhere. This file reads nothing of M6/M7 and rewrites less.
    let route =
        read("data/evidence/m7/route-submit/route-91342-37563264-1790908382146/submissions.jsonl");
    assert!(
        route.contains("public_http_rpc"),
        "the M7 submit run wrote that class about itself, and §9 keeps its rows as they are"
    );
}

#[test]
fn a_conservative_label_still_describes_a_socket_and_a_read_only_one_still_refuses_to_send() {
    // The two questions §5 keeps apart: can this endpoint carry a broadcast, and do we know
    // who runs it. `Unknown` answers the second honestly and the first yes.
    assert!(EndpointKind::Unknown.may_broadcast());
    assert_eq!(EndpointKind::Unknown.name(), "unknown");
    assert!(!EndpointKind::Recorded.may_broadcast());
    assert_eq!(EndpointKind::Recorded.name(), "recorded_no_submission");
}

#[tokio::test]
async fn execution_that_does_not_fit_the_configuration_is_refused_before_the_socket() {
    let transaction = signed();

    // A recorded endpoint asked to submit is refused by the connect-time gate, and the
    // refusal names the kind it was given rather than inventing a destination.
    let (stub, error) = {
        let stub = Stub::spawn(Send::Ack(format!("{:#x}", transaction.hash())));
        let result = GiwaSequencerDirect::connect(
            &stub.url,
            CHAIN,
            ExecutionMode::Submit,
            EndpointKind::Recorded,
        )
        .await;
        (
            stub,
            result
                .err()
                .expect("a recorded endpoint cannot be asked to submit"),
        )
    };
    let text = error.to_string();
    assert!(
        text.contains("recorded_no_submission"),
        "the refusal quotes the endpoint class it was given: {text}"
    );
    assert!(
        text.contains("Submit"),
        "and the mode it was asked to serve: {text}"
    );
    assert_eq!(stub.sends(), 0, "the refusal happened before any byte left");

    // The mirror case: an endpoint whose class nobody stated is *not* a reason to refuse.
    // The gate that stops this run is its mode, and the mode's own words say so.
    let (stub, direct) = lane(
        Send::Ack(format!("{:#x}", transaction.hash())),
        EndpointKind::Unknown,
        ExecutionMode::BuildOnly,
    )
    .await;
    assert!(!direct.may_submit());
    let error = direct
        .submit(&transaction)
        .await
        .expect_err("BuildOnly never reaches the network");
    let text = error.to_string();
    assert!(
        text.contains("build-only"),
        "the refusal names the mode that refused: {text}"
    );
    assert_eq!(
        stub.sends(),
        0,
        "and the send that §5 says must be blocked for the configuration, not for the label, \
         is blocked at the mode gate"
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/execution sits two levels below the workspace root")
        .to_path_buf()
}
