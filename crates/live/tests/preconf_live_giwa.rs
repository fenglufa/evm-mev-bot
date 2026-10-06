//! M9.4 §30/§46/§48 — the one target in this crate that talks to a chain: a real
//! preconfirmation window against the official GIWA Flashblocks endpoint, with the
//! canonical host sampled in the same window, so the lead and the reconciliation have one
//! stopwatch behind them.
//!
//! It is `#[ignore]`: `cargo test --workspace` must never spend a request by accident.
//! Both endpoints come from the environment and neither is ever printed:
//!
//! ```text
//! GIWA_FLASHBLOCKS_RPC_URL=<preconfirmation endpoint> \
//! GIWA_RPC_URL=<canonical endpoint> \
//! cargo test -p evm-live --test preconf_live_giwa -- --ignored \
//!     --nocapture --test-threads=1
//! ```
//!
//! Optional knobs, all scalers of one run rather than separate experiments:
//!
//! ```text
//! M94_WINDOW_MS              wall length of the window        (default 15000)
//! M94_READ_INTERVAL_MS       the link's cadence               (default 250)
//! M94_CANONICAL_INTERVAL_MS  the canonical arm's cadence      (default 500)
//! M94_RAW_DIR                where the capture goes           (default the committed raw/)
//! M94_VERBATIM_BYTES         byte budget for verbatim answers (default 1500000)
//! ```
//!
//! The capture is two files, not one: [`projection`] keeps a recomputable line for *every*
//! read (a pending answer repeats almost all of the previous read's transactions, so the
//! verbatim form is the part that does not need to be bounded), and the verbatim answers are
//! kept for the bounded prefix the byte budget covers. That is §31's rule — minimal
//! replayable sample, capture metadata, hashes, full statistics — applied to a stream that
//! would otherwise be tens of megabytes per window.
//!
//! # What this run is allowed to read
//!
//! Two shapes of one pair of methods, and nothing else:
//!
//! * `eth_getBlockByNumber(["pending", true])` on the preconfirmation host — the read the
//!   decoder was written against (audit §2.5: 10/10 samples answered full transaction
//!   objects, 0 hash-only). Same method, same params as the canonical pending path already
//!   uses, so §34's ban on a new production RPC holds: no new method, and no
//!   `eth_blockNumber`, `eth_getLogs` or `eth_call` anywhere near it.
//! * `eth_getBlockByNumber(["latest", true])` on the canonical host, plus
//!   `eth_getBlockByNumber(["0x…", true])` only for a height the latest poll jumped over —
//!   the closing side of §15, one read per sealed block.
//!
//! `eth_chainId` is issued once per `connect`, counted in the capture metadata rather than
//! hidden. Every read either arm performs gets a line with its index, so the RPC totals in
//! the evidence tables are counts of lines, not claims.
//!
//! # Why the transport wrapper exists
//!
//! [`evm_live::PollingFrameSource`] reads [`evm_chain::HeadReader::pending_raw`], and the
//! HTTP adapter's implementation asks for `full: false` — the shape the M5 candidate
//! source needs, where a pending block's *size* is the observation. A pending block whose
//! transactions are hashes names no target, so the M9.4 decoder refuses it fail-closed
//! ([`evm_live::PreconfError::HashOnlyTransaction`]). The gap is the read shape, not the
//! method, so this file supplies a tests-only reader that asks the same endpoint for the
//! full objects. Doing that here rather than in `crates/chain` keeps §57's production diff
//! at zero and keeps the choice visible: a reader has to decide to ask for the heavy shape.
//!
//! # What is committed, and what is not
//!
//! The capture (`raw/`): every read of the pending arm as a compact projection with its
//! payload byte count and digest, the verbatim provider answers for the bounded prefix the
//! byte budget covers, every canonical block as the digest the radar was handed, the event
//! stream the link emitted, and the run report. The payloads are the stream's whole truth
//! and the window is bounded, so the sample is small enough to keep — §31's warning is about
//! an *unbounded* stream, and this run cannot become one: a deadline flips the stop flag and
//! the link returns.
//!
//! Nothing in `raw/` carries a URL. Transport errors are the one place a URL can arrive by
//! accident, because an HTTP client quotes the address it failed to reach, so every detail
//! string passes through [`Scrub`] before it is written; the gate then asserts the
//! directory holds no `://` at all (§33).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use evm_chain::{
    ChainAdapter, ChainBlock, ChainError, HeadReader, HttpChainAdapter, RpcTraceSink,
    RpcTraceSource,
};
use evm_core::{BlockNumber, ChainId};
use evm_live::{
    now_unix_ms, CanonicalDigest, EarlyRadar, FrameSource, LinkConfig, PollingFrameSource, PoolSet,
    PreconfError, PreconfLink, RadarConfig, RadarEvent,
};

