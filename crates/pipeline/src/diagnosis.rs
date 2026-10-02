//! M8.2 §9–§16: what a simulation's recorded RPC calls say about where its time went.
//!
//! The chain crate records calls ([`evm_chain::RpcTraceSink`]); this module only reads
//! those records back and does arithmetic on them. It decides nothing about the run and
//! optimizes nothing — §2's whole list of prohibitions is a description of this file's
//! boundaries. The one question it exists to answer is §1's: a simulation stage that
//! took 23.9 s spent that time doing *what*, and how much of it is a node being asked
//! for state one word at a time.
//!
//! ```text
//! input    one simulation's window (ns, monotonic, same origin as the events)
//!          + the RpcCallEvents its adapter recorded
//! output   a timeline (sum / wall / union / overlap / serial wait / non-RPC),
//!          a duplicate tally (§13), and per-method distributions (§14)
//!          — all integers; §30 forbids a float in these figures
//! ```
//!
//! ## Why `union` and not `total − sum`
//!
//! §11 is explicit that `non_rpc = total − rpc_sum` is wrong the moment two calls
//! overlap, and §9 requires the overlap to be *measured* rather than assumed. So every
//! figure here comes out of one sweep over the call intervals ([`scan`]), which reports
//! how much of the simulation's own clock had one call in flight, more than one, or
//! none. The three add up to the wall window, and `non_rpc` is what the union leaves
//! out of the simulation span.
//!
//! ## Why so many `Option`s
//!
//! A simulation that made no node calls — a fixture, or a dump-backed replay, both of
//! which are legitimate runs this repository has — has no first call to subtract a
//! last call from. §9 forbids writing `0` there, because `0` means "measured, and it was
//! instant", which is a different fact. `None` becomes `null` in the evidence, with a
//! reason beside it.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use evm_chain::{RpcCallEvent, RpcTraceSource};
use evm_metrics::baseline::stats;
use evm_metrics::unix_ms;
use evm_simulation::StateReadStats;
use serde_json::{json, Value};

use crate::error::{PipelineError, Result};

/// The schema version of one line of `simulation-traces.jsonl`.
pub const DIAGNOSIS_SCHEMA: u64 = 1;

/// §9's answer for a span no call was measured inside: nothing, not zero.
pub const NOTHING_MEASURED: &str = "no rpc call was recorded inside this simulation's window, \
     so no rpc timing exists to report";

/// §17's honest limit, carried beside every provider figure this module can produce:
/// the record is taken at the one point that sees a whole call, so construction, the
/// node's own work, and decoding are not separable here.
pub const PROVIDER_BREAKDOWN: &str = "provider_total_duration: this build records one call at \
     the point it becomes bytes on the wire, so request construction, the wait on the node, \
     response decoding and state conversion are not separable there (§17's \
     breakdown_unavailable) and are reported as one provider duration";

/// M8.3.1 §12's tally, in the words a reader of one line needs: which lookups were
/// consulted, and therefore what a hit and a miss each mean here.
///
/// The direction matters. A miss counts reads the boundary had to go and ask for; a hit
/// counts answers it already held, which is the same read the sink would have shown as one
/// more call had the boundary not existed. So the two series are comparable with the call
/// list (§7), and neither one of them is a count of calls *plus* answers.
pub const STATE_READS_TALLY_SCOPE: &str = "state_read_cache: a hit is a state read this \
     simulation answered from what it had already read at its own pinned block, so it cost no \
     request; a miss is a read it had to ask for. With reuse off, the account kinds (balance, \
     nonce, and the bytecode an account asks for) are never looked up and so carry neither \
     number — only the kinds this boundary reused before M8.3.1 (code on a direct read, \
     storage) are tallied. Nothing here is derived from the call list; it is counted at the \
     lookup that made it true.";

/// What a simulation that reads a recorded dump has to say about a cache: nothing, because
/// it never asked a node anything. `0` would read as "asked and reused nothing", which is a
/// different fact (§9's rule, applied to §12's fields).
pub const STATE_READS_UNAVAILABLE: &str = "no state-read boundary was attached to this \
     simulation, so there is no cache tally: a recorded dump answers state without a request, \
     and an unobserved source never reached the boundary this reads";

/// One simulation's own span, on the run's monotonic clock.
///
/// `started_ns`/`finished_ns` are the Simulation stage's two stamps as the lifecycle
/// already produced them, so a call event and this window are readings of one origin
/// and subtract cleanly (§7's one clock, and §11's requirement that `non_rpc` come out
/// of a comparison of the two).
#[derive(Clone, Debug)]
pub struct SimulationWindow {
    pub simulation_id: String,
    pub source: RpcTraceSource,
    pub chain_id: Option<u64>,
    pub block_number: Option<u64>,
    /// The provider's own name for what it read, §11-style: the string that says
    /// whether this window's state came from a node or from a recorded dump.
    pub state_source: Option<String>,
    pub started_ns: u64,
    pub finished_ns: u64,
}

impl SimulationWindow {
    pub fn duration_ns(&self) -> u64 {
        self.finished_ns.saturating_sub(self.started_ns)
    }

    /// The window as §15's JSON asks for it.
    pub fn to_json(&self) -> Value {
        json!({
            "diagnosis_schema": DIAGNOSIS_SCHEMA,
            "simulation_id": self.simulation_id,
            "source": self.source.as_str(),
            "chain_id": self.chain_id,
            "block_number": self.block_number,
            "state_source": self.state_source,
            "simulation_duration_ns": self.duration_ns(),
            "started_ns": self.started_ns,
            "finished_ns": self.finished_ns,
        })
    }
}

/// §9's and §11's timeline figures for one simulation, every one of them a measurement
/// rather than a subtraction a reader has to redo.
#[derive(Clone, Debug, Default)]
pub struct RpcTimeline {
    pub total_calls: usize,
    pub successful_calls: usize,
    pub failed_calls: usize,
    /// HTTP tries across all calls: this above `total_calls` is the retry loop having
    /// run, which is §25's E (connection) evidence rather than its B (node) evidence.
    pub total_attempts: usize,
    pub retried_calls: usize,
    pub sum_duration_ns: u64,
    /// `last_end − first_start`, or `None` when there was no call (§9).
    pub wall_duration_ns: Option<u64>,
    /// The part of that window at least one call was in flight.
    pub union_duration_ns: Option<u64>,
    /// `sum − union`: the time two or more calls spent inside each other. Zero here is
    /// the measurement that says the calls were end to end, not an absent figure.
    pub overlap_duration_ns: Option<u64>,
    /// The part of the covered span where *exactly one* call was in flight — the wait a
    /// serial dependency chain imposes, which is the quantity §25's C is about.
    pub serial_wait_duration_ns: Option<u64>,
    /// `wall − union`: inside the call window, the stretches with nothing in flight.
    pub gap_duration_ns: Option<u64>,
    /// `simulation_duration − union`, §11's way. `None` with no calls, per §9.
    pub non_rpc_duration_ns: Option<u64>,
    pub max_concurrency: usize,
    /// How many calls were clipped to the window because a stamp fell outside it.
    pub calls_clipped: usize,
}

impl RpcTimeline {
    /// §9's verdict, in the two words §9 uses, from the two figures it names.
    ///
    /// `null` when there is nothing to judge; `serial` when no two calls were ever in
    /// flight together; `overlap` otherwise. A `mixed` label is deliberately absent:
    /// the counts beside it say how much of the window each was, which is the
    /// information, so a third word would only round it away. A window whose calls all
    /// landed on one instant is `serial` — nothing shared an instant with anything.
    pub fn serial_or_overlap(&self) -> Option<&'static str> {
        match (self.wall_duration_ns, self.overlap_duration_ns) {
            (Some(_), Some(overlap)) => Some(if overlap == 0 { "serial" } else { "overlap" }),
            _ => None,
        }
    }

    /// The timeline as §11 and §15 ask a report to carry it.
    pub fn to_json(&self) -> Value {
        let mut row = json!({
            "total_calls": self.total_calls,
            "successful_calls": self.successful_calls,
            "failed_calls": self.failed_calls,
            "total_attempts": self.total_attempts,
            "retried_calls": self.retried_calls,
            "rpc_sum_duration_ns": self.sum_duration_ns,
            "rpc_wall_duration_ns": self.wall_duration_ns,
            "rpc_union_duration_ns": self.union_duration_ns,
            "rpc_overlap_duration_ns": self.overlap_duration_ns,
            "serial_wait_duration_ns": self.serial_wait_duration_ns,
            "rpc_gap_duration_ns": self.gap_duration_ns,
            "non_rpc_duration_ns": self.non_rpc_duration_ns,
            "max_concurrency": self.max_concurrency,
            "calls_clipped": self.calls_clipped,
            "serial_or_overlap": self.serial_or_overlap(),
            "provider_duration_field": PROVIDER_BREAKDOWN,
        });
        if self.total_calls == 0 {
            // The nulls above need their reason in the same object a reader is looking
            // at, or `null` reads as a missing field rather than as §9's answer.
            if let Some(object) = row.as_object_mut() {
                object.insert("nothing_measured".to_string(), json!(NOTHING_MEASURED));
            }
        }
        row
    }
}

/// §13's duplicate tally: how many state reads asked for something already asked for,
/// with the ratio left as its two terms.
#[derive(Clone, Debug, Default)]
pub struct DuplicateReads {
    /// Calls whose §12 key this build could build. The denominator of the ratio.
    pub total_state_reads: usize,
    pub unique_state_reads: usize,
    /// `total − unique`, which is §12's counted-the-way-it-says figure: one read asked
    /// for three times contributes 2, because the extra asks are the waste.
    pub duplicate_state_reads: usize,
    /// Calls counted everywhere else and excluded from the three numbers above, because
    /// no key was available. Reported so the tally is read as a tally of *keyable* reads
    /// rather than of all reads.
    pub unkeyed_calls: usize,
    /// Repeated reads by method, so a duplicate rate is not attributed to a method that
    /// did not produce it.
    pub by_method: BTreeMap<String, (usize, usize, usize)>,
    /// The most-repeated keys, up to twenty of them, each with how often it was asked
    /// for. This is the list M8.3 reads first: it names the state, not just a count.
    pub most_repeated: Vec<(String, usize)>,
}

impl DuplicateReads {
    pub fn to_json(&self) -> Value {
        let per_method: Vec<Value> = self
            .by_method
            .iter()
            .map(|(method, (total, unique, duplicates))| {
                json!({
                    "method": method,
                    "total_state_reads": total,
                    "unique_state_reads": unique,
                    "duplicate_state_reads": duplicates,
                })
            })
            .collect();
        let repeated: Vec<Value> = self
            .most_repeated
            .iter()
            .map(|(key, count)| json!({ "dedup_key": key, "times_read": count }))
            .collect();
        json!({
            // §13 forbids the float and names the two terms instead; `numerator` and
            // `denominator` are those two terms, spelled so a reader cannot mistake
            // which of the counts is which.
            "duplicate_ratio": {
                "numerator": self.duplicate_state_reads,
                "denominator": self.total_state_reads,
                "notation": "integer terms, no float (§13)",
            },
            "total_state_reads": self.total_state_reads,
            "unique_state_reads": self.unique_state_reads,
            "duplicate_state_reads": self.duplicate_state_reads,
            "unkeyed_calls": self.unkeyed_calls,
            "per_method": per_method,
            "most_repeated": repeated,
        })
    }
}

