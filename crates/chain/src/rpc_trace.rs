//! M8.2 §6, §12, §18: one JSON-RPC call's own record, read off the calls the run
//! was already going to make.
//!
//! This module exists to answer a question the M8.1 baseline could only raise: a
//! simulation stage that took 23.9 s was filed under *reads-and-compute* because a
//! node read was known to happen while the span was open, but nothing yet said how
//! many reads there were, how long each one took, whether they queued behind each
//! other, or whether the same state word was asked for twice. Those are §24's
//! questions, and every one of them is a question about **the calls, not about the
//! stage**.
//!
//! ```text
//! where the record is taken   HttpChainAdapter::request_traced — the one place this
//!                             repository turns a method name and a params array into
//!                             bytes on the wire
//! what the path gains         two `Instant` reads per call, one small key string
//!                             built from the params already in hand, one push into a
//!                             Vec behind a per-simulation Mutex
//! what the path does NOT gain a request (§18), a retry (§19), a reorder, a cache, a
//!                             batch, a disk write, a per-call serialization (§31)
//! ```
//!
//! ## Why the sink is attached rather than global
//!
//! §31 forbids a global singleton and §6 requires every call to be attributable to
//! *one* simulation. One move satisfies both: the code that starts a simulation makes
//! a sink, hands it to a derived clone of the adapter
//! ([`HttpChainAdapter::with_rpc_trace`][crate::rpc::HttpChainAdapter::with_rpc_trace]),
//! and reads that sink back when the simulation is over. An adapter clone without a
//! sink behaves as it always did — every recording call sits behind a `None` test — so
//! the connection pool, the request order and the single retry inside
//! [`request_with`][crate::rpc::HttpChainAdapter::request_with] are untouched. The one
//! new thing on that path is observation.
//!
//! ## What a `duration_ns` measures, and what it cannot
//!
//! An event's duration is the whole logical call: request construction, the POST, the
//! node's own work, the response body and its decode — because the choke point cannot
//! tell a slow node apart from a slow decode. §17 names this and allows it: the
//! sub-split is `breakdown_unavailable`, and that string goes into the evidence
//! instead of leaving a reader to infer it. What the same point *can* see is the
//! retry: [`RpcCallEvent::attempts`] carries one entry per HTTP attempt, so a call
//! that burned a 20 s client timeout and then succeeded is a different shape from one
//! that took 20 s in a single attempt — and §25's E against its B turns on which of
//! the two happened.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

/// The schema version of one recorded event, so a later reader of
/// `simulation-traces.jsonl` knows which fields were promised when it was written.
///
/// Version 2 adds [`RpcCallEvent::slot`]: M8.3.2 §6 asks what each of a simulation's
/// storage reads reads, and until now the slot existed only inside `dedup_key`, as one
/// of that key's five terms. A reader that wanted it had to take apart a string another
/// module builds — so the field is stated rather than re-derived. Nothing else moved: a
/// version-1 line and a version-2 line describe the same call.
pub const RPC_TRACE_SCHEMA: u64 = 2;

/// The largest event list one sink will hold.
///
/// A cap is here because a sink lives in memory for the length of a simulation, and a
/// run that produced an unbounded number of reads must not be able to grow the process
/// without limit. It is not a tuning knob: past 20 000 calls a simulation is already a
/// story about call volume. Every call dropped beyond the cap is counted in
/// [`RpcTraceSink::dropped_events`] and refused by name, so a truncated list reads as
/// a truncation rather than as a run that made fewer calls (§46).
pub const MAX_EVENTS_PER_SINK: usize = 20_000;

/// The length a failure message is cut at, so one bad endpoint cannot make an
/// evidence line the size of a block.
const MAX_ERROR_DETAIL_CHARS: usize = 240;

/// One HTTP attempt inside one logical call.
///
/// A logical call is what the caller asked for; an attempt is what the wire saw. The
/// adapter retries a transport or HTTP-status failure once, so a run whose node
/// flattered and failed would otherwise look like one slow call, and the distinction
/// §25 draws between node latency and a connection that had to be re-made rests on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RpcAttempt {
    pub started_ns: u64,
    pub finished_ns: u64,
    pub duration_ns: u64,
    /// `ok`, or the class of the failure this attempt ended with — the same closed set
    /// as [`RpcCallEvent::error_class`].
    pub outcome: &'static str,
}

