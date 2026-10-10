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
//! the connection pool, the request order and the retry policy on the read path are
//! untouched ([`request_with`][crate::rpc::HttpChainAdapter::request_with] still asks a
//! node twice when a connection drops). The one new thing on that path is observation.
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
//!
//! ## M8.4.1: who asked, stated by the caller rather than inferred from a clock
//!
//! M8.4.1 §4 and §9 ask each call to carry the stage it belongs to and the code that
//! issued it. Until now the only attribution available was the pipeline's
//! containment test — "this call's `started_ns` falls inside that stage's span" — and
//! that test has a proven blind spot: the execution ladder's spans are written from
//! *millisecond* stamps, so a nanosecond call cannot be claimed to sit inside them, and
//! a call in one of those gaps classifies as `unknown` however obvious its author is to
//! someone who has read the code. So the label is stamped instead of inferred:
//!
//! ```text
//! who stamps        the code that issues the call, through [`RpcTraceSink::set_context`]
//! who reads it      [`HttpChainAdapter`][crate::rpc::HttpChainAdapter]'s one call path, at the
//!                   same instant it takes the start stamp
//! what it costs     one clone of two short strings per call, on the traced path only
//! when it lies      never, because the sink refuses to call an ambiguous label exact:
//!                   [`RpcCallEvent::context_note`] says so when a second call was
//!                   outstanding at this sink, or when the label was replaced while this
//!                   call was in flight
//! ```
//!
//! The note matters more than the label. A stamped label is a claim about the *sink's*
//! state at one instant; it becomes a claim about *this call* only if no other call
//! could have been issued under the same stamp. With concurrency 1 that holds and the
//! note is absent; with a batch of independent reads in flight it does not, and the
//! event says so rather than shipping a label that two calls share. The pipeline's
//! containment test stays in place beside it — the two are separate accounts of the
//! same question, and where they disagree the disagreement is the finding.

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
///
/// Version 3 adds [`RpcCallEvent::stage`], [`RpcCallEvent::caller`] and
/// [`RpcCallEvent::context_note`] — M8.4.1 §4/§9's "who issued this, from what code",
/// stated by the issuing site instead of inferred from a time window. The delta is
/// additive and every one of the three is optional, which is why
/// [`RPC_TRACE_SCHEMA_READABLE`] lists 2 as well: a version-2 line is a complete account
/// of the same call that simply predates the question: it carries no stage, no caller and
/// no note, because the build that wrote it could not have stamped any of them.
/// What a reader must not do is read that absence as "this call had no stage".
pub const RPC_TRACE_SCHEMA: u64 = 3;

/// The event schemas this build can read, oldest first.
///
/// A range rather than one number because the three fields version 3 adds are optional:
/// rejecting a version-2 line would throw away evidence that was written honestly, and
/// the only thing the reader loses is the attribution M8.4.1 asked for. Every other
/// mismatch — a newer schema, a field of the wrong shape, a class word this build does
/// not emit — stays fatal, because a guessed field in an evidence file is the one failure
/// a later reader cannot see.
pub const RPC_TRACE_SCHEMA_READABLE: [u64; 2] = [2, RPC_TRACE_SCHEMA];

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
/// A logical call is what the caller asked for; an attempt is what the wire saw. A read
/// retries a transport or HTTP-status failure once, so a run whose node flattered and
/// failed would otherwise look like one slow call; a submission never asks twice. The
/// distinction §25 draws between node latency and a re-made connection rests on this list.
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
    /// The lifecycle stage the issuing code said this call belongs to, or `None` when
    /// nobody stamped one. M8.4.1 §9's `stage`, taken from the same name the latency trace
    /// uses: the stamper is pipeline code holding the trace's own `Stage` enum, so the
    /// word here is that enum's `as_str()` and not a second vocabulary invented for the
    /// evidence (§8's rule).
    ///
    /// Read this together with [`RpcCallEvent::context_note`]: the note is what says
    /// whether the label can be attributed to *this* call alone.
    ///
    /// The three context fields are `skip_serializing_if`-absent rather than `null` when
    /// there is nothing to say, and that is what keeps a version-2 trace line re-emittable
    /// byte for byte: the calls M8.3.2 and M8.3.3 committed predate the question, so a
    /// re-assembly of their own evidence must not invent keys their lines never carried.
    /// What a reader loses is a `null` it would have had to ignore anyway — and
    /// [`RpcCallEvent::trace_schema`] still says which side of the boundary a line is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The site that issued the call, in the words of the code that issued it — a phase
    /// name plus the read it is going to make, not a file:line that a later edit could
    /// silently misplace. `None` for an unstamped call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    /// Why `stage` and `caller` are what they are, from the closed set
    /// [`CONTEXT_NOT_STAMPED`], [`CONTEXT_AMBIGUOUS_CONCURRENT_CALLS`],
    /// [`CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL`]. `None` when a label was stamped *and*
    /// nothing else was outstanding at this sink while the call was open, which is the only
    /// case in which a stamped label is a fact about one call rather than about a window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_note: Option<&'static str>,
}

