//! The one JSON-RPC endpoint both of M8.3.1's and M8.3.3's offline A/B tests read
//! through, so the two milestones provably run against the same snapshot.
//!
//! It is not a mock. It stands in for a node's *wire* behaviour — a connection, a
//! JSON-RPC request, an answer or an error — while every answer it gives is a field of
//! the committed M4 dump, which is what the real archive node said at block 37 191 169
//! (`crates/simulation/tests/real_chain.rs` recorded it once, and nothing here invents a
//! pool, a reserve or an opportunity).
//!
//! Two shapes, because the two milestones ask about different things:
//!
//! - [`Stub::spawn`] serves one request at a time on one thread. That is the shape a
//!   reuse question needs: an arrival list whose order *is* the run's order, so a cached
//!   arm's calls can be checked to be the baseline's with some removed and none
//!   reordered.
//! - [`Stub::spawn_concurrent`] answers each request on a thread of its own and holds
//!   each one open for [`RESPONSE_DELAY`] before replying. That is the shape a
//!   concurrency question needs, and the delay is why: an endpoint that answered in the
//!   time it takes to write a socket would let a run of two truly-concurrent reads and a
//!   run of two sequential ones produce the same intervals, and the trace could not tell
//!   them apart. A held-open request makes overlap something the trace measures rather
//!   than something the test hopes for.
//!
//! Both shapes enforce the same two properties, which is the part that is not a
//! convenience:
//!
//! - it holds one block. A read for any other height — including the words `latest` or
//!   `pending` — is answered with a JSON-RPC error, so a cache or a batch that drifted
//!   off the pin fails loudly instead of being served the pinned state and looking
//!   correct.
//! - it answers only what the dump recorded. A read for an account or slot the fixture
//!   never saw is an error too — a fabricated zero would let a wrong key pass as a hit.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alloy_primitives::{B256, U256};
use serde_json::{json, Value};

use super::{dump_path, BLOCK, CHAIN};

/// How long a concurrent endpoint holds a request open.
///
/// Ten milliseconds is two orders of magnitude above the local-socket round trip this
/// stub would otherwise measure, and small enough that a 39-call run still finishes in
/// well under a second per arm. It is a property of the *fixture*, not of any node: no
/// figure in any evidence file claims to be a latency a real endpoint has.
pub const RESPONSE_DELAY: Duration = Duration::from_millis(10);

/// What a stub holds: one block's state, in the exact quantity forms a node answers in.
pub struct ServedState {
    /// The height as this build's adapter sends it: `0x`-prefixed, no padding.
    height: String,
    block: Value,
    /// Lowercase address -> (balance, nonce, code), all hex quantities.
    accounts: BTreeMap<String, (String, String, String)>,
    /// (lowercase address, `0x` + 64 hex digits) -> hex quantity.
    storage: BTreeMap<(String, String), String>,
    /// The block hash, so a caller can pin to the same block the answers are from.
    pub block_hash: B256,
}

impl ServedState {
    /// Read the committed dump. Every answer below is a field of that file, so the state
    /// these tests run against is the state the live node served, not a fixture of ours.
    pub fn load() -> Self {
        let path = dump_path();
        assert!(
            path.exists(),
            "{} is missing. It is written by the live run:
    cargo test -p evm-simulation --test real_chain -- --ignored --nocapture
    These suites replay those recorded answers over a wire, so they cannot stand without it.",
            path.display()
        );
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let dump: Value = serde_json::from_str(&raw).expect("the dump is JSON");
        let header = &dump["header"];
        let hex = |value: &Value| -> String {
            let number = value
                .as_u64()
                .expect("the dump records header numbers as integers");
            format!("{number:#x}")
        };
        let quantity = |text: &str| -> String {
            let value: U256 = text
                .parse()
                .unwrap_or_else(|error| panic!("{} is not a quantity: {error}", text));
            format!("{value:#x}")
        };

        let mut accounts = BTreeMap::new();
        for (address, account) in dump["accounts"]
            .as_object()
            .expect("the dump records accounts as an object")
        {
            accounts.insert(
                address.to_ascii_lowercase(),
                (
                    quantity(account["balance"].as_str().expect("a balance")),
                    format!("{:#x}", account["nonce"].as_u64().expect("a nonce")),
                    account["code"].as_str().expect("code").to_ascii_lowercase(),
                ),
            );
        }
        let mut storage = BTreeMap::new();
        for (address, slots) in dump["storage"]
            .as_object()
            .expect("the dump records storage as an object")
        {
            for (slot, value) in slots
                .as_object()
                .expect("each address's storage is an object of slot to value")
            {
                storage.insert(
                    (
                        address.to_ascii_lowercase(),
                        normalize_slot(slot).expect("a 0x-prefixed slot"),
                    ),
                    quantity(value.as_str().expect("a storage value")),
                );
            }
        }

        Self {
            height: format!("{BLOCK:#x}"),
            block: json!({
                "number": format!("{BLOCK:#x}"),
                "hash": dump["block_hash"].as_str(),
                "timestamp": hex(&header["timestamp"]),
                "gasLimit": hex(&header["gas_limit"]),
                "miner": header["beneficiary"],
                "baseFeePerGas": hex(&header["base_fee_per_gas"]),
                "excessBlobGas": hex(&header["excess_blob_gas"]),
                "mixHash": header["prevrandao"],
            }),
            block_hash: dump["block_hash"]
                .as_str()
                .expect("the dump names the block it recorded")
                .parse()
                .expect("a block hash"),
            accounts,
            storage,
        }
    }