/// Where M9.2 recorded the edges it priced, and therefore where a pool address that
/// *exists* comes from. §43's ban on fabricated market data applies to the pool set a live
/// run stands on: an invented address would make the affected-pool count mean nothing.
const M92_GRAPH_REL: &str = "data/evidence/m9/m9.2/graph-integration.json";

fn env_required(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| {
        panic!(
            "{key} is required: this target reads two live endpoints, and no URL is \
             compiled into the repository (§33 keeps URLs out of evidence, §38 keeps any \
             other provider out of the code)"
        )
    })
}

fn env_number(key: &str, default: u64) -> u64 {
    match env::var(key) {
        Ok(text) => text
            .parse()
            .unwrap_or_else(|_| panic!("{key}={text} is not a number of milliseconds")),
        Err(_) => default,
    }
}

/// The digest of an endpoint, taken through the helper the canonical RPC trace already
/// uses, so this milestone's endpoint ids are comparable with M8 and M9.1–M9.3 rather than
/// a second spelling of the same fact.
fn endpoint_digest(url: &str) -> String {
    let sink = RpcTraceSink::new(
        Instant::now(),
        "m9.4-live-window",
        RpcTraceSource::Live,
        None,
    )
    .with_endpoint(url);
    sink.endpoint_id()
        .unwrap_or_else(|| panic!("a sink handed an endpoint answers with its id"))
        .to_string()
}

fn hex_to_u64(value: Option<&Value>) -> Option<u64> {
    let text = value.and_then(Value::as_str)?;
    u64::from_str_radix(text.strip_prefix("0x").unwrap_or(text), 16).ok()
}

fn parse_b256(value: Option<&Value>) -> Option<B256> {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<B256>().ok())
}

/// A URL never leaves this process. The adapter's own scrubbing lives inside its trace
/// sink and does not cover a message this harness writes, so the replacement happens here,
/// over every URL the run was given.
struct Scrub {
    needles: Vec<String>,
}

impl Scrub {
    fn new(urls: &[String]) -> Self {
        let mut needles: Vec<String> = urls.iter().filter(|url| !url.is_empty()).cloned().collect();
        // Longest first: one endpoint's URL can be a prefix of another's, and replacing the
        // shorter needle first would leave the longer one half-scrubbed.
        needles.sort_by_key(|a| std::cmp::Reverse(a.len()));
        Self { needles }
    }

    fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for needle in &self.needles {
            out = out.replace(needle.as_str(), "endpoint-redacted");
        }
        // A scheme survives a needle that named only the host, and §33 forbids the
        // substring `://` in this directory whatever its length.
        if out.contains("://") {
            out = "endpoint-redacted".to_string();
        }
        out
    }
}

// — the tests-only full-transaction reader.

/// `eth_getBlockByNumber(["pending", true])`, and nothing more.
struct PendingFullReader {
    adapter: HttpChainAdapter,
    reads: Arc<AtomicU64>,
    scrub: Arc<Scrub>,
}