/// One JSON-RPC call, as the choke point saw it.
///
/// Every field is either a copy of something the caller already passed in, a reading
/// of the monotonic clock, or a classification of an error this build produced. None
/// of them is learned by asking the node anything (§18), and no duration here comes
/// from a wall clock (§7): the `*_ns` fields are nanoseconds since this sink's
/// `origin`, which is the run's own monotonic start, so an event and a latency trace
/// stage from the same run sit on one timeline and subtract cleanly (§2.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RpcCallEvent {
    pub trace_schema: u64,
    /// Position in this sink's call order, 1-based. Unique within a simulation.
    pub rpc_id: u64,
    /// The literal method string this build put on the wire — not a name this module
    /// invented for it.
    pub method: String,
    /// The block argument as it was sent, normalized: a hex height becomes decimal, a
    /// tag stays the tag. `None` when the method takes no block.
    pub block: Option<String>,
    /// The account, contract or hash the call is about, when the params name one.
    pub target: Option<String>,
    /// The storage word an `eth_getStorageAt` asked for, zero-padded to 64 hex digits —
    /// the same normalized text [`describe_call`] puts into that method's `dedup_key`, and
    /// read off the same params element. `None` for every other method, which is why this
    /// is a field rather than a rule a reader applies to the key: M8.3.2 §6 wants the 20
    /// storage reads of a simulation listed one per word, and no other method has a slot.
    pub slot: Option<String>,
    pub started_ns: u64,
    pub finished_ns: u64,
    /// `finished_ns - started_ns`, floored at zero: the whole logical call, its retries
    /// included. §17 names the sub-split this does not have.
    pub duration_ns: u64,
    pub success: bool,
    /// A closed set, so a summary can group by it without inventing categories:
    /// [`CLASS_SEND_FAILED`], [`CLASS_NON_JSON`], [`CLASS_HTTP_STATUS`],
    /// [`CLASS_NODE_REJECTED`], [`CLASS_DECODE_FAILED`]. `None` when it succeeded.
    pub error_class: Option<&'static str>,
    /// The failure's own message, with this adapter's endpoint string replaced by
    /// `<endpoint>` (a transport error quotes the URL it was given, and an endpoint is
    /// not something an evidence file should carry) and cut at
    /// [`MAX_ERROR_DETAIL_CHARS`] with the cut marked.
    pub error_detail: Option<String>,
    pub attempts: Vec<RpcAttempt>,
    /// The canonical identity of the state this call asked for, when §12 defines one
    /// for its method and the params actually parse. A call with no key is still
    /// counted — it is only excluded from the duplicate tally, which is the honest
    /// direction §12 asks for.
    pub dedup_key: Option<String>,
    /// Why there is no `dedup_key`: [`DEDUP_KEY_UNAVAILABLE_FOR_METHOD`] (this build's
    /// §12 list does not cover it) or [`DEDUP_KEY_PARAMS_UNREADABLE`] (the params were
    /// not the shape the rule assumes). Never a guess in its place.
    pub key_note: Option<&'static str>,
}

/// §12's note when a method is outside the key list this build can name.
pub const DEDUP_KEY_UNAVAILABLE_FOR_METHOD: &str = "dedup_key_unavailable_for_method";

/// §12's note when the params did not arrive in the shape the key rule expects.
pub const DEDUP_KEY_PARAMS_UNREADABLE: &str = "dedup_key_params_unreadable";

/// The error classes a call can end in at this choke point, plus the one an
/// individual attempt stores when it did not fail.
pub const CLASS_SEND_FAILED: &str = "send_failed";
pub const CLASS_NON_JSON: &str = "non_json_response";
pub const CLASS_HTTP_STATUS: &str = "http_status";
pub const CLASS_NODE_REJECTED: &str = "node_rejected";
pub const CLASS_DECODE_FAILED: &str = "decode_failed";
pub const CLASS_OK: &str = "ok";

/// §17's own words, stored beside a summary that had no way to split them: what this
/// build measures is the whole provider call, and request construction, the wait on
/// the node, and response decoding are not separable at the point the record is taken.
pub const BREAKDOWN_UNAVAILABLE: &str = "breakdown_unavailable";

/// Where one sink's calls came from, in §20's three words.
///
/// §20 requires live, replay and fixture to be separable and never blended; the
/// cheapest way to hold that is to make the source a property of the sink, set by
/// whoever opens it, rather than something a summary has to infer afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcTraceSource {
    /// A node answering over the network, in real time.
    Live,
    /// Recorded blocks and state read from disk; issues no node calls at all.
    Replay,
    /// A deterministic in-process stand-in. Never presented as market latency.
    Fixture,
}

impl RpcTraceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Replay => "replay",
            Self::Fixture => "fixture",
        }
    }
}

/// The in-memory event list one simulation writes into, and the handle the adapter
/// holds.
///
/// `Clone` shares the list (a handle, not a second buffer), because the adapter is
/// cloned by design to share its connection pool and each clone has to land in the
/// same place. The lock is held only long enough to push one already-built event —
/// never across an `await` — so this is not a lock on the request path, and it is
/// per-simulation rather than global (§31).
#[derive(Clone)]
pub struct RpcTraceSink {
    inner: Arc<Inner>,
}

struct Inner {
    origin: Instant,
    simulation_id: String,
    source: RpcTraceSource,
    chain_id: Option<u64>,
    /// Filled once, by whoever knows the endpoint. A `OnceLock` rather than a field
    /// because a sink is shared by clone (the adapter is cloned on purpose) and an
    /// `Arc` cannot be assigned through — the alternative, `Arc::get_mut`, would
    /// silently skip the setting whenever a clone already existed, and a skipped
    /// setting here means an endpoint can travel into an evidence file.
    endpoint: OnceLock<String>,
    /// A one-way digest of [`Self::endpoint`], set in the same call: M8.3.2 §8 asks whether
    /// one simulation's storage reads and M8.3.2 §14's reads outside it went to the *same*
    /// provider. That is a question about identity, and an identity can be stated as a
    /// digest — the URL itself stays out of the evidence file, because a transport error
    /// quotes the URL it was given and an endpoint may carry a credential in its path.
    endpoint_id: OnceLock<String>,
    events: Mutex<Vec<RpcCallEvent>>,
    /// What this instrumentation refused to do, in the order it refused it. A refusal
    /// is a fact about the observer and is reported as one — never dropped, never a
    /// panic (§29, and the same rule M8.1's `instrumentation_refusals` holds).
    refusals: Mutex<Vec<String>>,
    next_id: AtomicU64,
    dropped: AtomicU64,
}