/// One simulation's §12 tally, as the line that carries it wants it: the four kinds named,
/// each with both of its integers, and the scope sentence that says what a hit cost.
fn cache_json(stats: &StateReadStats) -> Value {
    json!({
        "reuse": stats.reuse,
        "code": { "hits": stats.code.hits, "misses": stats.code.misses },
        "balance": { "hits": stats.balance.hits, "misses": stats.balance.misses },
        "nonce": { "hits": stats.nonce.hits, "misses": stats.nonce.misses },
        "storage": { "hits": stats.storage.hits, "misses": stats.storage.misses },
        "cache_hits": stats.total_hits(),
        "cache_misses": stats.total_misses(),
        "tally_scope": STATE_READS_TALLY_SCOPE,
    })
}

/// §12's tally folded over the simulations one source recorded, kept per kind.
///
/// The fold is a sum of integers and nothing else: no ratio, no mean, no per-kind
/// weighting. A source's cache figures are read beside its call counts, and the two are
/// only worth comparing because both were counted over the same simulations.
#[derive(Clone, Debug, Default)]
pub struct CacheTotals {
    /// Simulations whose boundary was allowed to reuse, and simulations whose boundary
    /// was not — the two arms of §6, counted separately so an A/B run's tables cannot
    /// silently merge them.
    pub simulations_with_reuse: usize,
    pub simulations_without_reuse: usize,
    /// `(hits, misses)` per kind, summed over the simulations that carried a tally.
    code: (usize, usize),
    balance: (usize, usize),
    nonce: (usize, usize),
    storage: (usize, usize),
    /// Simulations recorded with no tally at all (a dump-backed source), reported so the
    /// sums above are known to be over fewer simulations than `simulations`.
    pub simulations_without_tally: usize,
}

impl CacheTotals {
    fn fold(&mut self, stats: &StateReadStats) {
        if stats.reuse {
            self.simulations_with_reuse += 1;
        } else {
            self.simulations_without_reuse += 1;
        }
        self.code = (
            self.code.0 + stats.code.hits,
            self.code.1 + stats.code.misses,
        );
        self.balance = (
            self.balance.0 + stats.balance.hits,
            self.balance.1 + stats.balance.misses,
        );
        self.nonce = (
            self.nonce.0 + stats.nonce.hits,
            self.nonce.1 + stats.nonce.misses,
        );
        self.storage = (
            self.storage.0 + stats.storage.hits,
            self.storage.1 + stats.storage.misses,
        );
    }

    fn hits(&self) -> usize {
        self.code.0 + self.balance.0 + self.nonce.0 + self.storage.0
    }

    fn misses(&self) -> usize {
        self.code.1 + self.balance.1 + self.nonce.1 + self.storage.1
    }

    fn to_json(&self) -> Value {
        json!({
            "simulations_with_reuse": self.simulations_with_reuse,
            "simulations_without_reuse": self.simulations_without_reuse,
            "simulations_without_tally": self.simulations_without_tally,
            "code": { "hits": self.code.0, "misses": self.code.1 },
            "balance": { "hits": self.balance.0, "misses": self.balance.1 },
            "nonce": { "hits": self.nonce.0, "misses": self.nonce.1 },
            "storage": { "hits": self.storage.0, "misses": self.storage.1 },
            "cache_hits": self.hits(),
            "cache_misses": self.misses(),
            "tally_scope": STATE_READS_TALLY_SCOPE,
        })
    }
}

/// §14's per-method row, percentiles and all.
#[derive(Clone, Debug)]
pub struct MethodAggregate {
    pub method: String,
    pub count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub total_attempts: usize,
    pub retried_calls: usize,
    pub total_duration_ns: u64,
    pub min_duration_ns: u64,
    pub max_duration_ns: u64,
    pub durations_ns: Vec<u64>,
    /// `error_class` → how many calls ended that way, so a method that is slow and a
    /// method that is failing are not both reported as "expensive".
    pub error_classes: BTreeMap<String, usize>,
    pub key_note: Option<&'static str>,
    pub dedup_eligible: bool,
}

impl MethodAggregate {
    pub fn to_json(&self) -> Value {
        json!({
            "method": self.method,
            "count": self.count,
            "success_count": self.success_count,
            "failure_count": self.failure_count,
            "total_attempts": self.total_attempts,
            "retried_calls": self.retried_calls,
            "total_duration_ns": self.total_duration_ns,
            "min_duration_ns": self.min_duration_ns,
            "max_duration_ns": self.max_duration_ns,
            // §14's percentiles, from M8.1's own table builder so the sample policy is
            // literally the same code: a rank too few samples cannot support comes back
            // null with `insufficient_sample`, never as an extrapolated number.
            "distribution_ns": stats(&self.durations_ns),
            "error_classes": self.error_classes,
            "dedup_eligible": self.dedup_eligible,
            "key_note": self.key_note,
        })
    }
}

/// Everything one simulation's calls support, in one value.
#[derive(Clone, Debug)]
pub struct SimulationDiagnosis {
    pub window: SimulationWindow,
    pub timeline: RpcTimeline,
    pub duplicates: DuplicateReads,
    pub methods: Vec<MethodAggregate>,
    pub events: Vec<RpcCallEvent>,
    /// M8.3.1 §12's cache tally, read from the state-read boundary this simulation ran
    /// through. `None` when the simulation read no boundary at all — a recorded dump
    /// issues no state requests, so a cache would have nothing to have answered — and the
    /// trace line says which rather than writing a row of zeros.
    pub state_reads: Option<StateReadStats>,
}

impl SimulationDiagnosis {
    /// Analyze one simulation's recorded calls against its own window.
    pub fn new(window: SimulationWindow, events: Vec<RpcCallEvent>) -> Self {
        let timeline = timeline(&window, &events);
        let duplicates = duplicates(&events);
        let methods = methods(&events);
        Self {
            window,
            timeline,
            duplicates,
            methods,
            events,
            state_reads: None,
        }
    }

    /// Attach the boundary's tally. Kept a separate call rather than a fifth argument to
    /// [`Self::new`]: the analysis above is a pure function of the recorded calls, and this
    /// is a fact read off a different object after the run — the two should not look like
    /// one computation.
    pub fn with_state_reads(mut self, state_reads: Option<StateReadStats>) -> Self {
        self.state_reads = state_reads;
        self
    }

    /// §15's per-simulation line, with §8's rebuildable call list attached to it: one
    /// line of the JSONL holds a whole simulation's timeline *and* its calls, so the
    /// timeline does not have to be reassembled from a second file to be checked.
    pub fn to_trace_line(&self, refusals: &[String]) -> Value {
        let mut line = self.window.to_json();
        let calls: Vec<Value> = self
            .events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
            .collect();
        let methods: Vec<Value> = self.methods.iter().map(|row| row.to_json()).collect();
        if let Some(object) = line.as_object_mut() {
            object.insert("rpc".to_string(), self.timeline.to_json());
            object.insert("duplicates".to_string(), self.duplicates.to_json());
            object.insert("methods".to_string(), Value::Array(methods));
            object.insert("call_count".to_string(), json!(calls.len()));
            object.insert("calls".to_string(), Value::Array(calls));
            object.insert(
                "state_read_cache".to_string(),
                match &self.state_reads {
                    Some(stats) => cache_json(stats),
                    None => {
                        json!({ "measurements": Value::Null, "reason": STATE_READS_UNAVAILABLE })
                    }
                },
            );
            object.insert("diagnosis_refusals".to_string(), json!(refusals));
        }
        line
    }

    /// §16's per-method rows for this one simulation.
    pub fn method_rows(&self) -> Vec<Value> {
        self.methods
            .iter()
            .map(|row| {
                let mut value = row.to_json();
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "simulation_id".to_string(),
                        json!(self.window.simulation_id),
                    );
                    object.insert("source".to_string(), json!(self.window.source.as_str()));
                    object.insert("block_number".to_string(), json!(self.window.block_number));
                }
                value
            })
            .collect()
    }
}

/// One call's interval, placed inside the simulation window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u64,
    end: u64,
    /// Whether this span had to be shortened to fit the window, which is reported
    /// rather than absorbed: a call that started before the stage began is a fact about
    /// how the sink was attached, not a duration to be trimmed away silently.
    clipped: bool,
}

fn span_of(window: &SimulationWindow, event: &RpcCallEvent) -> Span {
    // A reversed stamp pair is floored rather than dropped. `RpcCallEvent`'s own
    // duration is already floored at zero; this is the same rule applied to the pair the
    // sweep uses, so one malformed event cannot move a boundary by a negative amount.
    let raw_start = event.started_ns.min(event.finished_ns);
    let raw_end = event.started_ns.max(event.finished_ns);
    let start = raw_start.max(window.started_ns);
    let end = raw_end.min(window.finished_ns);
    Span {
        start,
        // An entirely outside interval collapses onto the nearer edge: it still
        // contributes zero covered time, and collapsing keeps `last_end` inside the
        // window instead of letting a stray stamp widen it.
        end: end.max(start),
        clipped: start != raw_start || end != raw_end,
    }
}

/// §9's and §11's figures, from one sweep over the call intervals.
///
/// The sweep sorts the interval endpoints and walks them with a count of how many calls
/// are in flight, so serial time, overlapped time and idle time are each measured
/// directly rather than derived from one another. All endpoints at one instant are
/// applied as a batch *before* the next segment is measured, which is what makes two
/// calls that merely touch (`0→500`, `500→900`) read as serial: the count between those
/// two instants is one, never two (§27's adjacency case).
///
/// Returns `(union, serial, overlap, max_concurrency)`.
fn scan(spans: &[Span]) -> (u64, u64, u64, usize) {
    let mut points: Vec<(u64, i32)> = Vec::with_capacity(spans.len() * 2);
    for span in spans {
        points.push((span.start, 1));
        points.push((span.end, -1));
    }
    points.sort_unstable();
    let mut union = 0_u64;
    let mut serial = 0_u64;
    let mut overlap = 0_u64;
    let mut in_flight = 0_i32;
    let mut max_in_flight = 0_usize;
    let mut previous: Option<u64> = None;
    let mut index = 0;
    while index < points.len() {
        let at = points[index].0;
        if let Some(before) = previous {
            let width = at.saturating_sub(before);
            match in_flight {
                // Nothing in flight: this is `gap`, and the caller gets it by
                // subtracting the covered measure from the window it asked about.
                0 => {}
                1 => {
                    union = union.saturating_add(width);
                    serial = serial.saturating_add(width);
                }
                n => {
                    union = union.saturating_add(width);
                    // A stretch with two calls in flight is one stretch of covered time
                    // and two stretches of waiting: the difference is what concurrency
                    // saved, which is `overlap` by §9's definition (`sum − union`).
                    overlap =
                        overlap.saturating_add(width.saturating_mul(n.saturating_sub(1) as u64));
                    // Not serial wait — nothing was waiting alone here.
                }
            }
            max_in_flight = max_in_flight.max(in_flight.max(0) as usize);
        }
        while index < points.len() && points[index].0 == at {
            in_flight += points[index].1;
            index += 1;
        }
        previous = Some(at);
    }
    (union, serial, overlap, max_in_flight)
}