#[async_trait]
impl HeadReader for PendingFullReader {
    fn transport(&self) -> &'static str {
        "http"
    }

    async fn head(&mut self) -> evm_chain::Result<BlockNumber> {
        self.adapter.latest_block().await
    }

    async fn block_at(&mut self, number: BlockNumber) -> evm_chain::Result<Option<ChainBlock>> {
        match self.adapter.get_block(number).await {
            Ok(block) => Ok(Some(block)),
            Err(ChainError::MissingData(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn pending_raw(&mut self) -> evm_chain::Result<Option<Value>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let raw = self
            .adapter
            .request_raw("eth_getBlockByNumber", json!(["pending", true]))
            .await
            .map_err(|error| ChainError::Rpc(self.scrub.apply(&error.to_string())))?;
        Ok(if raw.is_null() { None } else { Some(raw) })
    }
}

/// The compact, recomputable face of one pending payload: every field the evidence tables
/// and the independent recompute read, and nothing that only makes the file bigger.
///
/// §31 asks for a *minimal replayable sample* plus capture metadata, hashes and full
/// statistics, and forbids committing an unbounded stream. A window of this shape is
/// ~30 KB per read, and a read repeats almost all of the previous read's transactions, so
/// the verbatim payloads are kept for a bounded prefix of the window (see
/// [`RecordingFrameSource`]) and every read of the window keeps this projection with its
/// payload hash. The recompute half then works on projections, which carry the whole of
/// what any table asserts, and the gate proves the projection faithful by re-deriving it
/// from each verbatim payload it has one of.
fn projection(view: &Value) -> Value {
    let keys: Vec<&str> = view
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let transactions: Vec<Value> = view
        .get("transactions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|entry| match entry {
                    Value::String(text) => json!({"shape": "hash_only", "hash": text}),
                    Value::Object(_) => json!({
                        "shape": "full_object",
                        "hash": entry.get("hash"),
                        "index": entry.get("transactionIndex"),
                        "from": entry.get("from"),
                        "to": entry.get("to"),
                        "selector": entry
                            .get("input")
                            .and_then(Value::as_str)
                            .map(|text| text.chars().take(10).collect::<String>()),
                    }),
                    other => json!({"shape": "neither_object_nor_hash", "value": other}),
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "header_keys": keys,
        "number": view.get("number"),
        "hash": view.get("hash"),
        "parent_hash": view.get("parentHash"),
        "state_root": view.get("stateRoot"),
        "chain_timestamp": view.get("timestamp"),
        "gas_used": view.get("gasUsed"),
        "miner": view.get("miner"),
        "base_fee_per_gas": view.get("baseFeePerGas"),
        "transactions": transactions,
    })
}

/// The canonical serialization of a provider answer: one string, so the byte count, the
/// digest and the verbatim copy all describe exactly the same text.
fn payload_text(view: &Value) -> String {
    serde_json::to_string(view).expect("a provider answer serializes")
}

/// The payload digest, over that text. `serde_json::Value` keeps its map sorted, so the
/// same provider answer hashes the same in the capture and in the gate (§41's determinism
/// rule applied to a hash rather than to a number).
fn payload_digest(text: &str) -> String {
    alloy_primitives::keccak256(text.as_bytes()).to_string()
}

/// The link's frame supply, wrapped so every read it makes is written down as it happened.
///
/// The recording is a *transport-side* record: it notes what the provider answered before
/// the decoder looked at it, so an independent recomputation never has to trust a decoded
/// value to know what was read (§32).
struct RecordingFrameSource {
    inner: PollingFrameSource<PendingFullReader>,
    log: Arc<Mutex<Vec<Value>>>,
    payloads: Arc<Mutex<Vec<Value>>>,
    verbatim_bytes_left: Arc<AtomicU64>,
    served: Arc<AtomicU64>,
    scrub: Arc<Scrub>,
}

#[async_trait]
impl FrameSource for RecordingFrameSource {
    fn endpoint_id(&self) -> &str {
        self.inner.endpoint_id()
    }

    fn transport(&self) -> &'static str {
        self.inner.transport()
    }

    async fn next_view(&mut self) -> Result<Option<Value>, PreconfError> {
        let index = self.served.fetch_add(1, Ordering::SeqCst);
        let at = now_unix_ms();
        let outcome = self.inner.next_view().await;
        let line = match &outcome {
            Ok(Some(view)) => {
                let text = payload_text(view);
                let bytes = text.len() as u64;
                let digest = payload_digest(&text);
                // The verbatim copy is kept while the byte budget lasts. A payload that
                // does not fit leaves the budget alone, so a run of heavy blocks cannot
                // consume the sample by outbidding it; the leftover is reported in meta.
                let keep = self
                    .verbatim_bytes_left
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        if left >= bytes && left > 0 {
                            Some(left - bytes)
                        } else {
                            None
                        }
                    })
                    .is_ok();
                if keep {
                    lock(&self.payloads).push(json!({
                        "read_index": index,
                        "at_unix_ms": at,
                        "payload_bytes": bytes,
                        "payload_digest": digest,
                        "view": view,
                    }));
                }
                json!({
                    "kind": "pending_answer",
                    "read_index": index,
                    "at_unix_ms": at,
                    "payload_bytes": bytes,
                    "payload_digest": digest,
                    "verbatim_included": keep,
                    "projection": projection(view),
                })
            }
            Ok(None) => json!({
                "kind": "pending_null",
                "read_index": index,
                "at_unix_ms": at,
            }),
            Err(error) => json!({
                "kind": "pending_error",
                "read_index": index,
                "at_unix_ms": at,
                "detail": self.scrub.apply(&error.to_string()),
            }),
        };
        lock(&self.log).push(line);
        outcome
    }
}