impl RpcTraceSink {
    /// A sink whose clock reads are offsets of `origin`.
    ///
    /// `origin` is the run's own monotonic start (`Clock::origin_instant()`), so the
    /// events line up with the latency trace's stage stamps without a second clock
    /// being introduced (§7, and §2.3's reason for it).
    pub fn new(
        origin: Instant,
        simulation_id: impl Into<String>,
        source: RpcTraceSource,
        chain_id: Option<u64>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                origin,
                simulation_id: simulation_id.into(),
                source,
                chain_id,
                endpoint: OnceLock::new(),
                endpoint_id: OnceLock::new(),
                events: Mutex::new(Vec::new()),
                refusals: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                dropped: AtomicU64::new(0),
            }),
        }
    }

    /// The sink's own identity, which is the simulation's: §6 requires every record to
    /// be attributable to one simulation, and a sink is opened for exactly one.
    pub fn simulation_id(&self) -> &str {
        &self.inner.simulation_id
    }

    pub fn source(&self) -> RpcTraceSource {
        self.inner.source
    }

    pub fn chain_id(&self) -> Option<u64> {
        self.inner.chain_id
    }

    /// The endpoint string to scrub out of any message stored in an event.
    ///
    /// The first endpoint given wins; a later call cannot replace it, because
    /// scrubbing that depended on the order sinks were passed around in would be a
    /// leak with a timing to it.
    pub fn with_endpoint(self, endpoint: &str) -> Self {
        let _ = self.inner.endpoint.set(endpoint.to_owned());
        let _ = self.inner.endpoint_id.set(endpoint_id(endpoint));
        self
    }

    /// The digest identity of this sink's endpoint, or `None` when nobody told the sink
    /// where its calls went. Emitted in place of the URL (§8's question, answered without
    /// answering it by printing a credential).
    pub fn endpoint_id(&self) -> Option<&str> {
        self.inner.endpoint_id.get().map(String::as_str)
    }

    /// Nanoseconds since this sink's origin, floored rather than wrapped: a stamp, not
    /// a subtraction a reader has to redo.
    pub fn mark(&self, at: Instant) -> u64 {
        at.duration_since(self.inner.origin)
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Events accepted so far, whether or not any were later dropped.
    pub fn recorded_events(&self) -> usize {
        events_lock(&self.inner).len()
    }

    /// Calls that arrived after [`MAX_EVENTS_PER_SINK`] and were therefore not kept.
    pub fn dropped_events(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    pub fn refusals(&self) -> Vec<String> {
        refusals_lock(&self.inner).clone()
    }

    /// A copy of the events, in call order, for whoever persists them.
    ///
    /// Cloned rather than borrowed so a summary can be built while the simulation's
    /// adapter still holds the sink — analyzing a Vec in place would be reading a list
    /// something else is pushing to.
    pub fn events(&self) -> Vec<RpcCallEvent> {
        events_lock(&self.inner).clone()
    }

    /// Push one finished event. Called by the choke point after its last `await`.
    pub fn record(&self, mut event: RpcCallEvent) {
        if let Some(endpoint) = self.inner.endpoint.get().map(String::as_str) {
            if let Some(detail) = event.error_detail.as_deref() {
                // Only when the two actually overlap: the rewrite exists to keep an
                // endpoint out of an evidence file, not to restate every message.
                if detail.contains(endpoint) {
                    event.error_detail = Some(detail.replace(endpoint, "<endpoint>"));
                }
            }
        }
        let mut guard = self.inner.events.lock().unwrap_or_else(into_events);
        if guard.len() >= MAX_EVENTS_PER_SINK {
            drop(guard);
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
            self.refuse(format!(
                "event {} was not stored: one sink holds at most {MAX_EVENTS_PER_SINK} calls, \
                 and the dropped count is reported rather than the list being quietly shorter",
                event.rpc_id
            ));
            return;
        }
        guard.push(event);
    }

    /// Remember that the instrumentation declined to do something — a write that
    /// arrived after the list was closed, a sink asked to record nothing.
    pub fn refuse(&self, note: String) {
        refusals_lock(&self.inner).push(note);
    }

    /// The next `rpc_id`. Taken before the request starts, from an atomic counter
    /// rather than the event list, so no lock is held across the await.
    pub fn next_rpc_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// The event list, poison-resistant: a poisoned lock still holds its data, and
/// letting the instrumentation panic over that would make it the reason a run failed,
/// which §2.1 forbids.
fn events_lock(inner: &Inner) -> MutexGuard<'_, Vec<RpcCallEvent>> {
    inner.events.lock().unwrap_or_else(into_events)
}

fn into_events<'a>(
    error: PoisonError<MutexGuard<'a, Vec<RpcCallEvent>>>,
) -> MutexGuard<'a, Vec<RpcCallEvent>> {
    error.into_inner()
}