/// §9's and §11's timeline for one simulation.
pub fn timeline(window: &SimulationWindow, events: &[RpcCallEvent]) -> RpcTimeline {
    let mut out = RpcTimeline {
        total_calls: events.len(),
        ..Default::default()
    };
    let mut spans: Vec<Span> = Vec::with_capacity(events.len());
    for event in events {
        if event.success {
            out.successful_calls += 1;
        } else {
            out.failed_calls += 1;
        }
        out.total_attempts += event.attempts.len().max(1);
        if event.attempts.len() > 1 {
            out.retried_calls += 1;
        }
        let span = span_of(window, event);
        out.sum_duration_ns = out
            .sum_duration_ns
            .saturating_add(span.end.saturating_sub(span.start));
        out.calls_clipped += usize::from(span.clipped);
        spans.push(span);
    }
    if spans.is_empty() {
        // §9: no calls, so no wall time. `sum` stays a real zero (nothing was summed),
        // and every figure that would need a first or last call is None.
        return out;
    }
    let first = spans.iter().map(|span| span.start).min().unwrap_or(0);
    let last = spans.iter().map(|span| span.end).max().unwrap_or(0);
    let wall = last.saturating_sub(first);
    let (union, serial, overlap, max_concurrency) = scan(&spans);
    out.wall_duration_ns = Some(wall);
    out.union_duration_ns = Some(union);
    out.overlap_duration_ns = Some(overlap);
    out.serial_wait_duration_ns = Some(serial);
    out.gap_duration_ns = Some(wall.saturating_sub(union));
    out.non_rpc_duration_ns = Some(window.duration_ns().saturating_sub(union));
    out.max_concurrency = max_concurrency;
    out
}

/// §12 and §13: the duplicate tally, over the calls a key could be built for.
pub fn duplicates(events: &[RpcCallEvent]) -> DuplicateReads {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out = DuplicateReads::default();
    for event in events {
        match event.dedup_key.as_deref() {
            None => out.unkeyed_calls += 1,
            Some(key) => {
                out.total_state_reads += 1;
                *seen.entry(key).or_insert(0) += 1;
                let entry = out
                    .by_method
                    .entry(event.method.clone())
                    .or_insert((0, 0, 0));
                entry.0 += 1;
                let first_time = seen[key] == 1;
                if first_time {
                    entry.1 += 1;
                } else {
                    entry.2 += 1;
                }
            }
        }
    }
    out.unique_state_reads = seen.len();
    out.duplicate_state_reads = out.total_state_reads.saturating_sub(out.unique_state_reads);
    let mut repeated: Vec<(String, usize)> = seen
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(key, count)| (key.to_string(), count))
        .collect();
    // Most-repeated first, and a key tie broken by the key itself so the list is the
    // same list on every run over the same calls.
    repeated.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    repeated.truncate(20);
    out.most_repeated = repeated;
    out
}

/// §14's per-method rows, ordered by method name.
pub fn methods(events: &[RpcCallEvent]) -> Vec<MethodAggregate> {
    let mut rows: BTreeMap<&str, MethodAggregate> = BTreeMap::new();
    for event in events {
        let row = rows
            .entry(event.method.as_str())
            .or_insert_with(|| MethodAggregate {
                method: event.method.clone(),
                count: 0,
                success_count: 0,
                failure_count: 0,
                total_attempts: 0,
                retried_calls: 0,
                total_duration_ns: 0,
                min_duration_ns: u64::MAX,
                max_duration_ns: 0,
                durations_ns: Vec::new(),
                error_classes: BTreeMap::new(),
                key_note: event.key_note,
                dedup_eligible: event.dedup_key.is_some(),
            });
        row.count += 1;
        if event.success {
            row.success_count += 1;
        } else {
            row.failure_count += 1;
            if let Some(class) = event.error_class {
                *row.error_classes.entry(class.to_string()).or_insert(0) += 1;
            }
        }
        row.total_attempts += event.attempts.len().max(1);
        row.retried_calls += usize::from(event.attempts.len() > 1);
        row.total_duration_ns = row.total_duration_ns.saturating_add(event.duration_ns);
        row.min_duration_ns = row.min_duration_ns.min(event.duration_ns);
        row.max_duration_ns = row.max_duration_ns.max(event.duration_ns);
        row.durations_ns.push(event.duration_ns);
        // One method can hold both kinds of note across a run; the absence of a key is
        // the one a reader has to be told about, so it wins the tie.
        if event.dedup_key.is_none() {
            row.key_note = event.key_note;
            row.dedup_eligible = false;
        }
    }
    rows.into_values()
        .map(|mut row| {
            row.durations_ns.sort_unstable();
            row
        })
        .collect()
}

/// §22's five names. The directory holds one of each per run.
pub const TRACES_FILE: &str = "simulation-traces.jsonl";
pub const SIMULATION_SUMMARY_FILE: &str = "simulation-summary.json";
pub const RPC_SUMMARY_FILE: &str = "rpc-summary.json";
pub const DUPLICATES_FILE: &str = "duplicate-reads.json";
pub const README_FILE: &str = "README.md";

/// How many duration samples one series keeps before it says it stopped counting.
///
/// A cap is here for the same reason the chain-side sink has one: a percentile table
/// should not be able to grow the process. 10 000 calls of one method is already a run
/// whose story is call volume, and a truncated series is marked as one rather than being
/// reported as a distribution over fewer.
const MAX_SAMPLES_PER_SERIES: usize = 10_000;

/// The §22 directory, written as simulations finish rather than all at the end.
///
/// One line of `simulation-traces.jsonl` per simulation, appended the moment that
/// simulation's stage closes — §31's shape: an in-memory event list per simulation,
/// persisted once, never a write per RPC. The three summary files and the README are
/// written whole through a temporary name at [`DiagnosisEvidence::finish`], because a
/// `rpc-summary.json` that stops mid-object reads like evidence about a finished run
/// when it is not.
///
/// Sources are bucketed on the way in and nothing here adds two of them together (§20):
/// a live run's node latency and a fixture's in-process stand-in are not two samples of
/// one quantity.
pub struct DiagnosisEvidence {
    dir: PathBuf,
    traces: File,
    git_revision: String,
    execution_mode: String,
    generated_at_unix_ms: u64,
    buckets: BTreeMap<&'static str, SourceBucket>,
    simulations: usize,
    /// What the writer itself refused, reported in the summaries beside the figures it
    /// applies to rather than dropped.
    refusals: Vec<String>,
}

/// One source's running aggregation.
struct SourceBucket {
    source: &'static str,
    simulations: usize,
    simulation_ids: Vec<String>,
    chain_ids: Vec<u64>,
    /// Simulations whose window held no call at all, counted separately so a series
    /// below is not silently diluted by them.
    simulations_without_calls: usize,
    calls: u64,
    attempts: u64,
    retried_calls: u64,
    successful: u64,
    failed: u64,
    total_duration_ns: u64,
    wall_ns: Vec<u64>,
    union_ns: Vec<u64>,
    sum_ns: Vec<u64>,
    overlap_ns: Vec<u64>,
    serial_ns: Vec<u64>,
    non_rpc_ns: Vec<u64>,
    gap_ns: Vec<u64>,
    calls_per_simulation: Vec<u64>,
    serial_simulations: u64,
    overlap_simulations: u64,
    total_state_reads: u64,
    unique_state_reads: u64,
    duplicate_state_reads: u64,
    unkeyed_calls: u64,
    /// Per simulation, not per source: a ratio averaged across simulations and then
    /// handed to `stats` would be a distribution of means, which is what §40 says is not
    /// a distribution of anything.
    unique_per_simulation: Vec<u64>,
    duplicate_per_simulation: Vec<u64>,
    /// M8.3.1 §12's cache tally, summed over this source's simulations.
    state_reads: CacheTotals,
    method_rows: BTreeMap<String, MethodAggregate>,
    method_samples: BTreeMap<String, (Vec<u64>, bool)>,
}

impl SourceBucket {
    fn new(source: &'static str) -> Self {
        Self {
            source,
            simulations: 0,
            simulation_ids: Vec::new(),
            chain_ids: Vec::new(),
            simulations_without_calls: 0,
            calls: 0,
            attempts: 0,
            retried_calls: 0,
            successful: 0,
            failed: 0,
            total_duration_ns: 0,
            wall_ns: Vec::new(),
            union_ns: Vec::new(),
            sum_ns: Vec::new(),
            overlap_ns: Vec::new(),
            serial_ns: Vec::new(),
            non_rpc_ns: Vec::new(),
            gap_ns: Vec::new(),
            calls_per_simulation: Vec::new(),
            serial_simulations: 0,
            overlap_simulations: 0,
            total_state_reads: 0,
            unique_state_reads: 0,
            duplicate_state_reads: 0,
            unkeyed_calls: 0,
            unique_per_simulation: Vec::new(),
            duplicate_per_simulation: Vec::new(),
            state_reads: CacheTotals::default(),
            method_rows: BTreeMap::new(),
            method_samples: BTreeMap::new(),
        }
    }