    /// The JSON-RPC `result` for one call, or the reason this endpoint has no answer.
    pub fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        let text = |index: usize| -> Result<String, String> {
            params
                .get(index)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{method} param {index} is not a string"))
        };
        let at_pinned_height = |sent: &str| -> Result<(), String> {
            if sent == self.height {
                return Ok(());
            }
            Err(format!(
                "this endpoint holds block {BLOCK} only and was asked for `{sent}`"
            ))
        };
        match method {
            "eth_chainId" => Ok(json!(format!("{:#x}", CHAIN.0))),
            "eth_getBlockByNumber" => {
                let sent = text(0)?;
                at_pinned_height(&sent)?;
                Ok(self.block.clone())
            }
            "eth_getCode" | "eth_getBalance" | "eth_getTransactionCount" => {
                let address = text(0)?.to_ascii_lowercase();
                at_pinned_height(&text(1)?)?;
                let account = self.accounts.get(&address).ok_or_else(|| {
                    format!(
                        "the recorded dump holds no account {address}, so this endpoint has \
                         no real answer to give and says so rather than serving a zero"
                    )
                })?;
                let value = match method {
                    "eth_getCode" => account.2.clone(),
                    "eth_getBalance" => account.0.clone(),
                    _ => account.1.clone(),
                };
                Ok(json!(value))
            }
            "eth_getStorageAt" => {
                let address = text(0)?.to_ascii_lowercase();
                let slot = normalize_slot(&text(1)?)?;
                at_pinned_height(&text(2)?)?;
                let value = self
                    .storage
                    .get(&(address.clone(), slot.clone()))
                    .ok_or_else(|| {
                        format!(
                            "the recorded dump holds no storage at {address} slot {slot}, so \
                             this endpoint answers nothing rather than a zero nobody read"
                        )
                    })?
                    .clone();
                Ok(json!(value))
            }
            other => Err(format!("this endpoint serves no {other}")),
        }
    }
}

/// One slot written two ways is one slot: the adapter sends 64 hex digits, the dump keys
/// the same way, and a test that compared strings loosely would hide a keying bug.
fn normalize_slot(text: &str) -> Result<String, String> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or_else(|| format!("`{text}` is not a hex slot"))?;
    let value = U256::from_str_radix(digits, 16).map_err(|error| format!("`{text}`: {error}"))?;
    Ok(format!("0x{value:064x}"))
}

/// One request as the endpoint saw it.
#[derive(Clone)]
pub struct Arrival {
    /// The method name, exactly as the JSON-RPC body carried it.
    pub method: String,
    /// Its params, unmodified.
    pub params: Value,
    /// This request's position in the endpoint's own accept order, taken before it is
    /// served.
    ///
    /// In [`Stub::spawn`] that is also the completion order, because one thread serves
    /// everything; in [`Stub::spawn_concurrent`] the two differ, and the difference is
    /// what an overlap claim is made of.
    pub accept_index: usize,
    /// How long the endpoint held the request open, from the moment it started serving it
    /// to the moment it wrote the reply — the server-side witness that a held-open
    /// request really was held.
    pub held_ns: u64,
}

/// A stub endpoint and its own account of what arrived.
pub struct Stub {
    url: String,
    /// What arrived, in the order the endpoint finished serving it.
    arrivals: Arc<Mutex<Vec<Arrival>>>,
    /// How many requests this endpoint is holding right now.
    outstanding: Arc<AtomicUsize>,
    /// The most it ever held at one instant — a separate cell, because this one is never
    /// decremented. Reading the high-water mark off the gauge itself would report however
    /// many requests are in flight at the moment of the read, which is zero by the time an
    /// arm has finished.
    peak: Arc<AtomicUsize>,
}