fn refusals_lock(inner: &Inner) -> MutexGuard<'_, Vec<String>> {
    inner.refusals.lock().unwrap_or_else(into_refusals)
}

fn into_refusals<'a>(
    error: PoisonError<MutexGuard<'a, Vec<String>>>,
) -> MutexGuard<'a, Vec<String>> {
    error.into_inner()
}

/// What [`describe_call`] learned about a params array before the call was made.
///
/// A separate type because the choke point needs this *before* `params` is moved into
/// the request body, and because §12's key rules are the part a reader has to be able
/// to test without a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcCallDescription {
    pub block: Option<String>,
    pub target: Option<String>,
    /// The normalized storage word, for `eth_getStorageAt` only. See
    /// [`RpcCallEvent::slot`].
    pub slot: Option<String>,
    pub dedup_key: Option<String>,
    pub key_note: Option<&'static str>,
}

/// §12's canonical identity of what one call asked for, plus the two fields a timeline
/// needs beside it.
///
/// The rules are §12's and no others:
///
/// ```text
/// eth_getStorageAt         (chain, block, address, slot)
/// eth_getBalance           (chain, block, address)
/// eth_getCode              (chain, block, address)
/// eth_getTransactionCount  (chain, block, address)  — block is what was sent, so a
///                                                    `latest` read never key-matches a
///                                                    numbered one
/// eth_call                 (chain, block, to, data)  — plus `value` when the request
///                                                    carries one
/// eth_getBlockByNumber     (chain, block, hydrated)
/// eth_getBlockByHash       (chain, hash, hydrated)
/// anything else            dedup_key_unavailable_for_method
/// ```
///
/// A key is built from the params this call already carries, so the cost is one pass
/// over a small JSON value and one `String`; nothing here asks the node (§18).
pub fn describe_call(method: &str, params: &Value, chain_id: Option<u64>) -> RpcCallDescription {
    let chain = match chain_id {
        Some(id) => id.to_string(),
        // A call made before the chain id is known — the `eth_chainId` of a connect —
        // gets `chain-unknown` rather than a key that claims an identity this build
        // does not have yet.
        None => "chain-unknown".to_string(),
    };
    let field = |index: usize| -> Option<&Value> {
        params
            .as_array()
            .and_then(|list| list.get(index))
            .filter(|value| !value.is_null())
    };
    let text = |index: usize| -> Option<String> {
        field(index).and_then(Value::as_str).map(str::to_owned)
    };
    let call_field = |name: &str| -> Option<String> {
        field(0)
            .and_then(|call| call.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };

    // The block argument, wherever this method puts it, normalized so one height
    // written two ways is one key.
    let block = match method {
        "eth_getStorageAt" => text(2).as_deref().map(normalize_block_param),
        "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" | "eth_call" => {
            text(1).as_deref().map(normalize_block_param)
        }
        "eth_getBlockByNumber" => text(0).as_deref().map(normalize_block_param),
        "eth_getBlockByHash" | "eth_getLogs" | "eth_chainId" | "eth_blockNumber" => None,
        _ => None,
    };

    let target = match method {
        "eth_getStorageAt" | "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" => {
            text(0)
        }
        "eth_call" => call_field("to"),
        "eth_getBlockByNumber" => block.clone(),
        "eth_getBlockByHash" => text(0).as_deref().map(normalize_hash),
        _ => None,
    };

    // §6's storage word. Read from the same params element the key is built from and
    // normalized by the same function, so the field and the key cannot name two different
    // words for one call — which is the only way a per-slot list can be checked against
    // the duplicate tally beside it.
    let slot = if method == "eth_getStorageAt" {
        text(1).as_deref().map(normalize_slot)
    } else {
        None
    };

    // Keyed per §12. `None` from a builder below means the params were not the shape
    // that rule assumes — which is reported, not patched over.
    let dedup_key = match method {
        "eth_getStorageAt" => match (text(0).as_deref(), slot.as_deref(), block.as_deref()) {
            (Some(address), Some(word), Some(height)) => Some(format!(
                "storage|{chain}|{height}|{}|{word}",
                normalize_address(address)
            )),
            _ => None,
        },
        "eth_getBalance" => match (text(0).as_deref(), block.as_deref()) {
            (Some(address), Some(height)) => Some(format!(
                "balance|{chain}|{height}|{}",
                normalize_address(address)
            )),
            _ => None,
        },
        "eth_getCode" => match (text(0).as_deref(), block.as_deref()) {
            (Some(address), Some(height)) => Some(format!(
                "code|{chain}|{height}|{}",
                normalize_address(address)
            )),
            _ => None,
        },
        "eth_getTransactionCount" => match (text(0).as_deref(), block.as_deref()) {
            (Some(address), Some(height)) => Some(format!(
                "nonce|{chain}|{height}|{}",
                normalize_address(address)
            )),
            _ => None,
        },
        "eth_call" => match (
            call_field("to").as_deref(),
            call_field("data").as_deref(),
            call_field("value").as_deref(),
            block.as_deref(),
        ) {
            (Some(to), Some(data), value, Some(height)) => Some(format!(
                "call|{chain}|{height}|{}|{}{}",
                normalize_address(to),
                normalize_calldata(data),
                match value {
                    Some(amount) => format!("|value|{amount}"),
                    // This build's `CallRequest` carries `to` and `data` and no value,
                    // so the key has those two and the README says so, rather than
                    // assuming every eth_call in the repository is value-free.
                    None => String::new(),
                }
            )),
            _ => None,
        },
        "eth_getBlockByNumber" => match (block.as_deref(), field(1).and_then(Value::as_bool)) {
            (Some(height), Some(hydrated)) => {
                Some(format!("block|{chain}|{height}|hydrated|{hydrated}"))
            }
            _ => None,
        },
        "eth_getBlockByHash" => match (text(0).as_deref(), field(1).and_then(Value::as_bool)) {
            (Some(hash), Some(hydrated)) => Some(format!(
                "blockhash|{chain}|{}|hydrated|{hydrated}",
                normalize_hash(hash)
            )),
            _ => None,
        },
        _ => None,
    };

    let listed = matches!(
        method,
        "eth_getStorageAt"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getTransactionCount"
            | "eth_call"
            | "eth_getBlockByNumber"
            | "eth_getBlockByHash"
    );
    let key_note = match (&dedup_key, listed) {
        (Some(_), _) => None,
        (None, true) => Some(DEDUP_KEY_PARAMS_UNREADABLE),
        (None, false) => Some(DEDUP_KEY_UNAVAILABLE_FOR_METHOD),
    };

    RpcCallDescription {
        block,
        target,
        slot,
        dedup_key,
        key_note,
    }
}

/// A block argument as §12 wants it: a `0x` height becomes its decimal value; a tag
/// (`latest`, `pending`, `finalized`, `safe`) stays exactly the tag it was sent as, so
/// a tag read can never key-match a numbered one.
///
/// Only an explicit `0x` is read as hex. A provider quantity always carries that
/// prefix, so a bare digit string is not a hex number with the prefix missing — it is
/// something else, and guessing hex for it would turn block `37594591` into
/// `928597393` and key two different blocks as one word of state. Kept verbatim, it
/// still normalizes to the same text the prefixed form produces.
fn normalize_block_param(raw: &str) -> String {
    match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => match u64::from_str_radix(hex, 16) {
            Ok(number) => number.to_string(),
            // `0x` and then not a number: not a height this build can name, so it is
            // stored as it arrived rather than as a zero that would key real blocks.
            Err(_) => raw.to_owned(),
        },
        None => raw.to_owned(),
    }
}