    fn record(&mut self, diagnosis: &SimulationDiagnosis) {
        self.simulations += 1;
        self.simulation_ids
            .push(diagnosis.window.simulation_id.clone());
        if let Some(chain_id) = diagnosis.window.chain_id {
            if !self.chain_ids.contains(&chain_id) {
                self.chain_ids.push(chain_id);
            }
        }
        let timeline = &diagnosis.timeline;
        self.calls += timeline.total_calls as u64;
        self.attempts += timeline.total_attempts as u64;
        self.retried_calls += timeline.retried_calls as u64;
        self.successful += timeline.successful_calls as u64;
        self.failed += timeline.failed_calls as u64;
        self.total_duration_ns = timeline
            .sum_duration_ns
            .saturating_add(self.total_duration_ns);
        self.calls_per_simulation.push(timeline.total_calls as u64);
        if timeline.total_calls == 0 {
            self.simulations_without_calls += 1;
        }
        // `Option` in, and a series only ever holds measurements: a simulation with no
        // calls contributes nothing to a percentile list rather than a zero in it (§9).
        let covered = timeline.wall_duration_ns.map(|_| timeline.sum_duration_ns);
        for (series, value) in [
            (&mut self.wall_ns, timeline.wall_duration_ns),
            (&mut self.union_ns, timeline.union_duration_ns),
            (&mut self.sum_ns, covered),
            (&mut self.overlap_ns, timeline.overlap_duration_ns),
            (&mut self.serial_ns, timeline.serial_wait_duration_ns),
            (&mut self.non_rpc_ns, timeline.non_rpc_duration_ns),
            (&mut self.gap_ns, timeline.gap_duration_ns),
        ] {
            if let Some(value) = value {
                push_sample(series, value);
            }
        }
        match timeline.serial_or_overlap() {
            Some("serial") => self.serial_simulations += 1,
            Some("overlap") => self.overlap_simulations += 1,
            _ => {}
        }

        let duplicates = &diagnosis.duplicates;
        self.total_state_reads += duplicates.total_state_reads as u64;
        self.unique_state_reads += duplicates.unique_state_reads as u64;
        self.duplicate_state_reads += duplicates.duplicate_state_reads as u64;
        self.unkeyed_calls += duplicates.unkeyed_calls as u64;
        push_sample(
            &mut self.unique_per_simulation,
            duplicates.unique_state_reads as u64,
        );
        push_sample(
            &mut self.duplicate_per_simulation,
            duplicates.duplicate_state_reads as u64,
        );

        match &diagnosis.state_reads {
            Some(stats) => self.state_reads.fold(stats),
            None => self.state_reads.simulations_without_tally += 1,
        }

        for row in &diagnosis.methods {
            let merged = self
                .method_rows
                .entry(row.method.clone())
                .or_insert_with(|| MethodAggregate {
                    method: row.method.clone(),
                    count: 0,
                    success_count: 0,
                    failure_count: 0,
                    total_attempts: 0,
                    retried_calls: 0,
                    total_duration_ns: 0,
                    min_duration_ns: u64::MAX,
                    max_duration_ns: 0,
                    durations_ns: Vec::new(),
                    error_classes: BTreeMap::new(),
                    key_note: row.key_note,
                    dedup_eligible: row.dedup_eligible,
                });
            merged.count += row.count;
            merged.success_count += row.success_count;
            merged.failure_count += row.failure_count;
            merged.total_attempts += row.total_attempts;
            merged.retried_calls += row.retried_calls;
            merged.total_duration_ns = merged
                .total_duration_ns
                .saturating_add(row.total_duration_ns);
            merged.min_duration_ns = merged.min_duration_ns.min(row.min_duration_ns);
            merged.max_duration_ns = merged.max_duration_ns.max(row.max_duration_ns);
            // A method that produced a keyless call anywhere in this source is not
            // dedup-eligible for the summary that claims it is.
            merged.dedup_eligible &= row.dedup_eligible;
            if row.key_note.is_some() {
                merged.key_note = row.key_note;
            }
            for (class, count) in row.error_classes.clone() {
                *merged.error_classes.entry(class).or_insert(0) += count;
            }
            let (samples, capped) = self
                .method_samples
                .entry(row.method.clone())
                .or_insert_with(|| (Vec::new(), false));
            for sample in &row.durations_ns {
                if samples.len() >= MAX_SAMPLES_PER_SERIES {
                    *capped = true;
                    break;
                }
                samples.push(*sample);
            }
        }
    }

    fn call_series(&self) -> Value {
        json!({
            "source": self.source,
            "simulations": self.simulations,
            "simulations_without_calls": self.simulations_without_calls,
            "total_calls": self.calls,
            "successful_calls": self.successful,
            "failed_calls": self.failed,
            "total_attempts": self.attempts,
            "retried_calls": self.retried_calls,
            "calls_per_simulation": {
                // §40 wants a distribution where a mean was asked for, so the mean is
                // given as its integer part and the series is next to it.
                "integer_mean": integer_mean(self.calls, self.simulations),
                "distribution": count_stats(&self.calls_per_simulation),
            },
            "per_simulation_wall_ns": stats(&self.wall_ns),
            "per_simulation_sum_ns": stats(&self.sum_ns),
            "per_simulation_union_ns": stats(&self.union_ns),
            "per_simulation_overlap_ns": stats(&self.overlap_ns),
            "per_simulation_serial_wait_ns": stats(&self.serial_ns),
            "per_simulation_gap_ns": stats(&self.gap_ns),
            "per_simulation_non_rpc_ns": stats(&self.non_rpc_ns),
            "serial_simulations": self.serial_simulations,
            "overlap_simulations": self.overlap_simulations,
        })
    }

    fn method_series(&self) -> Vec<Value> {
        self.method_rows
            .values()
            .map(|row| {
                let (samples, capped) = self
                    .method_samples
                    .get(&row.method)
                    .cloned()
                    .unwrap_or_default();
                let mut value = json!({
                    "source": self.source,
                    "method": row.method,
                    "count": row.count,
                    "success_count": row.success_count,
                    "failure_count": row.failure_count,
                    "total_attempts": row.total_attempts,
                    "retried_calls": row.retried_calls,
                    "total_duration_ns": row.total_duration_ns,
                    "min_duration_ns": row.min_duration_ns,
                    "max_duration_ns": row.max_duration_ns,
                    "distribution_ns": stats(&samples),
                    "error_classes": row.error_classes,
                    "dedup_eligible": row.dedup_eligible,
                    "key_note": row.key_note,
                });
                if capped {
                    if let Some(object) = value.as_object_mut() {
                        object.insert("samples_capped".to_string(), json!(MAX_SAMPLES_PER_SERIES));
                    }
                }
                value
            })
            .collect()
    }

    fn duplicate_series(&self) -> Value {
        json!({
            "source": self.source,
            "simulations": self.simulations,
            "total_state_reads": self.total_state_reads,
            "unique_state_reads": self.unique_state_reads,
            "duplicate_state_reads": self.duplicate_state_reads,
            "unkeyed_calls": self.unkeyed_calls,
            "duplicate_ratio": {
                "numerator": self.duplicate_state_reads,
                "denominator": self.total_state_reads,
                "notation": "integer terms, no float (§13)",
            },
            "per_simulation_unique_reads": count_stats(&self.unique_per_simulation),
            "per_simulation_duplicate_reads": count_stats(&self.duplicate_per_simulation),
            // §12's own numbers, beside §13's: the duplicate tally says how many asks were
            // repeats, this says how many of them the run did not make.
            "state_read_cache": self.state_reads.to_json(),
        })
    }
}

/// A mean as its integer part, because §30 and §13 together leave no float here.
fn integer_mean(total: u64, count: usize) -> Option<u64> {
    if count == 0 {
        return None;
    }
    Some(total / count as u64)
}

/// `stats` over a population of *counts*.
///
/// The sample policy is M8.1's function unchanged — same ranks, same
/// `insufficient_sample` nulls — so a percentile here is not a second, weaker rule. But
/// `stats` spells its keys `min_ns`/`p90_ns` and says `unit: "ns"`, and 41 duplicate reads
/// are not 41 nanoseconds: a reader who summed that column into the duration table beside
/// it would get a number that means nothing. The suffix is stripped and the unit corrected
/// here, in the one place both series are built.
/// Made public because §19's assembler (`crates/pipeline/tests/reuse_ab_evidence.rs`) counts
/// the same populations over the recorded runs: a second relabelling of `stats` would be a
/// second rule about what a count is not.
pub fn count_stats(samples: &[u64]) -> Value {
    let mut row = serde_json::Map::new();
    for (key, value) in stats(samples).as_object().into_iter().flatten() {
        if key == "unit" {
            row.insert("unit".to_string(), json!("count"));
        } else {
            row.insert(
                key.strip_suffix("_ns").unwrap_or(key).to_string(),
                value.clone(),
            );
        }
    }
    Value::Object(row)
}

/// Push a sample, keeping the series bounded.
fn push_sample(series: &mut Vec<u64>, value: u64) {
    if series.len() < MAX_SAMPLES_PER_SERIES {
        series.push(value);
    }
}

impl DiagnosisEvidence {
    /// Open `dir` for one run's diagnosis.
    pub fn open(dir: &Path, git_revision: &str, execution_mode: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|error| PipelineError::Evidence {
            path: dir.to_path_buf(),
            detail: format!("the diagnosis directory could not be created: {error}"),
        })?;
        let path = dir.join(TRACES_FILE);
        let traces = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| PipelineError::Evidence {
                path,
                detail: format!("the diagnosis traces could not be opened for appending: {error}"),
            })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            traces,
            git_revision: git_revision.to_string(),
            execution_mode: execution_mode.to_string(),
            // The one wall-clock reading in these files, used for nothing but its own
            // timestamp: every duration here is a monotonic difference (§7).
            generated_at_unix_ms: unix_ms(),
            buckets: BTreeMap::new(),
            simulations: 0,
            refusals: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one finished simulation and fold it into the per-source tables.
    ///
    /// Called once, at the end of the simulation whose calls it holds — the point §31
    /// names. It touches no provider, so it cannot issue a request (§18).
    pub fn record(&mut self, diagnosis: SimulationDiagnosis, refusals: &[String]) -> Result<()> {
        let line = diagnosis.to_trace_line(refusals);
        let key = source_key(diagnosis.window.source);
        let bucket = self
            .buckets
            .entry(key)
            .or_insert_with(|| SourceBucket::new(key));
        bucket.record(&diagnosis);
        self.simulations += 1;
        let text = serde_json::to_string(&line).map_err(|error| PipelineError::Evidence {
            path: self.dir.join(TRACES_FILE),
            detail: format!(
                "simulation {} does not serialize: {error}",
                diagnosis.window.simulation_id
            ),
        })?;
        self.traces
            .write_all(format!("{text}\n").as_bytes())
            .and_then(|_| self.traces.flush())
            .map_err(|error| write_failed(TRACES_FILE, error))?;
        Ok(())
    }

    /// Note a refusal of the writer's own — a source that could not be traced, a
    /// simulation whose sink came back empty because the adapter was not derivable.
    pub fn refuse(&mut self, note: String) {
        self.refusals.push(note);
    }

    /// Write the summaries and the README. Returns the directory, which is what the
    /// run's own record names so a reader can find the traces.
    pub fn finish(&mut self) -> Result<PathBuf> {
        let metadata = json!({
            "diagnosis_schema": DIAGNOSIS_SCHEMA,
            "git_revision": self.git_revision,
            "execution_mode": self.execution_mode,
            "generated_at_unix_ms": self.generated_at_unix_ms,
            "generated_at_used_for_durations": false,
            "unit": "ns",
            "clock": "one monotonic origin per run, shared with the latency trace (§7)",
            "percentile_algorithm": "nearest_rank",
            "minimum_samples_for_rank": {
                "p50": evm_metrics::minimum_samples_for(50),
                "p90": evm_metrics::minimum_samples_for(90),
                "p95": evm_metrics::minimum_samples_for(95),
                "p99": evm_metrics::minimum_samples_for(99),
            },
            "sources_are_never_blended": true,
            "instrumentation_issues_no_requests": true,
            "provider_duration_field": PROVIDER_BREAKDOWN,
        });
        let sources: Vec<Value> = self
            .buckets
            .values()
            .map(SourceBucket::call_series)
            .collect();
        let methods: Vec<Value> = self
            .buckets
            .values()
            .flat_map(SourceBucket::method_series)
            .collect();

        let mut simulation_summary = metadata.clone();
        if let Some(object) = simulation_summary.as_object_mut() {
            object.insert("simulations".to_string(), json!(self.simulations));
            object.insert(
                "per_source".to_string(),
                json!(self
                    .buckets
                    .values()
                    .map(|bucket| {
                        json!({
                            "source": bucket.source,
                            "simulations": bucket.simulations,
                            "simulation_ids": bucket.simulation_ids,
                            "chain_ids": bucket.chain_ids,
                            "simulations_without_calls": bucket.simulations_without_calls,
                            "total_rpc_duration_ns": bucket.total_duration_ns,
                            "total_calls": bucket.calls,
                        })
                    })
                    .collect::<Vec<_>>()),
            );
            object.insert("diagnosis_refusals".to_string(), json!(self.refusals));
        }
        self.write_whole(SIMULATION_SUMMARY_FILE, &simulation_summary)?;

        let mut rpc_summary = metadata.clone();
        if let Some(object) = rpc_summary.as_object_mut() {
            object.insert("simulations".to_string(), json!(self.simulations));
            object.insert("per_source".to_string(), Value::Array(sources.clone()));
            object.insert("per_method".to_string(), Value::Array(methods));
            object.insert("diagnosis_refusals".to_string(), json!(self.refusals));
        }
        self.write_whole(RPC_SUMMARY_FILE, &rpc_summary)?;

        let mut duplicate_summary = metadata.clone();
        if let Some(object) = duplicate_summary.as_object_mut() {
            object.insert(
                "per_source".to_string(),
                json!(self
                    .buckets
                    .values()
                    .map(SourceBucket::duplicate_series)
                    .collect::<Vec<_>>()),
            );
        }
        self.write_whole(DUPLICATES_FILE, &duplicate_summary)?;

        std::fs::write(
            self.dir.join(README_FILE),
            readme(&sources, &rpc_summary, self.simulations),
        )
        .map_err(|error| write_failed(README_FILE, error))?;
        Ok(self.dir.clone())
    }

    fn write_whole(&mut self, name: &str, value: &Value) -> Result<()> {
        let text =
            serde_json::to_string_pretty(value).map_err(|error| PipelineError::Evidence {
                path: self.dir.join(name),
                detail: format!("the diagnosis summary does not serialize: {error}"),
            })?;
        let temp = self.dir.join(format!("{name}.part"));
        std::fs::write(&temp, format!("{text}\n")).map_err(|error| PipelineError::Evidence {
            path: temp.clone(),
            detail: format!("{error}"),
        })?;
        std::fs::rename(&temp, self.dir.join(name)).map_err(|error| PipelineError::Evidence {
            path: self.dir.join(name),
            detail: format!("the completed file could not take the temporary one's place: {error}"),
        })
    }

    pub fn simulations_recorded(&self) -> usize {
        self.simulations
    }
}