/// What the code issuing a call says it is for: M8.4.1 §4's `stage` and `caller`, as one
/// stamp on [`RpcTraceSink::set_context`].
///
/// `String` rather than `&'static str` because a caller label is assembled at the call
/// site (`"step 3 of 6 — execute"`), and because putting the stage enum in here would make
/// `evm-chain` depend on `evm-metrics` for a word that a sink only ever stores. The names
/// are the trace's own because the *stamper* is pipeline code holding the `Stage` enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RpcCallContext {
    pub stage: String,
    pub caller: String,
}

impl RpcCallContext {
    pub fn new(stage: impl Into<String>, caller: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            caller: caller.into(),
        }
    }
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

/// The closed set of [`RpcCallEvent::context_note`] values, in the order a table lists
/// them. A note is never a fourth value: one of these three strings or no string at all
/// is the whole of what this build says about a label's exactness.
pub const CONTEXT_NOTES: [&str; 3] = [
    CONTEXT_NOT_STAMPED,
    CONTEXT_AMBIGUOUS_CONCURRENT_CALLS,
    CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL,
];

/// No label was on the sink when this call was issued, so `stage` and `caller` are absent
/// as a fact about the *observer*, not about the call. This is the state of every sink in
/// a run that has RPC tracing on and context stamping off, and it is also the state of a
/// sink whose owner never reaches a stamping site.
pub const CONTEXT_NOT_STAMPED: &str = "context_not_stamped";

/// A second call was outstanding at this sink at one of the two instants this call was
/// stamped or recorded, so the label the sink held was shared. The stage and caller are
/// still the ones that were on the sink — they are not stripped — but they describe a
/// window, not this call. M8.4.1 §5's rule that concurrency must not be *assumed* away
/// is what this note exists for: with a batch of independent reads in flight, a stamped
/// label is a weaker claim than a sequential one, and the evidence says which it is.
pub const CONTEXT_AMBIGUOUS_CONCURRENT_CALLS: &str = "context_ambiguous_concurrent_calls";