// — the canonical arm.

/// One sealed block, as the radar's closing side needs it. Fail-closed: a block whose
/// identity cannot be read is refused and counted, never completed by a guess (§5/§6).
fn digest_from_block(
    chain_id: ChainId,
    block: &Value,
    observed_at_unix_ms: u64,
) -> Result<CanonicalDigest, String> {
    let number = hex_to_u64(block.get("number"))
        .ok_or_else(|| "`number` missing or not a hex quantity".to_string())?;
    let hash = parse_b256(block.get("hash"))
        .ok_or_else(|| "`hash` missing or not a 32-byte hex string".to_string())?;
    let parent_hash = parse_b256(block.get("parentHash"))
        .ok_or_else(|| "`parentHash` missing or not a 32-byte hex string".to_string())?;
    let chain_timestamp_secs = hex_to_u64(block.get("timestamp"))
        .ok_or_else(|| "`timestamp` missing or not a hex quantity".to_string())?;
    let transactions = block
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or_else(|| "`transactions` missing or not a list".to_string())?;
    let mut transaction_hashes = Vec::with_capacity(transactions.len());
    for entry in transactions {
        let hash = match entry {
            Value::Object(object) => parse_b256(object.get("hash")),
            Value::String(text) => text.parse::<B256>().ok(),
            _ => None,
        }
        .ok_or_else(|| "a transaction entry names no readable hash".to_string())?;
        transaction_hashes.push(hash);
    }
    Ok(CanonicalDigest {
        chain_id,
        number: BlockNumber(number),
        hash,
        parent_hash,
        chain_timestamp_secs,
        transaction_hashes,
        // Not read. `eth_getBlockReceipts` would be a third method and this window asks
        // for two, so the canonical side carries no log evidence. The table writes that as
        // `N/A` rather than as an empty set (§28).
        affected_pools: None,
        observed_at_unix_ms,
    })
}

/// The closing side's whole environment in one bundle, so the arm stays a function of
/// one value rather than ten positional arguments a caller can transpose silently.
struct CanonicalArmEnv {
    adapter: HttpChainAdapter,
    chain_id: ChainId,
    endpoint_id: String,
    scrub: Arc<Scrub>,
    tx: mpsc::Sender<CanonicalDigest>,
    log: Arc<Mutex<Vec<Value>>>,
    reads: Arc<AtomicU64>,
    pending_served: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    interval: Duration,
}