/// The bucket a source's figures go into, as the two words §20 uses.
fn source_key(source: RpcTraceSource) -> &'static str {
    source.as_str()
}

/// §23's README: what the numbers are, how they were taken, and what they cannot say.
fn readme(sources: &[Value], summary: &Value, simulations: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# M8.2 simulation state acquisition diagnosis\n\n\
         {simulations} simulation(s) are recorded here — one line of `{TRACES_FILE}` each, with the\n\
         per-method and per-source tables in `{RPC_SUMMARY_FILE}`, `{SIMULATION_SUMMARY_FILE}` and\n\
         `{DUPLICATES_FILE}`. Nothing here changed what the run decided: the difference between a\n\
         traced run and an untraced one is that a call which was already about to happen got two\n\
         clock readings and one `Vec` push around it.\n\n\
         What the run was configured to do about the repeats it shows is a separate question, and\n\
         it is answered per run rather than per directory: a build from M8.3.1 onward may reuse a\n\
         state read it already made at the same pinned block, so the same line can describe either\n\
         arm. Each line's `state_read_cache.reuse` field says which one it was. `{DUPLICATES_FILE}`\n\
         counts how many simulations ran with reuse on and off, and sums the tallies over both — so\n\
         an A/B comparison is two runs into two directories, never one directory with both arms in\n\
         it.\n"
    ));
    lines.push(
        "## Data source\n\n\
         Each line names its own `source`: `live` (a node answering over the network), \
         `replay` (recorded state read from disk), `fixture` (a deterministic in-process \
         stand-in). The tables are built per source and no figure here adds two of them \
         together. A fixture's numbers demonstrate the machinery; they are not market \
         latency.\n\n\
         The bucket key is `sources_are_never_blended: true` in each summary file.\n"
            .to_string(),
    );
    lines.push(format!(
        "## Sample policy\n\n\
         Percentiles are nearest-rank with M8.1's minimum sample counts: p50 needs {}, p90 {},\n\
         p95 {}, p99 {}. A rank too few samples cannot support is `null` with\n\
         `\"reason\": \"insufficient_sample\"` and its `minimum_samples` beside it, never an\n\
         extrapolated number. A ratio is two integers — a numerator and a denominator — and\n\
         never a float (§13). `generated_at_unix_ms` is a wall-clock reading and is used for\n\
         nothing but the file's own timestamp: no duration in these files is computed from it.\n",
        evm_metrics::minimum_samples_for(50),
        evm_metrics::minimum_samples_for(90),
        evm_metrics::minimum_samples_for(95),
        evm_metrics::minimum_samples_for(99),
    ));
    lines.push(format!(
        "## RPC instrumentation methodology\n\n\
         A call is recorded at the one place this repository turns a method name and a params\n\
         array into bytes on the wire (`HttpChainAdapter::request_with`). So:\n\n\
         - `method` is the literal string that went out, not a name inferred from a caller.\n\
         - `duration_ns` is the whole logical call: construction, the POST, the node's own\n\
           work, the response body and its decode. §17's sub-split is not separable at this\n\
           point and is recorded as `breakdown_unavailable`; the same sentence is carried in\n\
           every summary as `provider_duration_field` so it travels with the numbers it\n\
           qualifies, not only in prose.\n\
         - `attempts[]` holds one entry per HTTP try, so a call that burned the client's 20 s\n\
           timeout and then retried is distinguishable from one slow answer — which is what\n\
           lets a reader tell §25's E (connection) from its B (node) at all.\n\
         - Every stamp is nanoseconds since this run's own monotonic origin, the same origin\n\
           the M8.1 latency trace reads, so an event and a stage span subtract cleanly.\n\
         - **The instrumentation issues no requests.** It wraps calls the run was already\n\
           going to make (§18); it never asks for a block, a balance or a storage word in\n\
           order to complete a record.\n\
         - A `trace` flag that is off leaves the request path as it was, including the single\n\
           retry and the order of calls (§19).\n\n\
         `{}`\n",
        summary["provider_duration_field"]
            .as_str()
            .unwrap_or(PROVIDER_BREAKDOWN)
    ));
    lines.push(
        "## Duplicate detection methodology\n\n\
         A state read's identity is §12's tuple for its method, built from the params already\n\
         in hand: storage by (chain, block, address, slot); balance, code and transaction\n\
         count by (chain, block, address); eth_call by (chain, block, to, data) plus a value\n\
         term when the request carried one; blocks by (chain, block-or-hash, hydrated). Hex\n\
         heights normalize to decimal and a tag (`latest`, `pending`, `finalized`, `safe`) is\n\
         kept as the tag, so a numbered read and a tagged read never key alike. One word\n\
         written two ways — `0x8` and a zero-padded 64-digit slot, or an address in either\n\
         checksum case — is one key. A method with no rule here, or params that do not match\n\
         the rule, is counted as a call and reported under `unkeyed_calls` with the `key_note`\n\
         naming which of the two it was; it is not handed a borrowed key and not dropped.\n\n\
         `duplicate_state_reads` counts repeats, not asks: a read made three times adds 2, so\n\
         the figure names the avoidable asks rather than the total ones.\n\n\
         `state_read_cache` is a second, independent account of the same subject, counted at the\n\
         boundary the simulation reads through rather than at the wire: its hits are the asks\n\
         that never became calls. The two are meant to be checked against each other (§7), which\n\
         is why both are here and why neither is derived from the other.\n"
            .to_string(),
    );
    lines.push(
        "## Serial / overlap methodology\n\n\
         One sweep over the call intervals measures, per simulation, how much of the\n\
         simulation's own window had exactly one call in flight\n\
         (`serial_wait_duration_ns`), how much had two or more (`rpc_overlap_duration_ns`,\n\
         which is `sum − union`), and how much had none (`rpc_gap_duration_ns`).\n\
         `rpc_wall_duration_ns` is last end minus first start; `non_rpc_duration_ns` is the\n\
         simulation's span minus the *covered* measure — not span minus sum, which is the\n\
         mistake §11 calls out once anything overlaps. Intervals that merely touch count as\n\
         serial, not as overlapping. A simulation that made no calls reports `null` for every\n\
         one of these, never 0: `0` would say the RPC part was instant, which is a different\n\
         fact (§9).\n"
            .to_string(),
    );
    lines.push(
        "## Missing data\n\n\
         `null` means nothing was measured, and the field beside it says why. `0` means a\n\
         measurement landed on zero. A series with one sample carries a `note` saying it is\n\
         one measurement and not a distribution. A per-method sample list that reached 10 000\n\
         entries reports `samples_capped`. `calls_clipped` counts calls whose stamps fell\n\
         outside the simulation's own window and were clipped to it; `simulations_without_calls`\n\
         counts simulations that ran, were traced, and asked the node for nothing.\n"
            .to_string(),
    );
    let mut limitations = String::from("## Known limitations\n\n");
    for source in sources {
        let name = source["source"].as_str().unwrap_or("?");
        let serial = source["serial_simulations"].as_u64().unwrap_or(0);
        let overlap = source["overlap_simulations"].as_u64().unwrap_or(0);
        let without = source["simulations_without_calls"].as_u64().unwrap_or(0);
        limitations.push_str(&format!(
            "- `{name}`: {serial} simulation(s) measured fully serial, {overlap} with any \
             overlap, {without} with no recorded call inside the window. A source with one \
             simulation cannot support p90 or above, and says so rather than estimating.\n"
        ));
    }
    limitations.push_str(
        "- The record is taken at the wire, so a slow decode and a slow node are one number \
           (§17). What separates them here is the attempt list, not a sub-timer.\n\
         - Detection-stage reads (the reserves priced before a simulation is built) are not in\n\
           these traces: the sink is attached to the adapter the simulation itself reads \
           through, so a call in this directory is a call that simulation made.\n\
         - Submission, receipt and header reads on other transports — the WebSocket client's \
           own request path, and everything `crates/execution` sends through its submitter — \
           are outside what this adapter sees, and are named here rather than counted as zero.\n\
         - A source whose adapter cannot hand out a traced clone reports no sink at all. That \
           is recorded as `diagnosis_refusals`, not as a simulation with zero calls.\n",
    );
    lines.push(limitations);
    lines.push(String::from(
        "## What this directory is not\n\n\
         It is not a result. It records calls, and records them the same way whichever arm the\n\
         run was configured for.\n\n\
         Two of §26's candidates were later built; the rest were not. State read reuse (M8.3.1)\n\
         exists and is switched per run by `--no-state-read-reuse`, so a duplicate that still\n\
         appears in a line here is a duplicate that arm was left in place to measure — read\n\
         `state_read_cache.reuse` before concluding anything from a repeat. RPC batching, reading\n\
         the two venues concurrently, request prefetch and connection tuning are still not\n\
         implemented, and no code in this build does any of them: a simulation's calls stay one\n\
         per state read, in the order the EVM asked for them.\n",
    ));
    lines.join("\n")
}