impl Stub {
    /// One thread, one request at a time — the shape a sequential endpoint has, which is
    /// what an arm that must *not* overlap is measured against.
    pub fn spawn(state: Arc<ServedState>) -> Self {
        Self::start(state, None)
    }

    /// A thread per connection, each holding its request open for `RESPONSE_DELAY` — the
    /// shape a node with several outstanding requests has.
    ///
    /// `connection: close` is sent by both shapes, so a keep-alive connection can never
    /// make two logical calls look like one arrival; here that is not a detail but the
    /// point, because the number of simultaneously-served requests is read off the number
    /// of simultaneously-open connections.
    pub fn spawn_concurrent(state: Arc<ServedState>) -> Self {
        Self::start(state, Some(RESPONSE_DELAY))
    }

    fn start(state: Arc<ServedState>, delay: Option<Duration>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port is available");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let outstanding = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&arrivals);
        // Handles for the accept loop, which lives in its own thread: the loop clones one
        // per connection and the struct keeps the original below, so the two accounts —
        // what a worker measured and what a test reads — are the same cell.
        let held = Arc::clone(&outstanding);
        let high_water = Arc::clone(&peak);
        let dispatcher = Arc::new(AtomicUsize::new(0));
        let accept = Arc::clone(&dispatcher);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let index = accept.fetch_add(1, Ordering::AcqRel);
                let state = Arc::clone(&state);
                let counted = Arc::clone(&counted);
                let held = Arc::clone(&held);
                let high_water = Arc::clone(&high_water);
                let work = move || {
                    if let Some(call) =
                        Self::serve(stream, &state, delay, index, &held, &high_water)
                    {
                        counted.lock().expect("the tally").push(call);
                    }
                };
                match delay {
                    None => work(),
                    Some(_) => {
                        thread::spawn(work);
                    }
                }
            }
        });
        Self {
            url,
            arrivals,
            outstanding,
            peak,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// What the endpoint saw, in the order requests reached it.
    ///
    /// Ordered by `accept_index`, not by the order the arrivals were pushed: a handler
    /// thread that finished writing its reply can be descheduled before it records the
    /// arrival, so completion order at the endpoint is a thread-scheduling detail. The
    /// accept order is not — it is the sequence the endpoint saw requests arrive in, which
    /// is the account that pairs with the client's dispatch order. The sink, which stamps
    /// and records on the client's own clock, is where completion order lives.
    pub fn calls(&self) -> Vec<Arrival> {
        let mut calls = self.arrivals.lock().expect("the tally").clone();
        calls.sort_by_key(|call| call.accept_index);
        calls
    }

    /// The methods answered, connect included — `eth_chainId` first in every arm, since
    /// each arm opens its own connection to its own endpoint.
    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|call| call.method).collect()
    }

    /// What the endpoint itself says about how many requests it was holding at once.
    ///
    /// This is the third account beside the trace's `max_concurrency` and the
    /// scheduler's `observed_peak`: a socket count taken on the server side of the wire,
    /// which neither of the other two can inflate.
    pub fn concurrent_peak(&self) -> usize {
        self.peak.load(Ordering::Acquire)
    }

    /// Read one JSON-RPC request, answer it, report what it asked for.
    ///
    /// Every reply says `connection: close`: a test that counts requests has to be sure
    /// a keep-alive connection cannot make two logical calls look like one arrival — and
    /// §13 of M8.3.1 keeps any keep-alive redesign out of these milestones anyway.
    fn serve(
        stream: TcpStream,
        state: &ServedState,
        delay: Option<Duration>,
        index: usize,
        held: &AtomicUsize,
        peak: &AtomicUsize,
    ) -> Option<Arrival> {
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return None;
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
            return None;
        }
        let request: Value = match serde_json::from_slice(&body) {
            Ok(request) => request,
            Err(_) => return None,
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let params = request.get("params").cloned().unwrap_or_else(|| json!([]));
        let id = request.get("id").cloned().unwrap_or(json!(1));
        // Open from here to the reply, so the count below is the number of requests this
        // endpoint was holding at once rather than the number it had accepted.
        let in_flight = held.fetch_add(1, Ordering::AcqRel) + 1;
        peak.fetch_max(in_flight, Ordering::AcqRel);
        let arrived = Instant::now();
        if let Some(hold) = delay {
            thread::sleep(hold);
        }
        let payload = match state.answer(&method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err(reason) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32000, "message": reason}
            })
            .to_string(),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\
             \r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = stream;
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        held.fetch_sub(1, Ordering::AcqRel);
        Some(Arrival {
            method,
            params,
            accept_index: index,
            held_ns: arrived.elapsed().as_nanos() as u64,
        })
    }
}
