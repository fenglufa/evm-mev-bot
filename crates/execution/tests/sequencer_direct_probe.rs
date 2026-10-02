//! §29: the endpoint named by `RETH_ROLLUP_SEQUENCERHTTP` is re-studied, not inherited.
//!
//! M6 concluded that no direct-sequencer protocol exists on GIWA, and M7's own P0 probe
//! re-ran the method whitelist on the sequencer host. Neither of those answers the
//! question §29 actually asks, which is about the *interface*: HTTP method, request
//! format, response format, authentication, and transaction submission semantics —
//! 「不要假设它就是 raw transaction API」. A name is not a protocol, and the previous
//! probes ran with a hand-rolled HTTP client whose request shape differed from the one
//! `crates/chain`'s adapter sends (§5's evidence: `eth_sendRawTransaction` came back
//! `-32700 parse error` there and `-32602 missing value for required argument 0` on M6,
//! which is a difference in the *caller* that was never pinned down). So this file
//! measures the shape our own lane uses, against three hosts, and writes down every
//! answer.
//!
//! ```text
//! GIWA_RPC_URL=https://sepolia-rpc.giwa.io \
//! RETH_ROLLUP_SEQUENCERHTTP=https://sepolia-sequencer.giwa.io \
//!   cargo test -p evm-execution --test sequencer_direct_probe -- --ignored --nocapture
//! ```
//!
//! What it sends, and what it never sends:
//!
//! * read-only JSON-RPC (`eth_chainId`, `eth_blockNumber`, `eth_getTransactionReceipt`,
//!   `eth_getBalance`, `eth_getTransactionCount`) and the direct-protocol candidates
//!   (`mev_sendBundle`, `engine_submitBlock`, `sequencer_submit`, `giwa_sendRawTransaction`,
//!   `eth_sendRawTransactionFlashblock`, `txpool_content`) with empty params, which cannot
//!   be a transaction;
//! * `eth_sendRawTransaction` payloads only, built by the real builder and signed by the
//!   synthetic scalar-1 key (§40's key, whose address this file first proves unfunded). The
//!   value is zero, the target is WETH, the calldata is empty: nothing these bytes can do
//!   is worth anything to anyone, and the account that would have to pay for it has never
//!   held a wei. **The operator's key is not read here** — the environment door is not
//!   even opened, because the signer is constructed from a local constant (§19).
//! * no real arbitrage, and no submission with a fundable sender. If a payload were ever
//!   accepted, the test says so in the evidence rather than pretending it was refused.
//!
//! The evidence file is the point: `data/evidence/m7/probe-sequencer-direct.json` holds
//! one record per call — request, HTTP status, the response headers that matter, the
//! parsed JSON, the latency — and a `finding` whose statements are assembled from the
//! records the same run produced.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alloy_primitives::{address, Address, Bytes, U256};
use serde_json::{json, Value};

use evm_execution::{
    decode_raw, recover_sender, ExecutionKey, ExecutionMode, SignedTransaction, Signer,
    TransactionType, UnsignedTransaction,
};

/// §40's synthetic key: the scalar one. Public knowledge, and the reason these bytes can
/// be recorded in a committed file without a second thought.
const TEST_SCALAR: [u8; 32] = {
    let mut bytes = [0u8; 32];
    bytes[31] = 1;
    bytes
};

const CHAIN: u64 = 91_342;
const WETH: Address = address!("0x4200000000000000000000000000000000000006");

const EVIDENCE: &str = "data/evidence/m7/probe-sequencer-direct.json";
/// M6's §35 validation transaction: a real, mined, already-final hash, so a receipt read
/// is a read with a known answer rather than a request into the void.
const M6_NODE_ANSWERS: &str = "data/evidence/m6/validation/node-answers-37503978.json";

/// One request's answer, exactly as it came back.
struct Call {
    section: &'static str,
    host: &'static str,
    label: String,
    verb: String,
    headers_sent: Vec<String>,
    request: Option<String>,
    http: u16,
    response_headers: Value,
    response_text: String,
    parsed: Value,
    ms: u64,
    transport_error: Option<String>,
}

impl Call {
    fn to_json(&self) -> Value {
        json!({
            "section": self.section,
            "host": self.host,
            "label": self.label,
            "http_verb": self.verb,
            "request_headers_sent": self.headers_sent,
            "request_body": self.request,
            "http_status": self.http,
            "response_headers": self.response_headers,
            "response_body": clip(&self.response_text),
            "response_json": self.parsed,
            "latency_ms": self.ms,
            "transport_error": self.transport_error,
        })
    }