/// The one error shape the writers need.
fn write_failed(name: &str, error: std::io::Error) -> PipelineError {
    PipelineError::Evidence {
        path: PathBuf::from(name),
        detail: format!("{error}"),
    }
}

/// A call that made it to the wire and came back, for the tests below — and for
/// `arbitrage.rs`'s, which needs an event list without owning a network.
#[cfg(test)]
pub(crate) fn call(
    rpc_id: u64,
    method: &str,
    started_ns: u64,
    finished_ns: u64,
    dedup_key: Option<&str>,
) -> RpcCallEvent {
    RpcCallEvent {
        trace_schema: evm_chain::RPC_TRACE_SCHEMA,
        rpc_id,
        method: method.to_string(),
        block: Some("37594591".to_string()),
        target: None,
        started_ns,
        finished_ns,
        duration_ns: finished_ns.saturating_sub(started_ns),
        success: true,
        error_class: None,
        error_detail: None,
        attempts: vec![evm_chain::RpcAttempt {
            started_ns,
            finished_ns,
            duration_ns: finished_ns.saturating_sub(started_ns),
            outcome: evm_chain::CLASS_OK,
        }],
        dedup_key: dedup_key.map(str::to_string),
        key_note: dedup_key.map_or(Some(evm_chain::DEDUP_KEY_UNAVAILABLE_FOR_METHOD), |_| None),
    }
}