/// An address, lowercased and `0x`-prefixed, so one account written in two checksum
/// cases is one key rather than two.
///
/// Public because M8.3.2 §6 and §9 group the evidence by address, and a group built by
/// lowercasing the text a call carried would be a *second* normalization of the same
/// field — one that could disagree with the dedup key beside it. The rows and the key
/// are meant to be the same string produced by this function.
pub fn normalize_address(raw: &str) -> String {
    let stripped = raw.strip_prefix("0x").unwrap_or(raw);
    format!("0x{}", stripped.to_lowercase())
}

/// A storage slot, zero-padded to 64 hex digits: slot `0x8` and slot `0x0…08` are one
/// word of state, and a key that split them would under-count duplicates.
fn normalize_slot(raw: &str) -> String {
    let stripped = raw.strip_prefix("0x").unwrap_or(raw).to_lowercase();
    let stripped = stripped.trim_start_matches('0');
    format!("0x{:0>64}", stripped)
}

fn normalize_hash(raw: &str) -> String {
    let stripped = raw.strip_prefix("0x").unwrap_or(raw);
    format!("0x{}", stripped.to_lowercase())
}

/// Calldata, lowered so one encoding written in a different case is one key.
fn normalize_calldata(raw: &str) -> String {
    let stripped = raw.strip_prefix("0x").unwrap_or(raw);
    stripped.to_lowercase()
}

/// Cut a failure message at [`MAX_ERROR_DETAIL_CHARS`] without letting the cut pass
/// itself off as the whole message.
pub fn bounded_detail(text: &str) -> String {
    match text.char_indices().nth(MAX_ERROR_DETAIL_CHARS) {
        Some((index, _)) => format!("{}…[cut]", &text[..index]),
        None => text.to_owned(),
    }
}