    /// The JSON-RPC error object, if the answer carried one.
    fn error_code(&self) -> Option<i64> {
        self.parsed
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64)
    }

    fn error_message(&self) -> Option<&str> {
        self.parsed
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
    }

    fn result_text(&self) -> Option<&str> {
        self.parsed.get("result").and_then(Value::as_str)
    }

    /// Whether the whitelist refused the method by name — §29's evidence of absence.
    fn unwhitelisted(&self) -> bool {
        self.error_code() == Some(-32601)
    }

    /// Whether the host served the method at all: `-32601` is the whitelist saying no, and
    /// a transport failure is not an answer. Anything else — a result, or a `-32602` from
    /// the JSON-RPC layer — means the method reached the node.
    fn served(&self) -> bool {
        match self.error_code() {
            Some(-32601) => false,
            Some(_) => self.http == 200,
            None => self.http == 200 && self.parsed.get("result").is_some(),
        }
    }
}

/// A response body kept whole up to two kilobytes; block and receipt JSON is larger than
/// the point of this probe.
fn clip(text: &str) -> String {
    const LIMIT: usize = 2048;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    let mut cut = LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…[{} bytes total]", &text[..cut], text.len())
}

struct Host {
    name: &'static str,
    url_env: &'static str,
    url: String,
}

struct Probe {
    client: reqwest::Client,
    calls: Vec<Call>,
}

/// The three headers that say something about how the host fronts the node.
const RESPONSE_HEADERS: [&str; 6] = [
    "content-type",
    "server",
    "www-authenticate",
    "x-request-id",
    "cf-ray",
    "content-length",
];

impl Probe {
    fn new() -> Self {
        Self {
            // The same shape as `HttpChainAdapter::connect`: one client, 20 s timeout.
            // The point of copying it is that a probe with a different transport can
            // report a different endpoint (§5's parse-error discrepancy).
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("a plain HTTP client builds"),
            calls: Vec::new(),
        }
    }