#[cfg(test)]
pub(crate) fn window(started_ns: u64, finished_ns: u64) -> SimulationWindow {
    SimulationWindow {
        simulation_id: "sim-under-test".to_string(),
        source: RpcTraceSource::Fixture,
        chain_id: Some(91_342),
        block_number: Some(37_594_591),
        state_source: Some("fixture".to_string()),
        started_ns,
        finished_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm_simulation::ReuseTally;

    /// §9's four figures, on §11's own worked example: three calls over a 1000 ns span,
    /// two of them overlapping. The example's point is that `non_rpc` is 100 and *not*
    /// `1000 − (500+500+200)`, so the arithmetic this function does has to land there.
    #[test]
    fn the_worked_example_in_the_spec_computes_to_the_numbers_in_the_spec() {
        let window = window(0, 1_000);
        let events = vec![
            call(1, "eth_getStorageAt", 0, 500, Some("storage|a")),
            call(2, "eth_getStorageAt", 200, 700, Some("storage|b")),
            call(3, "eth_getStorageAt", 800, 1_000, Some("storage|c")),
        ];
        let seen = timeline(&window, &events);
        assert_eq!(seen.sum_duration_ns, 500 + 500 + 200);
        assert_eq!(seen.wall_duration_ns, Some(1_000));
        assert_eq!(seen.union_duration_ns, Some(900), "0→700 and 800→1000");
        assert_eq!(seen.overlap_duration_ns, Some(300), "sum minus union");
        assert_eq!(seen.non_rpc_duration_ns, Some(100));
        assert_eq!(seen.gap_duration_ns, Some(100), "the 700→800 idle stretch");
        assert_eq!(
            seen.serial_wait_duration_ns,
            Some(600),
            "1→1 in flight: 0→200, 500→700, 800→1000... \
                   which is 200+200+200"
        );
        assert_eq!(seen.max_concurrency, 2);
        assert_eq!(seen.serial_or_overlap(), Some("overlap"));
    }

    /// §9's `serial`, measured rather than assumed: touching intervals are not an
    /// overlap, because nothing was ever in flight twice.
    #[test]
    fn adjacent_calls_are_serial_and_not_a_one_instant_overlap() {
        let window = window(0, 1_000);
        let events = vec![
            call(1, "eth_getCode", 0, 500, Some("code|a")),
            call(2, "eth_getCode", 500, 900, Some("code|b")),
        ];
        let seen = timeline(&window, &events);
        assert_eq!(seen.overlap_duration_ns, Some(0));
        assert_eq!(seen.union_duration_ns, Some(900));
        assert_eq!(seen.serial_wait_duration_ns, Some(900));
        assert_eq!(seen.gap_duration_ns, Some(0));
        assert_eq!(seen.non_rpc_duration_ns, Some(100));
        assert_eq!(seen.max_concurrency, 1);
        assert_eq!(seen.serial_or_overlap(), Some("serial"));
    }

    /// §9: with no calls the wall figure is `null`, and a zero here would be a claim
    /// that the RPC part was instant.
    #[test]
    fn a_simulation_that_made_no_calls_has_no_wall_time_rather_than_zero_wall_time() {
        let window = window(0, 5_000);
        let seen = timeline(&window, &[]);
        assert_eq!(seen.total_calls, 0);
        assert_eq!(
            seen.sum_duration_ns, 0,
            "a sum over nothing is genuinely zero"
        );
        assert_eq!(seen.wall_duration_ns, None);
        assert_eq!(seen.union_duration_ns, None);
        assert_eq!(seen.overlap_duration_ns, None);
        assert_eq!(seen.serial_wait_duration_ns, None);
        assert_eq!(seen.non_rpc_duration_ns, None);
        assert_eq!(seen.serial_or_overlap(), None);

        let line = SimulationDiagnosis::new(window, Vec::new()).to_trace_line(&[]);
        assert_eq!(line["rpc"]["rpc_wall_duration_ns"], Value::Null);
        assert!(line["rpc"]
            .get("provider_duration_field")
            .is_some_and(Value::is_string));
    }

    /// A call before the window opens or after it closes is clipped, counted, and does
    /// not widen the measured span — a stray stamp must not be able to invent coverage.
    #[test]
    fn a_call_outside_the_window_is_clipped_and_said_so() {
        let window = window(1_000, 2_000);
        let events = vec![
            call(1, "eth_getBalance", 0, 500, Some("balance|a")),
            call(2, "eth_getBalance", 1_200, 1_400, Some("balance|b")),
        ];
        let seen = timeline(&window, &events);
        assert_eq!(
            seen.calls_clipped, 1,
            "the first call collapsed onto the window edge"
        );
        assert_eq!(seen.wall_duration_ns, Some(400), "the edge plus 1200→1400");
        assert_eq!(seen.union_duration_ns, Some(200));
        assert_eq!(seen.non_rpc_duration_ns, Some(800));
    }

    /// §27's non-negative rule, at the one place a reversed pair could reach it.
    #[test]
    fn a_reversed_pair_of_stamps_cannot_borrow_time_from_the_window() {
        let window = window(0, 1_000);
        let events = vec![call(1, "eth_call", 900, 100, Some("call|a"))];
        let seen = timeline(&window, &events);
        assert_eq!(seen.sum_duration_ns, 800);
        assert_eq!(seen.overlap_duration_ns, Some(0));
        assert_eq!(seen.serial_wait_duration_ns, Some(800));
    }

    /// §13's three counts, and §12's example read exactly: one read asked for three
    /// times is two duplicates, not three.
    #[test]
    fn a_read_asked_for_three_times_counts_as_two_duplicates() {
        let events = vec![
            call(1, "eth_getStorageAt", 0, 10, Some("storage|same")),
            call(2, "eth_getStorageAt", 10, 20, Some("storage|same")),
            call(3, "eth_getStorageAt", 20, 30, Some("storage|same")),
            call(4, "eth_getStorageAt", 30, 40, Some("storage|other")),
        ];
        let seen = duplicates(&events);
        assert_eq!(seen.total_state_reads, 4);
        assert_eq!(seen.unique_state_reads, 2);
        assert_eq!(seen.duplicate_state_reads, 2);
        assert_eq!(seen.unkeyed_calls, 0);
        assert_eq!(seen.most_repeated, vec![("storage|same".to_string(), 3)]);

        let json = seen.to_json();
        assert_eq!(json["duplicate_ratio"]["numerator"], 2);
        assert_eq!(json["duplicate_ratio"]["denominator"], 4);
        // §13's ban is on the float, so the check is on the type: an integer-valued
        // number that survives a round trip through `f64` unchanged is not evidence of
        // anything, and `is_i64` is.
        assert!(json["duplicate_ratio"]["numerator"].is_i64());
        assert!(json["duplicate_ratio"]["denominator"].is_i64());
    }

    /// An unkeyable call is counted as a call and kept out of the ratio, rather than
    /// being handed a key or dropping out of the evidence entirely.
    #[test]
    fn a_call_with_no_key_is_counted_and_excluded_from_the_tally() {
        let events = vec![
            call(1, "eth_chainId", 0, 10, None),
            call(2, "eth_getCode", 10, 20, Some("code|a")),
        ];
        let seen = duplicates(&events);
        assert_eq!(seen.unkeyed_calls, 1);
        assert_eq!(seen.total_state_reads, 1);
        assert_eq!(seen.duplicate_state_reads, 0);
    }

    /// §27's duplicate matrix, read from the tally side. `rpc_trace.rs` proves the four
    /// §12 keys differ; this proves the tally *uses* that difference, so a simulation that
    /// read four different slots is reported as four distinct reads and not as three
    /// duplicates. A key that collapsed on any one dimension would invent exactly the
    /// bottleneck M8.3 would then be sent to fix.
    #[test]
    fn a_read_that_differs_by_slot_block_address_or_chain_is_not_a_duplicate() {
        let events = vec![
            call(
                1,
                "eth_getStorageAt",
                0,
                10,
                Some("storage|91342|100|0xabc|slot8"),
            ),
            call(
                2,
                "eth_getStorageAt",
                10,
                20,
                Some("storage|91342|100|0xabc|slot9"),
            ),
            call(
                3,
                "eth_getStorageAt",
                20,
                30,
                Some("storage|91342|101|0xabc|slot8"),
            ),
            call(
                4,
                "eth_getStorageAt",
                30,
                40,
                Some("storage|91342|100|0xdef|slot8"),
            ),
            call(
                5,
                "eth_getStorageAt",
                40,
                50,
                Some("storage|1|100|0xabc|slot8"),
            ),
            call(
                6,
                "eth_getStorageAt",
                50,
                60,
                Some("storage|91342|100|0xabc|slot8"),
            ),
        ];
        let seen = duplicates(&events);
        assert_eq!(seen.total_state_reads, 6);
        assert_eq!(
            seen.unique_state_reads, 5,
            "five distinct reads, one asked twice"
        );
        assert_eq!(
            seen.duplicate_state_reads, 1,
            "only the sixth call repeats the first one's read"
        );
        assert_eq!(
            seen.most_repeated,
            vec![("storage|91342|100|0xabc|slot8".to_string(), 2)]
        );
    }

    /// §14's row, including the count of retries the §25 E-versus-B question turns on.
    #[test]
    fn a_method_row_reports_its_calls_and_its_attempts_apart() {
        let mut twice = call(1, "eth_getStorageAt", 0, 100, Some("storage|a"));
        // A retried call's own duration covers both tries, because the choke point
        // stamps the call before the first POST and after the last one: the second
        // attempt is inside the first call's span, not an addition to it.
        twice.started_ns = 0;
        twice.finished_ns = 140;
        twice.duration_ns = 140;
        twice.attempts.push(evm_chain::RpcAttempt {
            started_ns: 100,
            finished_ns: 140,
            duration_ns: 40,
            outcome: evm_chain::CLASS_OK,
        });
        let failed = {
            let mut event = call(2, "eth_getStorageAt", 200, 260, Some("storage|a"));
            event.success = false;
            event.error_class = Some(evm_chain::CLASS_HTTP_STATUS);
            event
        };
        let rows = methods(&[twice, failed]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.method, "eth_getStorageAt");
        assert_eq!(row.count, 2);
        assert_eq!(row.success_count, 1);
        assert_eq!(row.failure_count, 1);
        assert_eq!(row.total_attempts, 3, "two calls, one of which tried twice");
        assert_eq!(row.retried_calls, 1);
        assert_eq!(row.min_duration_ns, 60);
        assert_eq!(row.max_duration_ns, 140);
        assert_eq!(row.total_duration_ns, 200);
        assert_eq!(
            row.error_classes.get(evm_chain::CLASS_HTTP_STATUS),
            Some(&1)
        );

        let json = row.to_json();
        // Two samples cannot carry a p90, and M8.1's policy says so instead of guessing:
        // the row is present, the rank is null, and the reason names the minimum.
        // p50 is reportable at two samples and is the *lower* of them — nearest-rank is
        // `ceil(50·2/100) − 1`, i.e. index 0, which is M5's formula unchanged.
        assert_eq!(json["distribution_ns"]["p50_ns"], 60);
        assert_eq!(json["distribution_ns"]["samples"], 2);
        assert_eq!(json["distribution_ns"]["p90_ns"], Value::Null);
        assert_eq!(
            json["distribution_ns"]["p90_reason"]["reason"],
            "insufficient_sample"
        );
        assert_eq!(json["distribution_ns"]["p90_reason"]["minimum_samples"], 10);
    }

    /// Two methods never merge into one row, and a method whose calls have no key says
    /// which note it carried rather than leaving the field blank.
    #[test]
    fn separate_methods_keep_separate_rows() {
        let events = vec![
            call(1, "eth_getCode", 0, 10, Some("code|a")),
            call(2, "eth_chainId", 10, 20, None),
            call(3, "eth_getCode", 20, 40, Some("code|b")),
        ];
        let rows = methods(&events);
        assert_eq!(
            rows.iter()
                .map(|row| row.method.as_str())
                .collect::<Vec<_>>(),
            vec!["eth_chainId", "eth_getCode"],
            "ordered by method, so the same calls always give the same table"
        );
        assert!(rows[0].key_note.is_some());
        assert!(!rows[0].dedup_eligible);
        assert!(rows[1].dedup_eligible);
        assert_eq!(rows[1].count, 2);
    }

    /// §8's rebuildability: the per-simulation line has to carry the calls themselves,
    /// or the timeline it reports cannot be checked against anything.
    #[test]
    fn one_trace_line_holds_a_whole_simulations_calls_in_order() {
        let window = window(0, 1_000);
        let events = vec![
            call(1, "eth_getStorageAt", 0, 300, Some("storage|a")),
            call(2, "eth_getBalance", 300, 600, Some("balance|a")),
        ];
        let diagnosis = SimulationDiagnosis::new(window, events);
        let line = diagnosis.to_trace_line(&["refused to do something".to_string()]);
        assert_eq!(line["source"], "fixture");
        assert_eq!(line["simulation_duration_ns"], 1_000);
        assert_eq!(line["call_count"], 2);
        assert_eq!(line["calls"][0]["rpc_id"], 1);
        assert_eq!(line["calls"][1]["method"], "eth_getBalance");
        assert_eq!(line["rpc"]["total_calls"], 2);
        assert_eq!(line["duplicates"]["duplicate_state_reads"], 0);
        assert_eq!(line["diagnosis_refusals"].as_array().map(Vec::len), Some(1));
        assert_eq!(diagnosis.method_rows().len(), 2);
    }

    /// §27's simulation matrix, last row: a call that failed still cost the time it took,
    /// so it belongs in the timeline's sums. Dropping it would make a simulation whose node
    /// refused mid-read look faster than it was, and would silently move time into
    /// §11's `non_rpc` bucket — which is the direction §25's E-vs-B question is asked in.
    #[test]
    fn a_failed_call_is_a_call_the_window_still_spent() {
        let window = window(0, 1_000);
        let mut refused = call(1, "eth_getStorageAt", 100, 400, Some("storage|a"));
        refused.success = false;
        refused.error_class = Some(evm_chain::CLASS_NODE_REJECTED);
        let events = vec![
            refused,
            call(2, "eth_getBalance", 500, 700, Some("balance|a")),
        ];
        let diagnosis = SimulationDiagnosis::new(window, events);
        let line = diagnosis.to_trace_line(&[]);
        assert_eq!(line["rpc"]["total_calls"], 2);
        assert_eq!(line["rpc"]["successful_calls"], 1);
        assert_eq!(line["rpc"]["failed_calls"], 1);
        assert_eq!(line["rpc"]["rpc_sum_duration_ns"], 500);
        assert_eq!(line["rpc"]["rpc_union_duration_ns"], 500);
        assert_eq!(line["rpc"]["rpc_wall_duration_ns"], 600, "100→700");
        assert_eq!(line["calls"][0]["error_class"], "node_rejected");
        assert_eq!(line["calls"][1]["success"], true);
        let row = &diagnosis
            .methods
            .iter()
            .find(|row| row.method == "eth_getStorageAt")
            .expect("the refused read has its own row");
        assert_eq!((row.count, row.success_count, row.failure_count), (1, 0, 1));
        assert_eq!(
            row.error_classes.get(evm_chain::CLASS_NODE_REJECTED),
            Some(&1)
        );
    }

    /// A run's directory has to contain the files §22 names, one trace line per
    /// simulation, and the README's seven sections — an evidence dir a reader cannot
    /// interpret is the failure mode §23 exists to prevent.
    #[test]
    fn a_run_writes_the_named_files_with_one_line_per_simulation() {
        let dir = temp_dir("files");
        let mut evidence =
            DiagnosisEvidence::open(&dir, "deadbeef", "disabled").expect("the directory opens");
        let with_calls = SimulationDiagnosis::new(
            window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 300, Some("storage|a")),
                call(2, "eth_getStorageAt", 300, 600, Some("storage|a")),
            ],
        );
        let without_calls = SimulationDiagnosis::new(window(2_000, 3_000), Vec::new());
        evidence
            .record(with_calls, &[])
            .expect("a trace line writes");
        evidence
            .record(without_calls, &[])
            .expect("a call-less simulation still gets its line");
        assert_eq!(evidence.simulations_recorded(), 2);
        let written = evidence.finish().expect("the summaries write");

        for name in [
            TRACES_FILE,
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            README_FILE,
        ] {
            assert!(
                written.join(name).exists(),
                "§22 names {name} and the directory does not hold it"
            );
        }
        // No `.part` file survives a finished run: a temporary name left behind is a
        // write that did not get renamed into place.
        assert!(
            !written.join(format!("{RPC_SUMMARY_FILE}.part")).exists(),
            "the temporary name was not replaced"
        );

        let traces = std::fs::read_to_string(written.join(TRACES_FILE)).expect("readable");
        let lines: Vec<Value> = traces
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line is one JSON object"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["call_count"], 2);
        assert_eq!(lines[1]["rpc"]["rpc_wall_duration_ns"], Value::Null);

        let summary = std::fs::read_to_string(written.join(RPC_SUMMARY_FILE)).expect("readable");
        let summary: Value = serde_json::from_str(&summary).expect("one object");
        assert_eq!(summary["git_revision"], "deadbeef");
        assert_eq!(summary["execution_mode"], "disabled");
        assert_eq!(summary["sources_are_never_blended"], true);
        assert_eq!(summary["instrumentation_issues_no_requests"], true);
        assert_eq!(summary["unit"], "ns");
        assert_eq!(
            summary["minimum_samples_for_rank"]["p99"],
            evm_metrics::minimum_samples_for(99)
        );
        // Two fixture simulations are one source; the call-less one is counted separately
        // rather than folded into the series.
        let sources = summary["per_source"].as_array().expect("a list");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["source"], "fixture");
        assert_eq!(sources[0]["simulations"], 2);
        assert_eq!(sources[0]["simulations_without_calls"], 1);
        assert_eq!(sources[0]["total_calls"], 2);
        // Two samples of one repeated key: the duplicate tally survives into the summary.
        let duplicates = std::fs::read_to_string(written.join(DUPLICATES_FILE)).expect("readable");
        let duplicates: Value = serde_json::from_str(&duplicates).expect("one object");
        assert_eq!(duplicates["per_source"][0]["duplicate_state_reads"], 1);
        assert_eq!(
            duplicates["per_source"][0]["duplicate_ratio"]["denominator"],
            2
        );

        let readme = std::fs::read_to_string(written.join(README_FILE)).expect("readable");
        for heading in [
            "## Data source",
            "## Sample policy",
            "## RPC instrumentation methodology",
            "## Duplicate detection methodology",
            "## Serial / overlap methodology",
            "## Missing data",
            "## Known limitations",
        ] {
            assert!(readme.contains(heading), "§23 asks for {heading}");
        }
        assert!(
            readme.contains("issues no requests") && readme.contains("breakdown_unavailable"),
            "the two claims §23 names specifically have to be in the README, not only in \
             the code comments"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §20's separation, tested at the file level: two sources in one directory must
    /// produce two buckets, and neither may hold the other's samples.
    #[test]
    fn live_and_fixture_simulations_never_share_a_table() {
        let dir = temp_dir("sources");
        let mut evidence = DiagnosisEvidence::open(&dir, "revision", "disabled").expect("opens");
        let mut live_window = window(0, 1_000);
        live_window.source = RpcTraceSource::Live;
        live_window.simulation_id = "live-1".to_string();
        let fixture_window = window(0, 1_000);
        evidence
            .record(
                SimulationDiagnosis::new(
                    live_window,
                    vec![call(1, "eth_getCode", 0, 400, Some("code|a"))],
                ),
                &[],
            )
            .expect("live records");
        evidence
            .record(
                SimulationDiagnosis::new(
                    fixture_window,
                    vec![
                        call(1, "eth_getCode", 0, 100, Some("code|a")),
                        call(2, "eth_getCode", 100, 200, Some("code|b")),
                    ],
                ),
                &[],
            )
            .expect("fixture records");
        let written = evidence.finish().expect("writes");
        let summary: Value = serde_json::from_str(
            &std::fs::read_to_string(written.join(RPC_SUMMARY_FILE)).expect("reads"),
        )
        .expect("one object");
        let sources = summary["per_source"].as_array().expect("a list");
        assert_eq!(
            sources.len(),
            2,
            "one bucket per source, not one blended table"
        );
        assert_eq!(sources[0]["source"], "fixture");
        assert_eq!(sources[0]["total_calls"], 2);
        assert_eq!(sources[1]["source"], "live");
        assert_eq!(sources[1]["total_calls"], 1);
        // Per-method rows carry their source, so a method table read across buckets still
        // cannot merge a fixture's 100 ns with a node's 900 ms by accident.
        let methods = summary["per_method"].as_array().expect("a list");
        assert_eq!(methods.len(), 2);
        assert!(methods
            .iter()
            .all(|row| row["min_duration_ns"].as_u64().is_some()));
        assert_eq!(
            methods.iter().filter(|row| row["source"] == "live").count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A refusal the writer makes has to reach the file, or "we could not trace this
    /// source" becomes indistinguishable from "this source made no calls".
    #[test]
    fn a_writer_that_refuses_says_so_in_the_summary() {
        let dir = temp_dir("refusals");
        let mut evidence = DiagnosisEvidence::open(&dir, "revision", "disabled").expect("opens");
        evidence
            .record(SimulationDiagnosis::new(window(0, 10), Vec::new()), &[])
            .expect("records");
        evidence.refuse(
            "the traced run asked for a fixture directory, which answers from disk".to_string(),
        );
        evidence
            .refuse("this source could not be traced: no adapter to attach a sink to".to_string());
        let written = evidence.finish().expect("writes");
        let summary: Value = serde_json::from_str(
            &std::fs::read_to_string(written.join(SIMULATION_SUMMARY_FILE)).expect("reads"),
        )
        .expect("one object");
        let refusals = summary["diagnosis_refusals"].as_array().expect("a list");
        let text: Vec<String> = refusals
            .iter()
            .filter_map(|row| row.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            text.len(),
            2,
            "both refusals the caller wrote reached the file, in the order it wrote them"
        );
        assert!(
            text[0].contains("answers from disk"),
            "the first one names the source it refused: {text:?}"
        );
        assert!(
            text[1].contains("no adapter"),
            "the second names the reason a sink could not be attached: {text:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A count table and a duration table must not share a spelling. `stats` prints
    /// `min_ns`/`p90_ns` and `unit: "ns"` because everything M8.1 fed it was a duration;
    /// 41 duplicate reads are not 41 nanoseconds, and a reader who added a column of one
    /// to a column of the other would get a figure that means nothing. So the count series
    /// says `count`, and no key in it ends in `_ns`.
    #[test]
    fn a_count_series_is_labelled_as_a_count_and_not_as_nanoseconds() {
        let counts = count_stats(&[3, 7, 7]);
        assert_eq!(counts["unit"], "count");
        assert_eq!(counts["min"], 3);
        assert_eq!(counts["max"], 7);
        assert_eq!(counts["samples"], 3);
        assert!(
            counts
                .as_object()
                .expect("an object")
                .keys()
                .all(|key| !key.ends_with("_ns")),
            "no nanosecond key survives in a count table"
        );
        // The duration tables beside it keep the suffix, because they are durations.
        let durations = stats(&[3, 7, 7]);
        assert_eq!(durations["unit"], "ns");
        assert_eq!(durations["min_ns"], 3);

        // And the sample policy is literally the same code: one count cannot carry a p90,
        // and says so with the same reason and the same minimum as a duration would.
        let single = count_stats(&[41]);
        assert_eq!(single["p90"], Value::Null);
        assert_eq!(single["p90_reason"]["reason"], "insufficient_sample");
        assert_eq!(single["p90_reason"]["minimum_samples"], 10);
    }

    /// The same evidence through two writers has to put the same bytes on disk. A
    /// summary a reader cannot reproduce from the traces is not evidence — and the one
    /// field that legitimately differs between two runs, its wall-clock timestamp, is
    /// pinned here precisely so everything else is being compared.
    #[test]
    fn the_same_calls_write_byte_identical_files() {
        let first = temp_dir("determinism-a");
        let second = temp_dir("determinism-b");
        let diagnosis = || {
            SimulationDiagnosis::new(
                window(0, 1_000),
                vec![
                    call(1, "eth_getStorageAt", 0, 300, Some("storage|a")),
                    call(2, "eth_getBalance", 200, 700, Some("balance|b")),
                    call(3, "eth_getStorageAt", 700, 900, Some("storage|a")),
                ],
            )
            .with_state_reads(Some(spec_tally(true)))
        };
        for dir in [&first, &second] {
            let mut evidence = DiagnosisEvidence::open(dir, "revision", "disabled").expect("opens");
            evidence.generated_at_unix_ms = 1_700_000_000_000;
            evidence.record(diagnosis(), &[]).expect("records");
            evidence.record(diagnosis(), &[]).expect("records");
            evidence.finish().expect("writes");
        }
        for name in [
            TRACES_FILE,
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            README_FILE,
        ] {
            let a = std::fs::read(first.join(name)).expect("readable");
            let b = std::fs::read(second.join(name)).expect("readable");
            assert_eq!(
                a, b,
                "{name} differs between two writers given identical calls"
            );
            assert!(!a.is_empty(), "{name} was written empty");
        }
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }

    /// §12's example tally, in the same numbers the spec quotes for one simulation:
    /// `code 6 miss / 17 hit`, `balance 6/12`, `nonce 6/12`, `storage 20/0`.
    fn spec_tally(reuse: bool) -> StateReadStats {
        let mut stats = StateReadStats::new(reuse);
        stats.code = ReuseTally {
            hits: 17,
            misses: 6,
        };
        stats.balance = ReuseTally {
            hits: 12,
            misses: 6,
        };
        stats.nonce = ReuseTally {
            hits: 12,
            misses: 6,
        };
        stats.storage = ReuseTally {
            hits: 0,
            misses: 20,
        };
        stats
    }

    /// M8.3.1 §12: the boundary's own hits and misses travel with the calls the wire
    /// recorded, in the same line, so §7's "these should align with the request counter"
    /// is checkable by a reader who has only this file. The last assertion is the one that
    /// makes it a second account rather than a restatement: adding a tally to a line
    /// changes nothing about the calls in it, because the tally was never computed from
    /// them.
    #[test]
    fn a_traced_simulation_carries_its_cache_tally_beside_its_calls() {
        let events = || {
            vec![
                call(1, "eth_getCode", 0, 300, Some("code|a")),
                call(2, "eth_getCode", 300, 600, Some("code|a")),
            ]
        };
        let line = SimulationDiagnosis::new(window(0, 1_000), events())
            .with_state_reads(Some(spec_tally(true)))
            .to_trace_line(&[]);
        let cache = &line["state_read_cache"];
        assert_eq!(cache["reuse"], true);
        assert_eq!(cache["code"]["misses"], 6);
        assert_eq!(cache["code"]["hits"], 17);
        assert_eq!(cache["balance"]["hits"], 12);
        assert_eq!(cache["nonce"]["hits"], 12);
        assert_eq!(cache["storage"]["misses"], 20);
        assert_eq!(cache["cache_misses"], 38);
        assert_eq!(cache["cache_hits"], 41);
        // §12 prints `miss = 39` under four kind lines that sum to 38 (6+6+6+20). The 39 is
        // §7's `cached: 39 calls`, which counts the block header read the cache never
        // touches. The sum here is the kinds' own, and it is not rounded up to agree with
        // the spec's total (§7: explain the difference, do not edit the number).
        assert!(
            cache["tally_scope"]
                .as_str()
                .unwrap_or_default()
                .contains("cost no request"),
            "a hit has to say what it saved, or it reads as a call: {cache}"
        );

        // The same calls with no tally at all: `null` with a reason, never a row of zeros
        // (§9's rule, applied to §12's fields).
        let untracked = SimulationDiagnosis::new(window(0, 1_000), events()).to_trace_line(&[]);
        assert_eq!(untracked["state_read_cache"]["reuse"], Value::Null);
        assert_eq!(untracked["state_read_cache"]["measurements"], Value::Null);
        assert!(
            untracked["state_read_cache"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("no state-read boundary"),
            "the reason names what is missing: {}",
            untracked["state_read_cache"]
        );

        // And the two accounts stay two accounts: a tallied line and an untallied one hold
        // the same calls, so nothing here was derived from the call list.
        assert_eq!(line["call_count"], untracked["call_count"]);
        assert_eq!(line["rpc"]["total_calls"], 2);
        assert_eq!(line["calls"], untracked["calls"]);
    }

    /// §6's two arms in one directory are counted as two arms: the per-source cache table
    /// says how many simulations reused, how many did not, and how many never reached a
    /// boundary at all. Summing reuse-on and reuse-off hits into one figure would be the
    /// blended table §13 forbids, so the split has to survive into the summary.
    #[test]
    fn the_per_source_cache_table_keeps_the_two_arms_apart() {
        let dir = temp_dir("cache-arms");
        let mut evidence = DiagnosisEvidence::open(&dir, "revision", "disabled").expect("opens");
        evidence
            .record(
                SimulationDiagnosis::new(window(0, 1_000), Vec::new())
                    .with_state_reads(Some(spec_tally(true))),
                &[],
            )
            .expect("records the cached arm");
        evidence
            .record(
                SimulationDiagnosis::new(window(2_000, 3_000), Vec::new())
                    .with_state_reads(Some(spec_tally(false))),
                &[],
            )
            .expect("records the baseline arm");
        evidence
            .record(
                SimulationDiagnosis::new(window(4_000, 5_000), Vec::new()),
                &[],
            )
            .expect("records a dump-backed simulation with no boundary");
        let written = evidence.finish().expect("writes");

        let duplicates: Value = serde_json::from_str(
            &std::fs::read_to_string(written.join(DUPLICATES_FILE)).expect("reads"),
        )
        .expect("one object");
        let cache = &duplicates["per_source"][0]["state_read_cache"];
        assert_eq!(cache["simulations_with_reuse"], 1);
        assert_eq!(cache["simulations_without_reuse"], 1);
        assert_eq!(cache["simulations_without_tally"], 1);
        // Both tallied arms carry §12's example numbers, so the sums are two of each — and
        // the zero-hit storage kind of the reuse-off arm is still a measured zero, not a
        // missing figure.
        assert_eq!(cache["code"]["hits"], 34);
        assert_eq!(cache["code"]["misses"], 12);
        assert_eq!(cache["balance"]["hits"], 24);
        assert_eq!(cache["nonce"]["hits"], 24);
        assert_eq!(cache["storage"]["hits"], 0);
        assert_eq!(cache["storage"]["misses"], 40);
        assert_eq!(cache["cache_hits"], 82);
        assert_eq!(cache["cache_misses"], 76);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory with a distinct name per test, because these run in one process and
    /// an evidence writer that appends would otherwise read another test's lines.
    fn temp_dir(kind: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "evm-pipeline-diagnosis-{kind}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ))
    }
}