/// The closing side: poll `latest`, hand each newly sealed height to the link, and record
/// every read with the pending-read count at the moment of sending — the position a replay
/// has to insert the digest back at.
async fn canonical_arm(env: CanonicalArmEnv) {
    let CanonicalArmEnv {
        adapter,
        chain_id,
        endpoint_id,
        scrub,
        tx,
        log,
        reads,
        pending_served,
        stop,
        interval,
    } = env;
    let mut last_seen: Option<u64> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let index = reads.fetch_add(1, Ordering::SeqCst);
        let at = now_unix_ms();
        let line = match adapter
            .request_raw("eth_getBlockByNumber", json!(["latest", true]))
            .await
        {
            Ok(block) if block.is_null() => json!({
                "kind": "canonical_null",
                "read_index": index,
                "at_unix_ms": at,
                "params": ["latest", true],
            }),
            Ok(block) => match digest_from_block(chain_id, &block, at) {
                Ok(digest) => {
                    let number = digest.number.0;
                    let mut gap_reads = Vec::new();
                    if let Some(previous) = last_seen {
                        for missing in (previous + 1)..number {
                            gap_reads.push(
                                read_missing_by_number(
                                    &adapter, chain_id, missing, &reads, &tx, &scrub,
                                )
                                .await,
                            );
                        }
                    }
                    last_seen = Some(number);
                    let delivered = tx.send(digest.clone()).await.is_ok();
                    json!({
                        "kind": "canonical_digest",
                        "read_index": index,
                        "at_unix_ms": at,
                        "params": ["latest", true],
                        "endpoint_id": endpoint_id,
                        "pending_reads_at_send": pending_served.load(Ordering::SeqCst),
                        "digest": digest,
                        "delivered": delivered,
                        "gap_reads": gap_reads,
                    })
                }
                Err(detail) => json!({
                    "kind": "canonical_refused",
                    "read_index": index,
                    "at_unix_ms": at,
                    "params": ["latest", true],
                    "detail": detail,
                }),
            },
            Err(error) => json!({
                "kind": "canonical_error",
                "read_index": index,
                "at_unix_ms": at,
                "params": ["latest", true],
                "detail": scrub.apply(&error.to_string()),
            }),
        };
        lock(&log).push(line);
        tokio::time::sleep(interval).await;
    }
}

/// One height the `latest` poll jumped over. It is read by number so that no sealed block
/// inside the window is silently missing from the closing side.
async fn read_missing_by_number(
    adapter: &HttpChainAdapter,
    chain_id: ChainId,
    number: u64,
    reads: &AtomicU64,
    tx: &mpsc::Sender<CanonicalDigest>,
    scrub: &Scrub,
) -> Value {
    let index = reads.fetch_add(1, Ordering::SeqCst);
    let at = now_unix_ms();
    let params = json!([format!("0x{number:x}"), true]);
    match adapter
        .request_raw("eth_getBlockByNumber", params.clone())
        .await
    {
        Ok(block) if block.is_null() => json!({
            "kind": "canonical_gap_null",
            "read_index": index,
            "at_unix_ms": at,
            "params": params,
        }),
        Ok(block) => match digest_from_block(chain_id, &block, at) {
            Ok(digest) => json!({
                "kind": "canonical_gap_digest",
                "read_index": index,
                "at_unix_ms": at,
                "params": params,
                "digest": digest,
                "delivered": tx.send(digest).await.is_ok(),
            }),
            Err(detail) => json!({
                "kind": "canonical_refused",
                "read_index": index,
                "at_unix_ms": at,
                "params": params,
                "detail": detail,
            }),
        },
        Err(error) => json!({
            "kind": "canonical_error",
            "read_index": index,
            "at_unix_ms": at,
            "params": params,
            "detail": scrub.apply(&error.to_string()),
        }),
    }
}

// — the pools.

/// The pool addresses M9.2 measured and priced, read out of its committed edge table,
/// lowercase because that is how the file spells them and how a comparison is a byte
/// comparison.
fn real_pool_set(workspace: &Path, chain_id: ChainId) -> (EarlyPools, u64) {
    let path = workspace.join(M92_GRAPH_REL);
    let text = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is required: the pool set an affected-pool count is measured against comes \
             from committed measurement, not from a fixture",
            path.display()
        )
    });
    let document: Value = serde_json::from_str(&text).expect("M9.2's evidence is JSON");
    let mut addresses: Vec<String> = document
        .get("edges")
        .and_then(Value::as_array)
        .map(|edges| {
            edges
                .iter()
                .filter_map(|edge| {
                    edge.get("id")
                        .and_then(|id| id.get("pool"))
                        .and_then(|pool| pool.get("address"))
                        .and_then(Value::as_str)
                        .map(str::to_ascii_lowercase)
                })
                .collect()
        })
        .unwrap_or_default();
    addresses.sort();
    addresses.dedup();
    let mut parsed = Vec::with_capacity(addresses.len());
    for text in &addresses {
        let address = text
            .parse::<Address>()
            .unwrap_or_else(|_| panic!("{text} is not an address in M9.2's edge table"));
        parsed.push(address);
    }
    let graph_block = document
        .get("graph_block")
        .and_then(Value::as_u64)
        .or_else(|| document.get("target_block").and_then(Value::as_u64))
        .unwrap_or(0);
    (
        EarlyPools {
            set: PoolSet::new(chain_id, parsed),
            addresses,
        },
        graph_block,
    )
}