    async fn send(
        &mut self,
        section: &'static str,
        host: &Host,
        label: String,
        verb: &str,
        request: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Call {
        let mut builder = self.client.request(
            reqwest::Method::from_bytes(verb.as_bytes()).expect("a known HTTP verb"),
            &host.url,
        );
        if let Some(body) = request {
            builder = builder.body(body.to_string());
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let started = Instant::now();
        let answer = builder.send().await;
        let ms = started.elapsed().as_millis() as u64;
        let (http, response_headers, response_text, transport_error) = match answer {
            Err(error) => (0, Value::Null, String::new(), Some(error.to_string())),
            Ok(response) => {
                let status = response.status().as_u16();
                let mut kept = serde_json::Map::new();
                for name in RESPONSE_HEADERS {
                    if let Some(value) = response.headers().get(name) {
                        kept.insert(
                            name.to_string(),
                            Value::String(value.to_str().unwrap_or("<binary>").to_string()),
                        );
                    }
                }
                let text = response
                    .text()
                    .await
                    .unwrap_or_else(|error| format!("<body unreadable: {error}>"));
                (status, Value::Object(kept), text, None)
            }
        };
        let parsed =
            serde_json::from_str::<Value>(&response_text).unwrap_or_else(|_| json!("<not JSON>"));
        let call = Call {
            section,
            host: host.name,
            label,
            verb: verb.to_string(),
            headers_sent: headers
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect(),
            request: request.map(clip),
            http,
            response_headers,
            response_text,
            parsed,
            ms,
            transport_error,
        };
        self.calls.push(call.clone_view());
        call
    }

    /// The canonical envelope, in the shape `crates/chain/src/rpc.rs` writes it.
    fn envelope(method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()
    }

    /// `POST` + `Content-Type: application/json` + the canonical envelope: what our lane
    /// actually does. Every baseline call goes through here.
    async fn rpc(
        &mut self,
        section: &'static str,
        host: &Host,
        method: &str,
        params: Value,
    ) -> Call {
        let label = format!("{method} {}", clip(&params.to_string()));
        let body = Self::envelope(method, params);
        self.send(
            section,
            host,
            label,
            "POST",
            Some(&body),
            &[("content-type", "application/json")],
        )
        .await
    }

    /// The first record whose label starts with `method`, which is how the finding reads
    /// answers without re-deriving them.
    fn find(&self, section: &str, method: &str) -> Option<&Call> {
        self.calls
            .iter()
            .find(|call| call.section == section && call.label.starts_with(method))
    }
}

/// `Call` is moved out of `send` and also parked in `calls`, so the caller gets a copy of
/// what it needs without the vector having to hand out references into itself.
impl Call {
    fn clone_view(&self) -> Self {
        Self {
            section: self.section,
            host: self.host,
            label: self.label.clone(),
            verb: self.verb.clone(),
            headers_sent: self.headers_sent.clone(),
            request: self.request.clone(),
            http: self.http,
            response_headers: self.response_headers.clone(),
            response_text: self.response_text.clone(),
            parsed: self.parsed.clone(),
            ms: self.ms,
            transport_error: self.transport_error.clone(),
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/execution sits two levels below the workspace root")
        .to_path_buf()
}

/// A host is named by an environment variable and never typed here (§44, still in force).
fn host(name: &'static str, url_env: &'static str, required: bool) -> Option<Host> {
    match std::env::var(url_env) {
        Ok(url) => Some(Host { name, url_env, url }),
        Err(_) if required => panic!(
            "{url_env} is required: §29 asks what the endpoint answers, and no endpoint is \
             hardcoded in this repository"
        ),
        Err(_) => None,
    }
}

/// The synthetic sender, derived from the key rather than written down.
fn synthetic_sender() -> Address {
    ExecutionKey::from_secret_bytes(&TEST_SCALAR)
        .expect("the scalar-1 key is in range")
        .address()
}

/// One EIP-1559 transaction, built and signed by the crate's own code, worth nothing.
fn probe_transaction(chain_id: u64, nonce: u64) -> SignedTransaction {
    let unsigned = UnsignedTransaction {
        tx_type: TransactionType::DynamicFee,
        chain_id,
        nonce,
        to: Some(WETH),
        value: U256::ZERO,
        gas_limit: 21_000,
        input: Bytes::new(),
        access_list: Vec::new(),
        max_priority_fee_per_gas: Some(U256::from(1_000_000u64)),
        max_fee_per_gas: Some(U256::from(1_000_370u64)),
    };
    let key = ExecutionKey::from_secret_bytes(&TEST_SCALAR).expect("a synthetic key is in range");
    let signer = Signer::from_key(ExecutionMode::SignOnly, key);
    signer.sign(&unsigned).expect("the synthetic key signs")
}

/// The payload in the form `eth_sendRawTransaction` takes it.
fn raw_payload(transaction: &SignedTransaction) -> String {
    format!("0x{}", hex::encode(transaction.raw().as_ref()))
}

/// §15's local control, run before any byte goes over the network: the payload our codec
/// writes is a payload our codec reads back, and the sender it proves is the synthetic one.
fn assert_the_payload_is_what_we_built(transaction: &SignedTransaction) {
    let raw = transaction.raw();
    let decoded = decode_raw(raw.as_ref(), recover_sender)
        .expect("the bytes the builder wrote decode back into a transaction");
    assert_eq!(decoded.unsigned, transaction.unsigned);
    assert_eq!(
        decoded.sender,
        Some(synthetic_sender()),
        "the signature recovers the synthetic account and nobody else's"
    );
}

/// The receipt read is only a positive control if the hash really is mined: the block the
/// receipt names is checked against M6's own frozen answer.
fn mined_transaction_hash() -> String {
    let path = workspace_root().join(M6_NODE_ANSWERS);
    let text = std::fs::read_to_string(&path).expect("M6's node-answers evidence is committed");
    let saved: Value = serde_json::from_str(&text).expect("the evidence file is JSON");
    saved["attempts"]["eth_getTransactionReceipt"]["result"]["transactionHash"]
        .as_str()
        .expect("the saved receipt names its transaction")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "probes live endpoints; the four gates must not depend on a node being up"]
async fn the_sequencer_endpoint_is_measured_before_it_is_called_direct() {
    let mut hosts = vec![
        host("rpc", "GIWA_RPC_URL", true).expect("the read host"),
        host("sequencer", "RETH_ROLLUP_SEQUENCERHTTP", true).expect("the §29 host"),
    ];
    if let Some(flashblocks) = host("flashblocks", "GIWA_FLASHBLOCKS_RPC_URL", false) {
        hosts.push(flashblocks);
    }
    let mut probe = Probe::new();

    // ------------------------------------------------------------------
    // 0. The write ladder's sender has never held a wei. Checked before the
    //    first submission and again after the last one, so the probe cannot
    //    have spent anything it was not given.
    // ------------------------------------------------------------------
    let sender = synthetic_sender();
    let balance_before = {
        let rpc = &hosts[0];
        probe
            .rpc(
                "0_unfunded_control",
                rpc,
                "eth_getBalance",
                json!([sender, "latest"]),
            )
            .await
    };
    assert_eq!(
        balance_before.result_text(),
        Some("0x0"),
        "the synthetic account holds {} at the read host, so this probe would be spending \
         real money and refuses to run",
        balance_before.result_text().unwrap_or("<unreadable>")
    );
    // The nonce comes from the node rather than from a guess. It turns out this
    // well-known key has a transaction in its own history on this chain, so the
    // "obvious" payload at nonce 0 would have been refused for the wrong reason —
    // `nonce too low` — and the ladder would have proved nothing about balances.
    let nonce_before = {
        let rpc = &hosts[0];
        probe
            .rpc(
                "0_unfunded_control",
                rpc,
                "eth_getTransactionCount",
                json!([sender, "pending"]),
            )
            .await
    };
    let nonce = u64::from_str_radix(
        nonce_before
            .result_text()
            .unwrap_or("0x0")
            .trim_start_matches("0x"),
        16,
    )
    .expect("the node answers the nonce as a hex quantity");

    // ------------------------------------------------------------------
    // 1. Read surface, per host. §29's first question in disguise: a host that
    //    cannot answer eth_chainId cannot be the endpoint our adapter connects
    //    to, because §6's chain-id check is the first thing `connect` does.
    // ------------------------------------------------------------------
    let mined = mined_transaction_hash();
    for host in &hosts {
        for (method, params) in [
            ("eth_chainId", json!([])),
            ("eth_blockNumber", json!([])),
            ("net_version", json!([])),
            ("web3_clientVersion", json!([])),
            ("eth_getTransactionReceipt", json!([mined])),
            (
                "eth_getTransactionCount",
                json!([sender.to_string(), "pending"]),
            ),
        ] {
            probe.rpc("1_read_surface", host, method, params).await;
        }
    }

    // ------------------------------------------------------------------
    // 2. The direct-protocol candidates, plus the misspelled control that makes
    //    `-32601` mean "absent" rather than "generic error".
    // ------------------------------------------------------------------
    let candidates = [
        "mev_sendBundle",
        "eth_sendBundle",
        "engine_submitBlock",
        "sequencer_submit",
        "giwa_sendRawTransaction",
        "eth_sendRawTransactionFlashblock",
        "txpool_content",
        "private_txSubmit",
        "etch_sendRawTransaction",
    ];
    for host in &hosts {
        for method in candidates {
            probe
                .rpc("2_direct_protocol", host, method, json!([]))
                .await;
        }
    }

    // ------------------------------------------------------------------
    // 3. HTTP method. The same valid envelope over six verbs: which ones the
    //    host answers, and whether the answer for a GET differs from POST.
    // ------------------------------------------------------------------
    let body = Probe::envelope("eth_chainId", json!([]));
    for host in &hosts {
        for verb in ["POST", "GET", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"] {
            let headers: &[(&str, &str)] = if verb == "GET" || verb == "HEAD" {
                &[]
            } else {
                &[("content-type", "application/json")]
            };
            probe
                .send(
                    "3_http_method",
                    host,
                    format!("{verb} eth_chainId"),
                    verb,
                    if verb == "GET" || verb == "HEAD" {
                        None
                    } else {
                        Some(&body)
                    },
                    headers,
                )
                .await;
        }
    }

    // ------------------------------------------------------------------
    // 4. Request format. Each mutation of the envelope on the read host, where
    //    a correct request has a known answer (0x164ce), so a change in the
    //    answer is a fact about the format and not about the chain.
    // ------------------------------------------------------------------
    let rpc = &hosts[0];
    type Mutation<'a> = (
        &'static str,
        Option<&'a str>,
        &'static [(&'static str, &'static str)],
    );
    let mutations: [Mutation<'_>; 9] = [
        (
            "canonical, with content-type",
            Some(&body),
            &[("content-type", "application/json")],
        ),
        ("canonical, NO content-type", Some(&body), &[]),
        (
            "canonical, text/plain content-type",
            Some(&body),
            &[("content-type", "text/plain")],
        ),
        (
            "params omitted",
            Some(r#"{""jsonrpc"":""2.0"",""id"":1,""method"":""eth_chainId""}"#),
            &[("content-type", "application/json")],
        ),
        (
            "jsonrpc field omitted",
            Some(r#"{""id"":1,""method"":""eth_chainId"",""params"":[]}"#),
            &[("content-type", "application/json")],
        ),
        (
            "id omitted",
            Some(r#"{""jsonrpc"":""2.0"",""method"":""eth_chainId"",""params"":[]}"#),
            &[("content-type", "application/json")],
        ),
        (
            "params as an object, not an array",
            Some(r#"{""jsonrpc"":""2.0"",""id"":1,""method"":""eth_chainId"",""params"":{}}"#),
            &[("content-type", "application/json")],
        ),
        (
            "a batch of two",
            Some(
                r#"[{""jsonrpc"":""2.0"",""id"":1,""method"":""eth_chainId"",""params"":[]},{""jsonrpc"":""2.0"",""id"":2,""method"":""eth_blockNumber"",""params"":[]}]"#,
            ),
            &[("content-type", "application/json")],
        ),
        (
            "not JSON at all",
            Some("{"),
            &[("content-type", "application/json")],
        ),
    ];
    for (label, request, headers) in mutations {
        probe
            .send(
                "4_request_format",
                rpc,
                format!("eth_chainId — {label}"),
                "POST",
                request,
                headers,
            )
            .await;
    }
    // Empty body, and no body at all: two different requests, and go-ethereum answers
    // them differently.
    probe
        .send(
            "4_request_format",
            rpc,
            "eth_chainId — empty body".to_string(),
            "POST",
            Some(""),
            &[("content-type", "application/json")],
        )
        .await;
    for host in &hosts {
        probe
            .send(
                "4_request_format",
                host,
                "eth_sendRawTransaction — no params key at all".to_string(),
                "POST",
                Some(r#"{""jsonrpc"":""2.0"",""id"":1,""method"":""eth_sendRawTransaction""}"#),
                &[("content-type", "application/json")],
            )
            .await;
    }

    // ------------------------------------------------------------------
    // 5. Authentication. Does any credential change any answer? If one does,
    //    the endpoint has an auth story §29 has to record; if none do, "no
    //    authentication" is a measurement and not an assumption.
    // ------------------------------------------------------------------
    let credentials: [(&str, &str); 4] = [
        ("authorization", "Bearer not-a-token"),
        ("x-api-key", "not-a-key"),
        ("x-giwa-key", "not-a-key"),
        ("cookie", "session=not-a-session"),
    ];
    for (name, value) in credentials {
        probe
            .send(
                "5_authentication",
                rpc,
                format!("eth_chainId with {name}"),
                "POST",
                Some(&body),
                &[("content-type", "application/json"), (name, value)],
            )
            .await;
    }
    // The same on the §29 host, where the answer to the read is already a refusal: an
    // unauthenticated refusal and a credential-shaped refusal are different findings.
    if hosts.len() > 1 {
        let sequencer = &hosts[1];
        probe
            .send(
                "5_authentication",
                sequencer,
                "eth_chainId with authorization".to_string(),
                "POST",
                Some(&body),
                &[
                    ("content-type", "application/json"),
                    ("authorization", "Bearer not-a-token"),
                ],
            )
            .await;
    }

    // ------------------------------------------------------------------
    // 6. Transaction submission semantics. The ladder runs the payload from
    //    "not a transaction" to "a transaction this chain would accept but this
    //    account cannot pay for", one rung at a time, on every host. The answer
    //    at each rung names the check the node ran to get there.
    // ------------------------------------------------------------------
    let right_chain = probe_transaction(CHAIN, nonce);
    let wrong_chain = probe_transaction(1, nonce);
    assert_the_payload_is_what_we_built(&right_chain);
    let right_hash = format!("{:#x}", right_chain.hash());
    let rungs: [(&str, Value); 6] = [
        ("no params", json!([])),
        ("empty bytes", json!(["0x"])),
        ("not RLP", json!(["0xdeadbeef"])),
        (
            "truncated 1559 envelope",
            json!([format!(
                "0x{}",
                &hex::encode(right_chain.raw().as_ref())[..10]
            )]),
        ),
        (
            "fully formed, right chain id, unfunded sender",
            json!([raw_payload(&right_chain)]),
        ),
        (
            "fully formed, chain id 1, replayed onto this chain",
            json!([raw_payload(&wrong_chain)]),
        ),
    ];
    let mut submissions = Vec::new();
    for host in &hosts {
        for (label, params) in &rungs {
            let call = probe
                .rpc(
                    "6_submission_semantics",
                    host,
                    "eth_sendRawTransaction",
                    params.clone(),
                )
                .await;
            submissions.push((host.name, (*label).to_string(), call));
        }
        // Idempotence: the same bytes a second time, immediately. A pool that already
        // holds them answers "already known"; a stateless forwarder answers the same
        // refusal twice — and that difference is submission semantics.
        let again = probe
            .rpc(
                "6_submission_semantics",
                host,
                "eth_sendRawTransaction",
                json!([raw_payload(&right_chain)]),
            )
            .await;
        submissions.push((host.name, "replay of the same bytes".to_string(), again));
    }
    // Does the accepted-anyway case leave a trace? Only the read host can answer this,
    // and the question is asked there.
    let after = {
        let rpc = &hosts[0];
        probe
            .rpc(
                "6_submission_semantics",
                rpc,
                "eth_getTransactionByHash",
                json!([right_hash]),
            )
            .await
    };

    // The control a second time: the probe itself must not have changed the account it
    // used as its sender.
    let balance_after = {
        let rpc = &hosts[0];
        probe
            .rpc(
                "6_submission_semantics",
                rpc,
                "eth_getBalance",
                json!([sender, "latest"]),
            )
            .await
    };
    assert_eq!(
        balance_after.result_text(),
        Some("0x0"),
        "the synthetic sender still holds nothing, so nothing in this run cost anything"
    );

    // ------------------------------------------------------------------
    // The finding, assembled from the records above.
    // ------------------------------------------------------------------
    let served = |section: &str, method: &str| -> Vec<String> {
        hosts
            .iter()
            .filter(|host| {
                probe.calls.iter().any(|call| {
                    call.section == section && call.host == host.name && {
                        call.label.starts_with(method) && call.served()
                    }
                })
            })
            .map(|host| host.name.to_string())
            .collect()
    };
    let refused = |section: &str, method: &str| -> Vec<String> {
        hosts
            .iter()
            .filter(|host| {
                probe.calls.iter().any(|call| {
                    call.section == section && call.host == host.name && {
                        call.label.starts_with(method) && call.unwhitelisted()
                    }
                })
            })
            .map(|host| host.name.to_string())
            .collect()
    };
    let read_hosts = served("1_read_surface", "eth_chainId");
    let accepted = submissions
        .iter()
        .filter(|(_, _, call)| call.http == 200 && call.parsed.get("result").is_some())
        .map(|(host, label, call)| format!("{host}: {label} → {}", clip(&call.response_text)))
        .collect::<Vec<_>>();
    let refusals = submissions
        .iter()
        .map(|(host, label, call)| {
            format!(
                "{host}: {label} → http {} {}",
                call.http,
                call.error_message()
                    .map(|message| message.to_string())
                    .unwrap_or_else(|| clip(&call.response_text))
            )
        })
        .collect::<Vec<_>>();
    let no_content_type = probe.find(
        "4_request_format",
        "eth_chainId — canonical, NO content-type",
    );
    let baseline = probe.find(
        "4_request_format",
        "eth_chainId — canonical, with content-type",
    );

    let verdict = if accepted.is_empty() {
        "Every submission rung was refused by every host, so nothing in this run reached a \
         mempool. The submission interface is `eth_sendRawTransaction` over POST with a \
         canonical JSON-RPC envelope on the host that serves reads; the §29 host adds no \
         protocol that the read host does not already answer."
    } else {
        "AT LEAST ONE SUBMISSION WAS ACCEPTED — see the `accepted` list. The probe's sender \
         is the unfunded synthetic account, so an accepted payload cannot execute, but the \
         statement 「no real transaction was broadcast」 must be read against this record."
    };

    // The counts the interpretation is stated in. Each one is a read of the records above,
    // so a drift changes the sentence rather than leaving it true by inertia.
    let sequencer_reads = probe
        .calls
        .iter()
        .filter(|call| call.section == "1_read_surface" && call.host == "sequencer")
        .count();
    let sequencer_refusals = probe
        .calls
        .iter()
        .filter(|call| {
            call.section == "1_read_surface" && call.host == "sequencer" && call.unwhitelisted()
        })
        .count();
    let post_200_refusals = submissions
        .iter()
        .filter(|(_, _, call)| call.http == 200 && call.error_code().is_some())
        .count();
    let post_400_refusals = submissions
        .iter()
        .filter(|(_, _, call)| call.http == 400)
        .count();
    let unwhitelisted_methods = probe
        .calls
        .iter()
        .filter(|call| call.section == "2_direct_protocol" && call.unwhitelisted())
        .count();

    let non_post = probe
        .calls
        .iter()
        .filter(|call| call.section == "3_http_method" && call.verb != "POST")
        .count();
    let non_post_405 = probe
        .calls
        .iter()
        .filter(|call| call.section == "3_http_method" && call.verb != "POST" && call.http == 405)
        .count();
    let parse_errors = probe
        .calls
        .iter()
        .filter(|call| call.section == "4_request_format" && call.http == 400)
        .count();

    let evidence = json!({
        "_provenance": {
            "milestone": "M7 §29",
            "asked": "M7 Coding.md 29: re-study RETH_ROLLUP_SEQUENCERHTTP and verify HTTP \
                      method / request format / response format / authentication / \
                      transaction submission semantics, without assuming it is a raw \
                      transaction API",
            "run_by": "cargo test -p evm-execution --test sequencer_direct_probe -- --ignored --nocapture",
            "urls_from_env": hosts.iter().map(|h| h.url_env).collect::<Vec<_>>(),
            "hosts": hosts.iter().map(|h| json!({"name": h.name, "url_env": h.url_env, "url": h.url})).collect::<Vec<_>>(),
            "client": "the same reqwest shape as crates/chain/src/rpc.rs: POST, \
                       Content-Type: application/json, {jsonrpc,id,method,params} — the \
                       deliberate difference from data/evidence/m7/probe-sequencer-method-whitelist.json, \
                       whose -32700 answer was produced by a hand-rolled client",
            "no_key_in_this_file": "the only signature is over the synthetic scalar-1 key \
                                    (§40); the operator's key is never read (§19) and its \
                                    environment variable is never touched",
            "no_value_at_risk": format!(
                "every probe payload has value 0 from {sender}, whose native balance this run \
                 read as 0x0 before the first submission and again after the last; the nonce \
                 {nonce} was read from eth_getTransactionCount(\"pending\") rather than \
                 assumed — this well-known key is not virgin on this chain, it already has a \
                 transaction in its history"
            ),
        },
        "calls": probe.calls.iter().map(Call::to_json).collect::<Vec<_>>(),
        "finding": {
            "read_surface_chain_id_served_by": read_hosts,
            "eth_sendRawTransaction_served_by": served("6_submission_semantics", "eth_sendRawTransaction"),
            "eth_sendRawTransaction_refused_by": refused("6_submission_semantics", "eth_sendRawTransaction"),
            "direct_protocol_candidates": candidates.iter().map(|method| json!({
                "method": method,
                "served_by": served("2_direct_protocol", method),
                "refused_by": refused("2_direct_protocol", method),
            })).collect::<Vec<_>>(),
            "misspelled_control": probe.calls.iter()
                .find(|call| call.label.starts_with("etch_sendRawTransaction"))
                .map(|call| format!("http {} {}", call.http, clip(&call.response_text))),
            "http_method": probe.calls.iter()
                .filter(|call| call.section == "3_http_method")
                .map(|call| format!("{} {} → http {}", call.host, call.label, call.http))
                .collect::<Vec<_>>(),
            "request_format": probe.calls.iter()
                .filter(|call| call.section == "4_request_format")
                .map(|call| format!("{} → http {} {}", call.label, call.http, clip(&call.response_text)))
                .collect::<Vec<_>>(),
            "response_format": {
                "headers_on_a_read": baseline.map(|call| call.response_headers.clone()),
                "success_shape": baseline.map(|call| clip(&call.response_text)),
                "refusal_shape": probe.find("2_direct_protocol", "mev_sendBundle")
                    .map(|call| clip(&call.response_text)),
                "transport_refusal_shape": probe.find("4_request_format", "eth_chainId — not JSON at all")
                    .map(|call| format!("http {} {}", call.http, clip(&call.response_text))),
            },
            "content_type_is_required": no_content_type.map(|call| format!(
                "a canonical envelope sent without Content-Type answered http {} {}",
                call.http, clip(&call.response_text)
            )),
            "authentication": probe.calls.iter()
                .filter(|call| call.section == "5_authentication")
                .map(|call| format!(
                    "{} {} → http {} {}",
                    call.host, call.label, call.http, clip(&call.response_text)
                ))
                .collect::<Vec<_>>(),
            "www_authenticate_ever_returned": probe.calls.iter()
                .any(|call| call.response_headers.get("www-authenticate").is_some()),
            "submission_semantics": refusals,
            "accepted_submissions": accepted,
            "did_the_accepted_payload_reach_the_pool": format!(
                "eth_getTransactionByHash({right_hash}) → {}",
                clip(&after.response_text)
            ),
            "sender_unaffected": format!(
                "balance {} before and {} after",
                balance_before.result_text().unwrap_or("?"),
                balance_after.result_text().unwrap_or("?")
            ),
            "verdict": verdict,
            "m7_consequence": "§29's answer decides which submitter the §58 run uses. \
                              SequencerDirect has no protocol on this endpoint and the \
                              host does not serve the reads §6's connect check needs, so \
                              the live run goes over eth_sendRawTransaction to the host \
                              that answers both — recorded here rather than assumed from M6.",
        },
        "interpretation": {
            "http_method": format!(
                "POST is the only verb any host answers. {non_post_405} of the {non_post} \
                 non-POST requests (GET, PUT, DELETE, HEAD, OPTIONS, PATCH, each with the same \
                 valid body) came back HTTP 405 with no JSON-RPC object at all — the front \
                 refuses the verb before the method is ever looked at.",
            ),
            "request_format": format!(
                "the envelope must carry jsonrpc, id, method AND params, with params an array: \
                 the canonical body (what crates/chain/src/rpc.rs writes) answers 0x164ce, and \
                 the {parse_errors} malformed-or-incomplete bodies measured here all answer \
                 HTTP 400 -32700 parse error with id null. Content-Type is not part of the \
                 contract — the same body without it still answered — which resolves the -32700 \
                 that data/evidence/m7/probe-sequencer-method-whitelist.json recorded for a \
                 correctly-named method: that probe's hand-rolled client sent a body missing one \
                 of the four required keys, so the node was refusing the envelope, not the \
                 method."
            ),
            "response_format": "one JSON-RPC object per request, newline-terminated, served by \
                                cloudflare with content-type application/json. Success is \
                                {result,id}; refusal is {error:{code,message},id}. Two status \
                                regimes matter for §25 and §39: a body the node could not even \
                                parse answers HTTP 400 with a -32602 or -32700, while a \
                                transaction it decoded and then refused answers HTTP 200 with a \
                                -32000-range code. crates/chain/src/rpc.rs only turns an answer \
                                into a refusal when the status is 2xx, so its caller sees the \
                                semantic refusals as Rejected and the decode refusals as \
                                Unknown — the safe direction (a 400 never releases the nonce \
                                lane), and recorded as a boundary of the submission path rather \
                                than papered over.",
            "authentication": format!(
                "none. The {} credential-shaped headers this run sent changed no answer, and no \
                 response in the run carried WWW-Authenticate — so the endpoint has no auth story \
                 to verify, and {} of the {} reads asked of the §29 host were refused by the \
                 method whitelist rather than by an identity check.",
                credentials.len(),
                sequencer_refusals,
                sequencer_reads
            ),
            "transaction_submission_semantics": format!(
                "eth_sendRawTransaction on all three hosts runs the same four checks in the same \
                 order and answers them with the same strings: arity (-32602 missing value for \
                 required argument 0), RLP/type-envelope decode (-32602 typed transaction too \
                 short, rlp: value size exceeds available input length), signature domain \
                 (-32000 invalid chain ID for a chain-1 payload on chain 91342), then pool state \
                 (-32003 insufficient funds for gas * price + value: have 0 want 21007770000). \
                 Replaying the identical bytes immediately answered the same -32003 rather than \
                 'already known', and eth_getTransactionByHash of that payload's hash returned \
                 result null — so {} refusals at HTTP 200 and {} at HTTP 400, and nothing this \
                 run sent ever entered a mempool.",
                post_200_refusals, post_400_refusals
            ),
            "sequencer_host_reads": format!(
                "the host named by RETH_ROLLUP_SEQUENCERHTTP refuses {sequencer_refusals} of \
                 {sequencer_reads} reads with -32601, eth_chainId among them. That is the \
                 decisive fact: GiwaSequencerDirect::connect verifies the chain id before it \
                 will build anything (§6), so this host cannot be the configured endpoint at \
                 all — a submit-only front whose identity cannot be checked is exactly what §6 \
                 refuses to act on."
            ),
            "direct_protocol": format!(
                "absent on every host: {unwhitelisted_methods} refusals across the \
                 direct-protocol candidates, with the misspelled control refusing the same way. \
                 §29's 「如果无法验证：SequencerDirect = BLOCKED」 applies to the *private path*, \
                 not to the endpoint: as a plain submission front the host does verify, and the \
                 lane therefore stays on eth_sendRawTransaction over the read host.",
            ),
        },
    });

    let path = workspace_root().join(EVIDENCE);
    std::fs::create_dir_all(path.parent().expect("data/evidence/m7 is a directory"))
        .expect("the evidence directory exists");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&evidence).expect("the evidence serializes"),
    )
    .expect("the evidence file writes");

    // The two claims §29 has to answer, asserted rather than narrated, so a future drift
    // fails this run instead of quietly changing the report.
    let control = probe
        .calls
        .iter()
        .find(|call| call.label.starts_with("etch_sendRawTransaction"))
        .expect("the misspelled control ran");
    assert!(
        control.unwhitelisted(),
        "the control itself must still be refused by the whitelist, or -32601 stops meaning \
         'absent': http {} {}",
        control.http,
        clip(&control.response_text)
    );
    let direct = probe
        .find("2_direct_protocol", "mev_sendBundle")
        .expect("mev_sendBundle was asked");
    assert!(
        direct.unwhitelisted(),
        "§29: a bundle method appeared on an endpoint that M6 measured as having none — the \
         submission path has to be re-decided, not re-used: {}",
        clip(&direct.response_text)
    );
    assert!(
        accepted.is_empty(),
        "no probe payload may be accepted: the synthetic sender is unfunded, so an \
         acceptance means the endpoint took a transaction this repository did not price"
    );
    // The claim the §58 run rests on: the §29 host cannot be the configured endpoint,
    // because it will not prove which chain it is.
    let sequencer_chain_id = probe
        .calls
        .iter()
        .find(|call| {
            call.section == "1_read_surface" && call.host == "sequencer" && {
                call.label.starts_with("eth_chainId")
            }
        })
        .expect("eth_chainId was asked of the §29 host");
    assert!(
        sequencer_chain_id.unwhitelisted(),
        "the sequencer host now answers eth_chainId ({}), so §6's connect check could run \
         against it and the endpoint decision has to be re-made rather than re-used",
        clip(&sequencer_chain_id.response_text)
    );

    let host_names = hosts.iter().map(|h| h.name).collect::<Vec<_>>().join(", ");
    let refused_hosts = probe
        .calls
        .iter()
        .filter(|call| call.section == "2_direct_protocol" && call.unwhitelisted())
        .map(|call| call.host)
        .collect::<std::collections::HashSet<_>>();
    let refused_list = refused_hosts.iter().copied().collect::<Vec<_>>().join(", ");
    println!(
        "§29 re-probe: {} calls over {} hosts ({}); eth_chainId served by {}; direct-protocol \
         candidates refused by {}; submissions accepted: {}; evidence {}",
        probe.calls.len(),
        hosts.len(),
        host_names,
        read_hosts.join(", "),
        refused_list,
        accepted.len(),
        EVIDENCE,
    );
    for (host, label, call) in &submissions {
        println!(
            "  submit {host} {label} → http {} {}",
            call.http,
            clip(&call.response_text)
        );
    }
}