/// The label on the sink changed between this call's start stamp and its record, so the
/// site that stamps moved on while the request was still open. Same handling as the
/// concurrency note: the label is published with its note rather than quietly replaced by
/// whichever stamp was current last.
pub const CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL: &str = "context_restamped_mid_call";

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
    /// Set in the same `OnceLock` shape as [`Self::endpoint`], for the same reason: a sink
    /// that exists before the chain id does (M8.4.1 §11's `connect_with_trace`, whose whole
    /// point is to observe the call that *learns* the id) cannot be given it at
    /// construction, and a field that could be reassigned would let a later run overwrite
    /// an earlier one's identity.
    chain_id: OnceLock<u64>,
    /// The label the issuing code last stamped, and how many stamps have been put on this
    /// sink. One mutex because the two are one fact — a label and its sequence number —
    /// and a snapshot that took them separately could pair a label with a stamp count that
    /// already moved past it.
    context: Mutex<ContextSlot>,
    /// Logical calls currently outstanding at this sink: incremented before the request
    /// starts, decremented after it is recorded. A count, not a claim about the node's
    /// connection pool — it says that at most this many of *this sink's* calls were open at
    /// once, which is the only concurrency the label's exactness depends on.
    outstanding: AtomicU64,
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
                chain_id: match chain_id {
                    Some(id) => OnceLock::from(id),
                    None => OnceLock::new(),
                },
                context: Mutex::new(ContextSlot::default()),
                outstanding: AtomicU64::new(0),
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
        self.inner.chain_id.get().copied()
    }

    /// Fill in the chain id for a sink that was opened before it was known.
    ///
    /// The first id given wins, exactly as with [`Self::with_endpoint`]: a sink that could
    /// be re-pointed at another chain would report one run's calls under another chain's
    /// number. This exists for `connect_with_trace`, whose whole purpose is to record the
    /// call that *learns* the id — that call is keyed `chain-unknown` by
    /// [`describe_call`], and everything after it is not.
    pub fn note_chain_id(&self, chain_id: u64) {
        let _ = self.inner.chain_id.set(chain_id);
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

    /// Put M8.4.1 §4's label on the next calls this sink records, and return the stamp's
    /// sequence number.
    ///
    /// The label is one for *every* call issued until the next stamp, which is what makes
    /// it a claim about the issuing code rather than about a span of time: the site that
    /// stamps is the site that is about to make the read. A caller that wants a
    /// per-call label therefore stamps, awaits the one read, stamps the next phase — the
    /// sequential shape the simulation's state provider has at concurrency 1. Two returns
    /// above `1` at the same sink is what [`Self::begin_call`] reports as ambiguity.
    pub fn set_context(&self, stage: impl Into<String>, caller: impl Into<String>) -> u64 {
        let mut slot = context_lock(&self.inner);
        slot.current = Some(RpcCallContext::new(stage, caller));
        slot.stamp += 1;
        slot.stamp
    }

    /// Take the label off, for a stretch of calls that genuinely has no issuing phase to
    /// name. Absence is stated here rather than by leaving a stale label on the sink,
    /// because a stale label would attach an earlier phase's name to a later call.
    pub fn clear_context(&self) -> u64 {
        let mut slot = context_lock(&self.inner);
        slot.current = None;
        slot.stamp += 1;
        slot.stamp
    }

    /// The label this sink is holding right now, if any.
    pub fn context(&self) -> Option<RpcCallContext> {
        context_lock(&self.inner).current.clone()
    }

    /// How many logical calls are open at this sink right now.
    pub fn outstanding_calls(&self) -> u64 {
        self.inner.outstanding.load(Ordering::Acquire)
    }

    /// Announce that a call is about to be issued, and snapshot the label it goes out
    /// under.
    ///
    /// The returned guard holds the count for as long as the call is open and decrements it
    /// on drop, so the choke point never has to remember to balance it — including on the
    /// paths that return early. Nothing here asks the node anything (§12): it is one atomic
    /// increment and one clone of the label the sink already holds.
    pub fn begin_call(&self) -> TracedCall {
        let opened = self.inner.outstanding.fetch_add(1, Ordering::AcqRel) + 1;
        let slot = context_lock(&self.inner);
        TracedCall {
            sink: self.clone(),
            context: slot.current.clone(),
            stamp: slot.stamp,
            outstanding_at_start: opened,
        }
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

/// What [`RpcTraceSink::set_context`] holds: the current label and how many labels this
/// sink has been given.
///
/// The count is what lets a call tell "the label is the one I was issued under" from "the
/// label happens to be what somebody stamped later". A sink that was stamped once and never
/// re-stamped while a call was open is the sequential case, and its calls carry no note.
#[derive(Default)]
struct ContextSlot {
    current: Option<RpcCallContext>,
    stamp: u64,
}

/// One call's open handle on the sink, returned by [`RpcTraceSink::begin_call`].
///
/// A guard rather than a pair of calls, because a count that has to be decremented on
/// every path out of a function that has three `await`s and two early returns is a count
/// that eventually stops matching the calls it counts. Dropping it is the whole of
/// releasing the slot; a panicked or returned-early call therefore cannot strand the
/// counter, and the next call's `outstanding_at_start` would be a number about a
/// bookkeeping bug rather than about the node.
pub struct TracedCall {
    sink: RpcTraceSink,
    context: Option<RpcCallContext>,
    stamp: u64,
    outstanding_at_start: u64,
}

impl Drop for TracedCall {
    fn drop(&mut self) {
        self.sink.inner.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TracedCall {
    /// The label to record, and whether it can be called this call's own.
    ///
    /// Read at record time rather than at start time, because the two things that can make
    /// a label inexact — another call opening at the same sink, and the stamp moving — are
    /// both about the interval the call spans, and only its end can see the whole of it.
    ///
    /// The note's precedence is deliberate: *no label* outranks *a shared label*, and a
    /// demonstrated overlap outranks a stamp that moved. Each is stronger evidence about
    /// the same call than the next, and a reader should see the strongest one that applies.
    pub fn label(&self) -> RpcCallLabel {
        let stamp_now = context_lock(&self.sink.inner).stamp;
        let overlapping = self.outstanding_at_start > 1 || self.sink.outstanding_calls() > 1;
        let note = match (self.context.is_some(), overlapping, stamp_now != self.stamp) {
            (false, _, _) => Some(CONTEXT_NOT_STAMPED),
            (true, true, _) => Some(CONTEXT_AMBIGUOUS_CONCURRENT_CALLS),
            (true, false, true) => Some(CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL),
            (true, false, false) => None,
        };
        RpcCallLabel {
            // The label this call was *issued* under, never the one on the sink at its end:
            // the start is what the issuing code said about this request. A call issued
            // with no stamp keeps no stage even if a stamp arrived before it finished, and
            // `CONTEXT_NOT_STAMPED` says so — that later stamp belongs to a later read.
            stage: self.context.as_ref().map(|context| context.stage.clone()),
            caller: self.context.as_ref().map(|context| context.caller.clone()),
            context_note: note,
        }
    }
}

/// The three context fields one event gets from [`TracedCall::label`].
///
/// A small plain type rather than three loose returns because the choke point has to write
/// all three or none: an event with a stage and no note would be a label this build claims
/// is exact without having checked whether two calls share it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RpcCallLabel {
    pub stage: Option<String>,
    pub caller: Option<String>,
    pub context_note: Option<&'static str>,
}

/// The label slot, poison-resistant for the same reason [`events_lock`] is: a panic
/// somewhere else in the process must not turn a *record* into a panic here, and a
/// poisoned slot still holds the label some phase stamped.
fn context_lock(inner: &Inner) -> MutexGuard<'_, ContextSlot> {
    inner.context.lock().unwrap_or_else(PoisonError::into_inner)
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
///
/// Public since M12-B §4.2: the session record now names an endpoint's purpose next to
/// its URL, and the digest has to sit beside both so a reader can tell "these two lines
/// are the same provider" without a second rule for hashing it. One function, one shape —
/// a second implementation is how an evidence file ends up with two ids for one node.
pub fn endpoint_id(url: &str) -> String {
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
                // Built by hand rather than issued through the choke point, so the context
                // fields carry nothing: no stamp was taken, and no note was computed.
                stage: None,
                caller: None,
                context_note: None,
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
            stage: None,
            caller: None,
            context_note: None,
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
            stage: None,
            caller: None,
            context_note: None,
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
            stage: None,
            caller: None,
            context_note: None,
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

    /// M8.4.1 §4's label, in the one case where it is a fact about a single call: nothing
    /// else was open at this sink, and nothing re-stamped while it was. The absence of a
    /// note is the assertion — it is what lets a table say "this read was issued by this
    /// phase" instead of "this read happened around this phase".
    #[test]
    fn a_label_stamped_for_one_open_call_is_that_calls_and_carries_no_note() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-ctx", RpcTraceSource::Live, Some(1));
        sink.set_context("simulation", "step 2 of 6 — swap exact in");
        let call = sink.begin_call();
        let label = call.label();
        assert_eq!(label.stage.as_deref(), Some("simulation"));
        assert_eq!(label.caller.as_deref(), Some("step 2 of 6 — swap exact in"));
        assert_eq!(
            label.context_note, None,
            "one call, one label, nothing to hedge"
        );
        drop(call);
        assert_eq!(
            sink.outstanding_calls(),
            0,
            "the guard balances its own count"
        );
    }

    /// §5's rule in the shape that actually breaks attribution: two calls open at one sink
    /// share the label, so *both* must say so. A note on only the second would let a reader
    /// group the first with a phase it may not belong to — the exact mistake stamped
    /// labels were supposed to remove.
    #[test]
    fn two_calls_open_at_one_sink_both_report_their_label_as_shared() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-ctx", RpcTraceSource::Live, Some(1));
        sink.set_context("simulation", "codes: touched_contracts");
        let first = sink.begin_call();
        sink.set_context("simulation", "code: sender");
        let second = sink.begin_call();

        let first_label = first.label();
        let second_label = second.label();
        assert_eq!(
            first_label.context_note,
            Some(CONTEXT_AMBIGUOUS_CONCURRENT_CALLS)
        );
        assert_eq!(
            second_label.context_note,
            Some(CONTEXT_AMBIGUOUS_CONCURRENT_CALLS)
        );
        // Each keeps the label it was *issued* under, rather than the one current at its
        // end: the start stamp is what the issuing code said about this request.
        assert_eq!(
            first_label.caller.as_deref(),
            Some("codes: touched_contracts")
        );
        assert_eq!(second_label.caller.as_deref(), Some("code: sender"));
        drop((first, second));
        assert_eq!(sink.outstanding_calls(), 0);
    }

    /// The case a time window cannot see and a stamp can: a phase boundary crossed while a
    /// request was still open. The label is published with the note rather than replaced by
    /// whichever stamp was current last, so a reader sees the same disagreement the run had.
    #[test]
    fn a_label_that_moves_while_a_call_is_open_is_reported_as_moved() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-ctx", RpcTraceSource::Live, Some(1));
        sink.set_context("observation", "latest_block");
        let call = sink.begin_call();
        sink.set_context("opportunity_detection", "price_legs: getReserves");
        let label = call.label();
        assert_eq!(label.stage.as_deref(), Some("observation"));
        assert_eq!(
            label.context_note,
            Some(CONTEXT_AMBIGUOUS_RESTAMPED_MID_CALL)
        );
        drop(call);
    }

    /// The sink's own `None` is not the same statement as an unstamped run, and neither is
    /// the same as a stale label left over from a phase that already ended. This test is
    /// the difference between the two: clearing is a stamp of *nothing*, and the note says
    /// no label was on the sink when the call went out.
    #[test]
    fn an_unstamped_or_cleared_sink_labels_nothing_and_says_which() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-ctx", RpcTraceSource::Live, Some(1));
        let never = sink.begin_call().label();
        assert_eq!(
            (never.stage.as_deref(), never.caller.as_deref()),
            (None, None)
        );
        assert_eq!(never.context_note, Some(CONTEXT_NOT_STAMPED));

        sink.set_context("simulation", "header");
        assert!(sink.context().is_some());
        sink.clear_context();
        assert!(sink.context().is_none());
        let cleared = sink.begin_call().label();
        assert_eq!(cleared.context_note, Some(CONTEXT_NOT_STAMPED));
        assert_eq!(
            sink.outstanding_calls(),
            0,
            "a temporary's drop still balances"
        );
    }

    /// A note is a closed set because §17's tables group by it: an open-ended string would
    /// let the same ambiguity arrive spelled two ways and be counted twice.
    #[test]
    fn the_context_note_is_one_of_three_words_or_none() {
        let sink = RpcTraceSink::new(Instant::now(), "sim-ctx", RpcTraceSource::Live, Some(1));
        let notes = [sink.begin_call().label().context_note, {
            sink.set_context("simulation", "account: sender");
            let call = sink.begin_call();
            let note = call.label().context_note;
            drop(call);
            note
        }];
        for note in notes.into_iter().flatten() {
            assert!(
                CONTEXT_NOTES.contains(&note),
                "`{note}` is not one of the notes this build emits"
            );
        }
        assert_eq!(CONTEXT_NOTES.len(), 3);
    }

    /// §11's `connect_with_trace` precondition, stated where the type lives: a sink can be
    /// opened before the chain id exists and filled in after, and the first id given is the
    /// one it keeps — a sink that could be re-pointed would report one run's calls under
    /// another chain's number.
    #[test]
    fn a_sink_opened_before_the_chain_id_is_known_can_be_filled_in_once() {
        let sink = RpcTraceSink::new(Instant::now(), "lifecycle", RpcTraceSource::Live, None);
        assert_eq!(sink.chain_id(), None);
        sink.note_chain_id(91_342);
        assert_eq!(sink.chain_id(), Some(91_342));
        sink.note_chain_id(1);
        assert_eq!(sink.chain_id(), Some(91_342), "the first id wins");

        let from_construction =
            RpcTraceSink::new(Instant::now(), "sim", RpcTraceSource::Live, Some(1));
        from_construction.note_chain_id(91_342);
        assert_eq!(from_construction.chain_id(), Some(1));
    }
}