struct EarlyPools {
    set: PoolSet,
    addresses: Vec<String>,
}

fn lock(log: &Mutex<Vec<Value>>) -> MutexGuard<'_, Vec<Value>> {
    match log.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_jsonl(path: &Path, log: &Mutex<Vec<Value>>) {
    let lines = lock(log);
    let text = lines
        .iter()
        .map(|line| serde_json::to_string(line).expect("a raw line serializes"))
        .collect::<Vec<String>>()
        .join("\n");
    fs::write(path, format!("{text}\n")).expect("the raw capture is writable");
    std::mem::drop(lines);
}

fn write_json(path: &Path, value: &Value) {
    let text = serde_json::to_string_pretty(value).expect("evidence serializes");
    fs::write(path, format!("{text}\n")).expect("the raw capture is writable");
}

// — the run.

#[tokio::test]
#[ignore]
async fn one_real_preconfirmation_window_against_the_official_endpoint() {
    let flashblocks_url = env_required("GIWA_FLASHBLOCKS_RPC_URL");
    let canonical_url = env_required("GIWA_RPC_URL");
    let window_ms = env_number("M94_WINDOW_MS", 15_000);
    let read_interval_ms = env_number("M94_READ_INTERVAL_MS", 250);
    let canonical_interval_ms = env_number("M94_CANONICAL_INTERVAL_MS", 500);

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/live sits two levels below the workspace root")
        .to_path_buf();
    let raw_dir = match env::var("M94_RAW_DIR") {
        Ok(text) if Path::new(&text).is_absolute() => PathBuf::from(text),
        Ok(text) => workspace.join(text),
        Err(_) => workspace.join("data/evidence/m9/m9.4/raw"),
    };
    fs::create_dir_all(&raw_dir).expect("the raw capture directory is creatable");

    let scrub = Arc::new(Scrub::new(&[
        flashblocks_url.clone(),
        canonical_url.clone(),
    ]));
    let flashblocks_endpoint = endpoint_digest(&flashblocks_url);
    let canonical_endpoint = endpoint_digest(&canonical_url);
    assert_ne!(
        flashblocks_endpoint, canonical_endpoint,
        "the two arms are two providers; if they digest alike, one URL was passed twice \
         and every lead in this window would be a comparison of a host with itself"
    );

    let fl_adapter = HttpChainAdapter::connect(&flashblocks_url)
        .await
        .expect("the preconfirmation endpoint answers eth_chainId");
    let canon_adapter = HttpChainAdapter::connect(&canonical_url)
        .await
        .expect("the canonical endpoint answers eth_chainId");
    let chain_id = fl_adapter.chain_id();
    assert_eq!(
        chain_id,
        canon_adapter.chain_id(),
        "both arms must describe the same chain, or the lead compares two chains"
    );

    let (pools, graph_block) = real_pool_set(&workspace, chain_id);
    assert!(
        !pools.addresses.is_empty(),
        "an affected-pool claim needs registered pools to be about"
    );

    let pending_log: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let payload_log: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let canonical_log: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let event_log: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let pending_reads = Arc::new(AtomicU64::new(0));
    let pending_served = Arc::new(AtomicU64::new(0));
    let canonical_reads = Arc::new(AtomicU64::new(0));
    let verbatim_budget = env_number("M94_VERBATIM_BYTES", 1_500_000);
    let verbatim_bytes_left = Arc::new(AtomicU64::new(verbatim_budget));

    let source = RecordingFrameSource {
        inner: PollingFrameSource::new(
            PendingFullReader {
                adapter: fl_adapter,
                reads: Arc::clone(&pending_reads),
                scrub: Arc::clone(&scrub),
            },
            flashblocks_endpoint.clone(),
        ),
        log: Arc::clone(&pending_log),
        payloads: Arc::clone(&payload_log),
        verbatim_bytes_left: Arc::clone(&verbatim_bytes_left),
        served: Arc::clone(&pending_served),
        scrub: Arc::clone(&scrub),
    };

    let radar = EarlyRadar::new(
        chain_id,
        flashblocks_endpoint.clone(),
        pools.set,
        RadarConfig::default(),
    );
    let mut link = PreconfLink::new(
        source,
        radar,
        LinkConfig {
            read_interval_ms,
            ..LinkConfig::default()
        },
    );

    let stop = Arc::new(AtomicBool::new(false));
    // Capacity 1 on the closing side: a digest is handed over when the link has room for
    // it, so the recorded `pending_reads_at_send` is within one read of the position the
    // link consumed it at. A wider queue would let the closing side run ahead of the view
    // it closes, and §19's late-frame behaviour would then be an artefact of a buffer size.
    let (ctx, crx) = mpsc::channel::<CanonicalDigest>(1);
    let (etx, mut erx) = mpsc::channel::<RadarEvent>(256);

    let collector_log = Arc::clone(&event_log);
    let collector = tokio::spawn(async move {
        let mut index = 0u64;
        while let Some(event) = erx.recv().await {
            let value = serde_json::to_value(&event).expect("an event serializes");
            lock(&collector_log).push(json!({
                "event_index": index,
                "recorded_at_unix_ms": now_unix_ms(),
                "event": value,
            }));
            index += 1;
        }
        index
    });

    let canonical_task = {
        let stop = Arc::clone(&stop);
        let scrub = Arc::clone(&scrub);
        let log = Arc::clone(&canonical_log);
        let reads = Arc::clone(&canonical_reads);
        let served = Arc::clone(&pending_served);
        let endpoint_id = canonical_endpoint.clone();
        tokio::spawn(async move {
            canonical_arm(CanonicalArmEnv {
                adapter: canon_adapter,
                chain_id,
                endpoint_id,
                scrub,
                tx: ctx,
                log,
                reads,
                pending_served: served,
                stop,
                interval: Duration::from_millis(canonical_interval_ms),
            })
            .await;
        })
    };

    let deadline = {
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(window_ms)).await;
            stop.store(true, Ordering::Relaxed);
        })
    };

    let started_at_unix_ms = now_unix_ms();
    let started = Instant::now();
    let report = link
        .run(etx, crx, &mut now_unix_ms, Arc::clone(&stop))
        .await
        .expect("a bounded window ends by deadline, not by failure");
    let elapsed_ms = started.elapsed().as_millis() as u64;
    deadline.abort();
    canonical_task
        .await
        .expect("the canonical arm stops with the flag");

    let events_delivered = collector
        .await
        .expect("the collector ran to the closed sink");

    // §30's mandatory records, plus the accounting this run must not be able to hide: the
    // source asked for exactly as many reads as its log has lines.
    assert_eq!(
        lock(&pending_log).len() as u64,
        pending_served.load(Ordering::Relaxed),
        "every read the source was asked for has exactly one line"
    );
    assert_eq!(
        pending_reads.load(Ordering::Relaxed),
        pending_served.load(Ordering::Relaxed),
        "the reader counted the same requests the source counted"
    );
    assert!(
        report.counters.frames_accepted > 0,
        "a window that accepted no frame says nothing about the protocol; the report is \
         written either way and its counters say what happened"
    );
    assert!(
        report.reads_answered >= 5,
        "§48 needs a sequence, which needs several reads; a window this short is reported \
         as insufficient traffic, not as evidence"
    );

    let meta = json!({
        "milestone": "M9.4",
        "kind": "live-window-capture",
        "chain_id": chain_id.0,
        "flashblocks_endpoint": flashblocks_endpoint,
        "canonical_endpoint": canonical_endpoint,
        "flashblocks_transport": "http-poll-pending",
        "canonical_transport": "http",
        "methods_requested": ["eth_chainId", "eth_getBlockByNumber"],
        "chain_id_reads": 2,
        "window_ms_requested": window_ms,
        "window_ms_elapsed": elapsed_ms,
        "started_at_unix_ms": started_at_unix_ms,
        "read_interval_ms": read_interval_ms,
        "canonical_interval_ms": canonical_interval_ms,
        "pending_reads_issued": pending_reads.load(Ordering::Relaxed),
        "canonical_reads_issued": canonical_reads.load(Ordering::Relaxed),
        "pending_payload_bytes_total": lock(&pending_log)
            .iter()
            .filter_map(|line| line.get("payload_bytes").and_then(Value::as_u64))
            .sum::<u64>(),
        "pending_answer_reads": lock(&pending_log)
            .iter()
            .filter(|line| line.get("kind").and_then(Value::as_str) == Some("pending_answer"))
            .count() as u64,
        "verbatim_payload_budget_bytes": verbatim_budget,
        "verbatim_payload_bytes_left": verbatim_bytes_left.load(Ordering::Relaxed),
        "verbatim_payload_reads": lock(&payload_log).len() as u64,
        "capture_layout": {
            "pending-reads.jsonl": "one line per read of the preconfirmation arm — the \
                                    transport-side answer index, wall clock, payload byte \
                                    count, payload digest, and the projection of every \
                                    field any table or recompute reads",
            "pending-payloads.jsonl": "the verbatim provider answers, kept for the bounded \
                                      prefix of the window the byte budget covers (§31's \
                                      minimal replayable sample); each line carries the \
                                      read_index and digest that pairs it with the \
                                      projection beside it",
            "canonical-reads.jsonl": "one line per read of the closing side, with the \
                                      pending-read count at the moment of sending",
            "events.jsonl": "the radar event stream, in delivery order",
            "live-run-report.json": "the link's own report and this metadata",
        },
        "pool_set_size": pools.addresses.len(),
        "pool_set_source": M92_GRAPH_REL,
        "pool_set_graph_block": graph_block,
        "events_delivered": events_delivered,
        "link_config": link.config(),
        "radar_config": link.radar().config(),
        "no_url_in_this_file": true,
    });

    write_jsonl(&raw_dir.join("pending-reads.jsonl"), &pending_log);
    write_jsonl(&raw_dir.join("pending-payloads.jsonl"), &payload_log);
    write_jsonl(&raw_dir.join("canonical-reads.jsonl"), &canonical_log);
    write_jsonl(&raw_dir.join("events.jsonl"), &event_log);
    write_json(
        &raw_dir.join("live-run-report.json"),
        &json!({
            "_provenance": {
                "asked": "§30/§46/§48: what the official preconfirmation endpoint actually \
                          answered over one bounded window, in the link's own words.",
                "produced_by": "crates/live/tests/preconf_live_giwa.rs",
                "produced_at_unix_ms": now_unix_ms(),
                "no_url_in_this_file": true,
            },
            "meta": meta,
            "report": serde_json::to_value(&report).expect("a run report serializes"),
        }),
    );

    let counters = report.counters;
    println!(
        "M9.4 live window: chain {} · heights {} · frames offered {} accepted {} rejected {} \
         duplicate {} · heights with >1 view {} · heights with >=3 views {} · transactions {} \
         · affected pools {} (target {}, log {}) · closures {} · reads {} pending + {} \
         canonical · elapsed {} ms · ended {:?}",
        chain_id.0,
        counters.heights_seen,
        counters.frames_offered,
        counters.frames_accepted,
        counters.frames_rejected,
        counters.frames_duplicate,
        counters.heights_multi_view,
        counters.heights_ge_three_views,
        counters.transactions_observed,
        counters.affected_pools_emitted,
        counters.affected_pools_by_target,
        counters.affected_pools_by_log,
        counters.canonical_closures,
        pending_reads.load(Ordering::Relaxed),
        canonical_reads.load(Ordering::Relaxed),
        elapsed_ms,
        report.ended_by,
    );
}