/// An endpoint's identity as a digest prefix, in the shape M8.1's trace ids already use
/// (`derive_trace_id` in `crates/metrics/src/trace.rs`): two sinks that name the same
/// endpoint name the same id, and no evidence file ever holds the URL. §8 asks a question
/// about whether reads share a provider; that is answerable by comparison, and comparison
/// does not require publication.
fn endpoint_id(url: &str) -> String {
    let hash = alloy_primitives::keccak256(url.as_bytes());
    format!("rpc-{}", &hash.to_string()[2..18])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn description(method: &str, params: Value) -> RpcCallDescription {
        describe_call(method, &params, Some(91_342))
    }

    /// §12's rule, and the five ways it can be got wrong: one storage word written two
    /// ways must key the same, and a different slot, block, address or chain must not.
    /// A duplicate tally that missed either direction would under- or over-count the
    /// one number M8.3 is meant to act on.
    #[test]
    fn storage_key_carries_chain_block_address_and_slot() {
        let base = description("eth_getStorageAt", json!(["0xAbC", "0x8", "0x23da5df"]));
        assert_eq!(base.block.as_deref(), Some("37594591"));
        let key = base.dedup_key.clone().unwrap_or_default();
        assert_eq!(
            key,
            format!(
                "storage|91342|37594591|0xabc|{}",
                "0x0000000000000000000000000000000000000000000000000000000000000008"
            )
        );

        let spelled_another_way = description(
            "eth_getStorageAt",
            json!([
                "0xabc",
                "0x0000000000000000000000000000000000000000000000000000000000000008",
                "37594591"
            ]),
        );
        assert_eq!(
            spelled_another_way.dedup_key, base.dedup_key,
            "one word, two spellings, one key"
        );

        for (what, params) in [
            ("a different slot", json!(["0xabc", "0x9", "0x23da5df"])),
            ("a different block", json!(["0xabc", "0x8", "0x23da5e0"])),
            ("a different address", json!(["0xdef", "0x8", "0x23da5df"])),
        ] {
            let other = description("eth_getStorageAt", params);
            assert_ne!(other.dedup_key, base.dedup_key, "{what}");
        }

        let other_chain = describe_call(
            "eth_getStorageAt",
            &json!(["0xabc", "0x8", "0x23da5df"]),
            Some(1),
        );
        assert_ne!(other_chain.dedup_key, base.dedup_key, "a different chain");
    }

    /// §6's own field, and the one way it could be wrong: if the slot the event names and
    /// the slot inside the dedup key came from different normalizations, a per-slot list
    /// and the duplicate tally beside it would be two accounts of the same call.
    #[test]
    fn the_slot_field_is_the_same_word_the_dedup_key_carries() {
        let short = description("eth_getStorageAt", json!(["0xAbC", "0x8", "0x23da5df"]));
        let padded = description(
            "eth_getStorageAt",
            json!([
                "0xabc",
                "0x0000000000000000000000000000000000000000000000000000000000000008",
                "0x23da5df"
            ]),
        );
        let word = "0x0000000000000000000000000000000000000000000000000000000000000008";
        assert_eq!(short.slot.as_deref(), Some(word));
        assert_eq!(
            padded.slot, short.slot,
            "one word, two spellings, one slot text"
        );
        for description_row in [&short, &padded] {
            assert!(
                description_row
                    .dedup_key
                    .as_deref()
                    .unwrap_or_default()
                    .ends_with(word),
                "the key and the field must name one word: {:?}",
                description_row.dedup_key
            );
        }

        // A params array the key rule cannot read gives no slot either — the field is not
        // a second chance to guess what the key refused to build.
        let unreadable = description("eth_getStorageAt", json!(["0xabc"]));
        assert_eq!(unreadable.slot, None);

        for (method, params) in [
            ("eth_getCode", json!(["0xabc", "0x1"])),
            ("eth_getBalance", json!(["0xabc", "0x1"])),
            ("eth_call", json!([{"to": "0xabc", "data": "0x01"}, "0x1"])),
        ] {
            assert_eq!(description(method, params).slot, None, "{method}");
        }
    }

    /// §8's question is whether two sets of calls share a provider, and the answer has to
    /// be comparable without printing the endpoint. An id every URL maps onto would answer
    /// it falsely, so the second half of this test is the control: a different endpoint has
    /// to produce a different id.
    #[test]
    fn an_endpoint_has_an_identity_that_compares_without_being_published() {
        let first = RpcTraceSink::new(Instant::now(), "sim-a", RpcTraceSource::Live, Some(1))
            .with_endpoint("https://rpc.invalid/token/abc123");
        let same_again = RpcTraceSink::new(Instant::now(), "sim-b", RpcTraceSource::Live, Some(1))
            .with_endpoint("https://rpc.invalid/token/abc123");
        let other = RpcTraceSink::new(Instant::now(), "sim-c", RpcTraceSource::Live, Some(1))
            .with_endpoint("https://rpc.invalid/token/dead456");

        let id = first.endpoint_id().unwrap_or_default();
        assert_eq!(id, same_again.endpoint_id().unwrap_or_default());
        assert_ne!(
            id,
            other.endpoint_id().unwrap_or_default(),
            "one id per endpoint"
        );
        assert!(id.starts_with("rpc-") && id.len() == 20, "{id}");
        for text in [id, first.simulation_id()] {
            assert!(
                !text.contains("abc123") && !text.contains("rpc.invalid"),
                "the endpoint travelled into an id: {text}"
            );
        }

        // A sink nobody told where its calls went says so rather than inventing an id.
        let unlabelled = RpcTraceSink::new(Instant::now(), "sim-d", RpcTraceSource::Live, Some(1));
        assert_eq!(unlabelled.endpoint_id(), None);
    }

    /// A tag is not a height, and a key that equated them would call two reads of two
    /// different blocks one read of one block.
    #[test]
    fn a_block_tag_is_never_the_same_key_as_a_block_number() {
        let numbered = description("eth_getBalance", json!(["0xabc", "0x23da5df"]));
        let latest = description("eth_getBalance", json!(["0xabc", "latest"]));
        assert_eq!(numbered.block.as_deref(), Some("37594591"));
        assert_eq!(latest.block.as_deref(), Some("latest"));
        assert_ne!(numbered.dedup_key, latest.dedup_key);
        assert_eq!(
            latest.dedup_key.as_deref(),
            Some("balance|91342|latest|0xabc")
        );
    }

    #[test]
    fn call_key_uses_the_calldata_it_was_given_and_says_it_had_no_value() {
        let one = description(
            "eth_call",
            json!([
                {"to": "0xA57c4b51750f87ff6487D2eC07B8AFF7bd6D027D", "data": "0x0902F1AC"},
                "0x1"
            ]),
        );
        assert_eq!(
            one.dedup_key.as_deref(),
            Some("call|91342|1|0xa57c4b51750f87ff6487d2ec07b8aff7bd6d027d|0902f1ac")
        );
        let with_value = description(
            "eth_call",
            json!([
                {"to": "0xA57c4b51750f87ff6487D2eC07B8AFF7bd6D027D", "data": "0x0902f1ac", "value": "0x0"},
                "0x1"
            ]),
        );
        assert!(
            with_value
                .dedup_key
                .unwrap_or_default()
                .ends_with("|value|0x0"),
            "a request that carries a value must not key like one that does not"
        );
    }

    /// §5's "if the provider does not use a method, do not manufacture a category" and
    /// §12's "if no canonical key can be built, say so" are one rule from two sides: an
    /// unlisted method is counted, and it is not handed a key.
    #[test]
    fn methods_outside_the_key_list_are_counted_and_say_why_they_have_no_key() {
        for (method, params) in [
            ("eth_chainId", json!([])),
            ("eth_blockNumber", json!([])),
            (
                "eth_getLogs",
                json!([{"fromBlock": "0x1", "toBlock": "0x2"}]),
            ),
            ("eth_getBlockReceipts", json!(["0x1"])),
            ("some_future_method", json!(["x"])),
        ] {
            let seen = description(method, params);
            assert_eq!(seen.dedup_key, None, "{method} must not be handed a key");
            assert_eq!(
                seen.key_note,
                Some(DEDUP_KEY_UNAVAILABLE_FOR_METHOD),
                "{method}"
            );
        }
    }

    /// A method on §12's list whose params are not the shape the rule assumes gets the
    /// *other* note — and still no key, and still a count.
    #[test]
    fn unreadable_params_get_their_own_note_rather_than_a_borrowed_key() {
        for (method, params) in [
            ("eth_getStorageAt", json!(["0xabc"])),
            ("eth_getCode", json!(["0xabc", null])),
            ("eth_getBlockByNumber", json!(["0x1"])),
            ("eth_call", json!([{"data": "0x0902f1ac"}, "0x1"])),
        ] {
            let seen = description(method, params);
            assert_eq!(seen.dedup_key, None, "{method}");
            assert_eq!(seen.key_note, Some(DEDUP_KEY_PARAMS_UNREADABLE), "{method}");
        }
    }

    #[test]
    fn an_unlisted_chain_id_is_named_as_unknown_rather_than_borrowing_a_number() {
        let unknown = describe_call("eth_getCode", &json!(["0xabc", "0x1"]), None);
        assert_eq!(
            unknown.dedup_key.as_deref(),
            Some("code|chain-unknown|1|0xabc")
        );
    }

    #[test]
    fn events_are_stamped_against_the_sinks_own_origin_and_in_call_order() {
        let origin = Instant::now();
        let sink = RpcTraceSink::new(origin, "sim-1", RpcTraceSource::Live, Some(91_342));
        assert_eq!(sink.mark(origin), 0);
        assert_eq!(
            sink.mark(origin + std::time::Duration::from_millis(1)),
            1_000_000
        );

        for index in 0..3 {
            let at = Instant::now();
            sink.record(RpcCallEvent {
                trace_schema: RPC_TRACE_SCHEMA,
                rpc_id: sink.next_rpc_id(),
                method: "eth_getCode".to_string(),
                block: Some("1".to_string()),
                target: Some(format!("0x{index}")),
                slot: None,
                started_ns: sink.mark(at),
                finished_ns: sink.mark(at),
                duration_ns: 0,
                success: true,
                error_class: None,
                error_detail: None,
                attempts: vec![RpcAttempt {
                    started_ns: sink.mark(at),
                    finished_ns: sink.mark(at),
                    duration_ns: 0,
                    outcome: CLASS_OK,
                }],
                dedup_key: Some(format!("code|91342|1|0x{index}")),
                key_note: None,
            });
        }
        let events = sink.events();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.iter().map(|event| event.rpc_id).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "rpc_id is the call order, not a hash of anything"
        );
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].started_ns <= pair[1].started_ns),
            "a monotonic origin cannot put a later call earlier"
        );
    }

    /// §29 asks that no duration be negative. Two stamps taken out of order would
    /// subtract time off the run, so the floor is the invariant and this says so.
    #[test]
    fn a_reversed_stamp_pair_floors_at_zero_instead_of_going_negative() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-2", RpcTraceSource::Fixture, None);
        let late = 50_000;
        let early = 45_000;
        sink.record(RpcCallEvent {
            trace_schema: RPC_TRACE_SCHEMA,
            rpc_id: sink.next_rpc_id(),
            method: "eth_chainId".to_string(),
            block: None,
            target: None,
            slot: None,
            started_ns: late,
            finished_ns: early,
            duration_ns: early.saturating_sub(late),
            success: false,
            error_class: Some(CLASS_SEND_FAILED),
            error_detail: Some("post failed".to_string()),
            attempts: Vec::new(),
            dedup_key: None,
            key_note: Some(DEDUP_KEY_UNAVAILABLE_FOR_METHOD),
        });
        assert_eq!(sink.events()[0].duration_ns, 0);
    }

    /// The endpoint never travels into an event, even though a transport error quotes
    /// the URL it was given.
    #[test]
    fn an_error_that_names_the_endpoint_is_scrubbed_before_it_is_stored() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-3", RpcTraceSource::Live, Some(1))
            .with_endpoint("https://rpc.invalid/token/abc123");
        sink.record(RpcCallEvent {
            trace_schema: RPC_TRACE_SCHEMA,
            rpc_id: sink.next_rpc_id(),
            method: "eth_getBlockByNumber".to_string(),
            block: Some("1".to_string()),
            target: Some("1".to_string()),
            slot: None,
            started_ns: 0,
            finished_ns: 1,
            duration_ns: 1,
            success: false,
            error_class: Some(CLASS_SEND_FAILED),
            error_detail: Some(
                "error sending request for url (https://rpc.invalid/token/abc123)".to_string(),
            ),
            attempts: Vec::new(),
            dedup_key: None,
            key_note: Some(DEDUP_KEY_UNAVAILABLE_FOR_METHOD),
        });
        let detail = sink.events()[0].error_detail.clone().unwrap_or_default();
        assert!(!detail.contains("abc123"), "endpoint leaked: {detail}");
        assert!(
            detail.contains("<endpoint>"),
            "scrub did not happen: {detail}"
        );

        // A message that never named the endpoint is stored as it came, not rewritten.
        sink.record(RpcCallEvent {
            error_detail: Some("http status 503".to_string()),
            ..sink.events()[0].clone()
        });
        assert_eq!(
            sink.events()[1].error_detail.as_deref(),
            Some("http status 503")
        );
    }

    #[test]
    fn a_long_failure_message_is_cut_and_says_it_was_cut() {
        let long = "x".repeat(MAX_ERROR_DETAIL_CHARS + 50);
        let cut = bounded_detail(&long);
        assert!(cut.ends_with("…[cut]"), "{cut}");
        assert!(cut.chars().count() <= MAX_ERROR_DETAIL_CHARS + 7);
        assert_eq!(bounded_detail("http status 503"), "http status 503");
    }

    /// A cap that silently shortened the list would turn "this simulation made 40 000
    /// calls" into "…made 20 000 calls" — an invented number of exactly the kind §46
    /// forbids. Both halves are reported.
    #[test]
    fn a_full_sink_drops_later_calls_and_counts_them_as_dropped() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-4", RpcTraceSource::Fixture, None);
        let event = RpcCallEvent {
            trace_schema: RPC_TRACE_SCHEMA,
            rpc_id: 0,
            method: "eth_chainId".to_string(),
            block: None,
            target: None,
            slot: None,
            started_ns: 0,
            finished_ns: 0,
            duration_ns: 0,
            success: true,
            error_class: None,
            error_detail: None,
            attempts: Vec::new(),
            dedup_key: None,
            key_note: None,
        };
        for _ in 0..MAX_EVENTS_PER_SINK + 2 {
            let mut event = event.clone();
            event.rpc_id = sink.next_rpc_id();
            sink.record(event);
        }
        assert_eq!(sink.recorded_events(), MAX_EVENTS_PER_SINK);
        assert_eq!(sink.dropped_events(), 2);
        assert_eq!(
            sink.refusals().len(),
            2,
            "each drop is a refusal a reader sees"
        );
    }

    #[test]
    fn normalization_keeps_its_own_promises() {
        assert_eq!(normalize_slot("0x8"), format!("0x{:0>64}", "8"));
        assert_eq!(normalize_slot("0x0"), normalize_slot("0"));
        assert_eq!(
            normalize_slot("0x000000000000000000000000000000000000000000000000000000000000000a"),
            normalize_slot("0xA")
        );
        assert_eq!(normalize_address("0xAbC"), "0xabc");
        assert_eq!(normalize_block_param("0x10"), "16");
        assert_eq!(normalize_block_param("pending"), "pending");
        // The bug this rule exists to prevent: an unprefixed decimal read as hex would
        // move the block by a factor of ~16^k and key two heights as one.
        assert_eq!(normalize_block_param("37594591"), "37594591");
        assert_eq!(normalize_block_param("0x23da5df"), "37594591");
        assert_eq!(normalize_hash("0xABC"), "0xabc");
    }
}
