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

use evm_chain::{RpcAttempt, RpcCallEvent, RpcTraceSource};
use evm_metrics::baseline::stats;
use evm_metrics::unix_ms;
use evm_simulation::StateReadStats;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{PipelineError, Result};

/// The schema version of one line of `simulation-traces.jsonl`.
///
/// Version 2 is M8.3.2's, and it is entirely additive: the `rpc` object gains
/// `rpc_gap_count` / `rpc_gap_widths_ns` / `rpc_gap_distribution_ns` /
/// `simulation_duration_ns` / `rpc_duration_ratio` (§12 and §13), and the line gains the
/// storage, account and `required_by` breakdowns (§6–§10). A v1 line and a v2 line describe
/// the same recorded calls — no field changed meaning or name — so a reader may treat the
/// older rows as a subset rather than as a different format.
pub const DIAGNOSIS_SCHEMA: u64 = 2;

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
    /// §12's `gap_count`: how many *maximal* idle stretches the call window held. A run
    /// of consecutive empty sweep segments is one wait, so this counts waits, not
    /// segments — and it is `None` (not zero) when there was no call to be between.
    pub gap_count: Option<usize>,
    /// §12's per-gap widths, the samples behind `gap_min / gap_median / gap_max`. Sorted
    /// by the sweep rather than by the caller, and empty exactly when `gap_count` is
    /// `Some(0)`: a window with calls that never sat idle is a measurement, not a gap in
    /// the evidence.
    pub gap_widths_ns: Vec<u64>,
    /// The simulation's own span, §13's denominator, carried so the ratio a reader sees
    /// was computed from the pair of figures rather than from one of them.
    pub simulation_duration_ns: u64,
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
            "rpc_gap_count": self.gap_count,
            "rpc_gap_distribution_ns": self.gap_distribution(),
            // M8.3.2 §12: the widths themselves, beside the distribution they produced. A
            // summary file over several runs has to merge samples, not summaries, and this
            // list is the only place they exist after the sweep returns — the same reason
            // `calls[]` sits next to `methods[]` in the line above.
            "rpc_gap_widths_ns": self.gap_widths_ns,
            "non_rpc_duration_ns": self.non_rpc_duration_ns,
            "max_concurrency": self.max_concurrency,
            "calls_clipped": self.calls_clipped,
            "serial_or_overlap": self.serial_or_overlap(),
            "provider_duration_field": PROVIDER_BREAKDOWN,
            "simulation_duration_ns": self.simulation_duration_ns,
            "rpc_duration_ratio": self.rpc_duration_ratio(),
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

    /// §12's gap distribution, or the sentence that says why there is none.
    ///
    /// An empty width list is two different facts depending on whether the window held a
    /// call at all, and only one of them is a measurement: a call-less window has no
    /// in-flight time to be idle between (`None` above says so), while a window full of
    /// calls that never sat idle has measured the absence. The two must not print alike.
    fn gap_distribution(&self) -> Value {
        if !self.gap_widths_ns.is_empty() {
            return stats(&self.gap_widths_ns);
        }
        if self.total_calls == 0 {
            return json!({
                "samples": 0,
                "measured": false,
                "reason": NOTHING_MEASURED,
            });
        }
        json!({
            "unit": "ns",
            "samples": 0,
            "measured": true,
            "min_ns": null,
            "max_ns": null,
            "note": "the call window held no idle stretch: every instant between the first \
                     call's start and the last call's end had a call in flight (§12)",
        })
    }

    /// §13's RPC-versus-local share, on the interval union and reported as two integers.
    ///
    /// The numerator is `rpc_union_duration_ns`, never `rpc_sum_duration_ns`: with two
    /// calls in flight together, `sum` counts the shared instant twice, so a `sum`-based
    /// ratio can exceed its own denominator and would say the simulation spent more time
    /// on RPC than it existed. `simulation_total − rpc_sum` is forbidden for the same
    /// reason, and `non_rpc_duration_ns` above is the union-based subtraction §13 allows.
    /// The quotient is an integer per-mille figure — a float would be a third number
    /// nobody measured.
    fn rpc_duration_ratio(&self) -> Value {
        match (self.union_duration_ns, self.simulation_duration_ns) {
            (Some(union), denominator) if denominator > 0 => json!({
                "numerator": union,
                "numerator_field": "rpc_union_duration_ns",
                "denominator": denominator,
                "denominator_field": "simulation_duration_ns",
                "per_mille": union.saturating_mul(1000) / denominator,
                "notation": "integer per-mille (‰) of the simulation span covered by at \
                             least one RPC call in flight; 1 000 ‰ = the whole span",
                "basis": "interval union, not sum and not total − sum (§13)",
            }),
            (Some(_), _) => json!({
                "numerator": self.union_duration_ns,
                "numerator_field": "rpc_union_duration_ns",
                "denominator": 0_u64,
                "denominator_field": "simulation_duration_ns",
                "per_mille": null,
                "notation": "integer per-mille (‰)",
                "reason": "the simulation window spans no time, so no share of it can be \
                          taken (§13 leaves this null rather than dividing by zero)",
            }),
            (None, _) => json!({
                "numerator": Value::Null,
                "numerator_field": "rpc_union_duration_ns",
                "denominator": self.simulation_duration_ns,
                "denominator_field": "simulation_duration_ns",
                "per_mille": null,
                "notation": "integer per-mille (‰)",
                "reason": NOTHING_MEASURED,
            }),
        }
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
    /// §8's endpoint identity: an opaque digest of the URL this simulation's sink was
    /// built with, never the URL itself. `None` when the sink had no endpoint named,
    /// which is reported as a null rather than as a share or a difference.
    pub endpoint_id: Option<String>,
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
            endpoint_id: None,
        }
    }

    /// Rebuild one simulation from the trace line [`Self::to_trace_line`] wrote for it.
    ///
    /// The line is a complete account of its simulation — window, calls, boundary tally,
    /// endpoint digest — so the four figures this module derives ([`Self::new`]'s timeline,
    /// the duplicate tally, the method rows and the storage and account lists) come back out
    /// of the calls rather than being copied in. That is the point of the decoder: §19's
    /// merged directory is assembled by replaying runs' lines through the same writer that
    /// produced them, and a round trip that returns the same bytes is the evidence that the
    /// replay adds no second account of anything.
    ///
    /// Refusals are not part of `Self` — [`Self::to_trace_line`] takes them as an argument —
    /// so [`trace_line_refusals`] reads the line's own list and a caller passes the pair on
    /// together.
    ///
    /// A line this build cannot read is refused with the reason, never guessed at: silently
    /// dropping a field would put a wrong figure in an evidence file, and a wrong figure in
    /// evidence is the one failure mode this milestone cannot report afterwards.
    pub fn from_trace_line(line: &Value) -> std::result::Result<Self, String> {
        let schema = required_u64(line, "diagnosis_schema")?;
        if schema != DIAGNOSIS_SCHEMA {
            return Err(format!(
                "this line was written by schema {schema} and this build reads \
                 {DIAGNOSIS_SCHEMA}, so the fields a replay depends on are not the ones this \
                 code names"
            ));
        }
        let source = source_from_str(required_str(line, "source")?)?;
        let window = SimulationWindow {
            simulation_id: required_str(line, "simulation_id")?.to_string(),
            source,
            chain_id: optional_u64(line, "chain_id")?,
            block_number: optional_u64(line, "block_number")?,
            state_source: optional_str(line, "state_source")?,
            started_ns: required_u64(line, "started_ns")?,
            finished_ns: required_u64(line, "finished_ns")?,
        };
        let events = trace_calls(line)?;
        let state_reads = trace_state_reads(line)?;
        let endpoint_id = optional_str(line, "endpoint_id")?;
        Ok(Self::new(window, events)
            .with_state_reads(state_reads)
            .with_endpoint(endpoint_id))
    }

    /// Attach the sink's endpoint identity. The third of the three `with_*` attachers, for
    /// the same reason: it is a fact read off the sink after the calls, not a function of
    /// the calls themselves.
    pub fn with_endpoint(mut self, endpoint_id: Option<String>) -> Self {
        self.endpoint_id = endpoint_id;
        self
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
    ///
    /// M8.3.2 adds the two row lists to that same line for the same reason: §6's storage
    /// breakdown and §9's account matrix are groupings *over* rows, so a reader who has the
    /// lines can recompute either table from them and get the file the run wrote — which is
    /// the check that the aggregate is a sum over the calls and not a second measurement.
    pub fn to_trace_line(&self, refusals: &[String]) -> Value {
        let mut line = self.window.to_json();
        let calls: Vec<Value> = self
            .events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
            .collect();
        let methods: Vec<Value> = self.methods.iter().map(|row| row.to_json()).collect();
        let storage = self.storage_rows();
        let account = self.account_rows();
        if let Some(object) = line.as_object_mut() {
            object.insert("rpc".to_string(), self.timeline.to_json());
            object.insert("duplicates".to_string(), self.duplicates.to_json());
            object.insert("methods".to_string(), Value::Array(methods));
            object.insert("call_count".to_string(), json!(calls.len()));
            object.insert("calls".to_string(), Value::Array(calls));
            object.insert("endpoint_id".to_string(), json!(self.endpoint_id));
            object.insert("storage".to_string(), Value::Array(storage.clone()));
            object.insert("storage_read_count".to_string(), json!(storage.len()));
            object.insert("account_reads".to_string(), Value::Array(account.clone()));
            object.insert("account_read_count".to_string(), json!(account.len()));
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

    /// §6's per-slot storage list: one row per `eth_getStorageAt` this simulation asked
    /// for, with the five fields §6 names (chain, block, address, slot, duration) beside
    /// the identity needed to trace the row back to one call (`simulation_id`, `rpc_id`)
    /// and the two §8 and §10 ask for (`endpoint_id`, `required_by`).
    ///
    /// The row is built from the recorded call, not from a second description of it: the
    /// address and slot are the strings the §12 dedup key was built from, so a row and a
    /// duplicate tally cannot name two different words for one read. A call whose params
    /// this build could not read contributes a row with `null` in the fields it could not
    /// name — an unlistable read is still a read the node answered, and dropping it would
    /// make the aggregate below a sum over a subset nobody was told about.
    pub fn storage_rows(&self) -> Vec<Value> {
        self.events
            .iter()
            .filter(|event| event.method == "eth_getStorageAt")
            .map(|event| {
                let (required_by, basis) = self.required_by(&event.method);
                json!({
                    "simulation_id": self.window.simulation_id,
                    "source": self.window.source.as_str(),
                    "rpc_id": event.rpc_id,
                    "chain_id": self.window.chain_id,
                    "block": event.block,
                    "address": event.target.as_deref().map(evm_chain::normalize_address),
                    "slot": event.slot,
                    "duration_ns": event.duration_ns,
                    "started_ns": event.started_ns,
                    "finished_ns": event.finished_ns,
                    "success": event.success,
                    "error_class": event.error_class,
                    "endpoint_id": self.endpoint_id,
                    "dedup_key": event.dedup_key,
                    "key_note": event.key_note,
                    "semantic": SEMANTIC_UNKNOWN,
                    "semantic_reason": STORAGE_SEMANTIC_REASON,
                    "required_by": required_by,
                    "required_by_basis": basis,
                })
            })
            .collect()
    }

    /// §9's account-leg rows: one per `eth_getCode`, `eth_getBalance` and
    /// `eth_getTransactionCount`, addressed so the matrix below can be built by grouping.
    ///
    /// These three are the milestone's subject because M8.3.1 drove each to six; the rows
    /// are what decides whether the six are six accounts asked once each or three accounts
    /// asked twice, which is a question about addresses rather than about methods.
    pub fn account_rows(&self) -> Vec<Value> {
        self.events
            .iter()
            .filter(|event| ACCOUNT_READ_METHODS.contains(&event.method.as_str()))
            .map(|event| {
                let (required_by, basis) = self.required_by(&event.method);
                json!({
                    "simulation_id": self.window.simulation_id,
                    "source": self.window.source.as_str(),
                    "rpc_id": event.rpc_id,
                    "chain_id": self.window.chain_id,
                    "method": event.method,
                    "kind": account_kind(&event.method),
                    "block": event.block,
                    "address": event.target.as_deref().map(evm_chain::normalize_address),
                    "duration_ns": event.duration_ns,
                    "started_ns": event.started_ns,
                    "success": event.success,
                    "endpoint_id": self.endpoint_id,
                    "dedup_key": event.dedup_key,
                    "key_note": event.key_note,
                    "required_by": required_by,
                    "required_by_basis": basis,
                })
            })
            .collect()
    }

    /// §10's `required_by`, decided by the two counts that have to agree.
    ///
    /// The reasoning is structural, and the parts of it that are not a measurement are
    /// named as code locations so a reader can go and look: this sink is attached at one
    /// place in the lifecycle — `arbitrage.rs`'s §B, on the adapter handle the simulation's
    /// [`RpcStateProvider`] reads through, replacing any sink the root adapter carried —
    /// so a call inside it is either an answer the reuse boundary had to go and ask for
    /// (which is the simulation asking) or it is a read the boundary did not account for.
    /// Hence: for the four kinds the boundary tallies, `wire calls == boundary misses` is
    /// `simulation`; a disagreement is `unknown`, because the extra asks are real calls
    /// whose caller this evidence does not name. `orchestration` never comes out of this
    /// function — a call outside the simulation's window is classified from the lifecycle
    /// stage that held it, which is the other sink's job (§14).
    ///
    /// A method with no boundary counter at all (`eth_getBlockByNumber`) is classified by
    /// its call site: the provider's own `header()` is the only reader of a block header in
    /// this path, so its one call is the simulation's. That is a reading of the code, and
    /// the basis says so rather than borrowing the counts' authority.
    fn required_by(&self, method: &str) -> (&'static str, String) {
        let wire = self
            .events
            .iter()
            .filter(|event| event.method == method)
            .count();
        let misses = match method {
            "eth_getCode" => self.state_reads.as_ref().map(|stats| stats.code.misses),
            "eth_getBalance" => self.state_reads.as_ref().map(|stats| stats.balance.misses),
            "eth_getTransactionCount" => self.state_reads.as_ref().map(|stats| stats.nonce.misses),
            "eth_getStorageAt" => self.state_reads.as_ref().map(|stats| stats.storage.misses),
            _ => None,
        };
        match misses {
            Some(misses) if misses == wire => (
                REQUIRED_BY_SIMULATION,
                format!(
                    "{wire} call(s) on the wire, {misses} miss(es) at the state-read boundary: \
                     every ask this sink recorded is one the simulation's own boundary went \
                     to get (§10)"
                ),
            ),
            Some(misses) => (
                REQUIRED_BY_UNKNOWN,
                format!(
                    "{wire} call(s) on the wire against {misses} miss(es) at the state-read \
                     boundary: the two counts disagree, so the caller of the difference is not \
                     named by this evidence (§10's `unknown`, not a guess)"
                ),
            ),
            None if method == "eth_getBlockByNumber" => (
                REQUIRED_BY_SIMULATION,
                "the state provider's `header()` read is the only block-header read in this \
                 path, and the header is what REVM prices a block on: a named call site, not a \
                 count — this kind has no boundary counter to agree with (§10)"
                    .to_string(),
            ),
            None => (
                REQUIRED_BY_UNKNOWN,
                format!(
                    "{wire} call(s) of a method this boundary keeps no tally for, so no count \
                     in this evidence says who asked for it (§10)"
                ),
            ),
        }
    }
}

/// The three account legs §9 counts, by the method each one uses on the wire.
const ACCOUNT_READ_METHODS: [&str; 3] =
    ["eth_getCode", "eth_getBalance", "eth_getTransactionCount"];

/// One HTTP attempt, read back off a trace line.
///
/// A mirror of [`evm_chain::RpcAttempt`] rather than that type with `Deserialize` on it:
/// the recorded type is what the wire path builds, and deriving a decoder for it would put
/// a parse step on the object a call writes at the moment the call finishes. The two are
/// kept in agreement by the round trip — a line decoded and re-emitted comes back with the
/// same attempt list it went in with — not by a comment saying they match.
#[derive(Deserialize)]
struct TraceAttempt {
    started_ns: u64,
    finished_ns: u64,
    duration_ns: u64,
    outcome: String,
}

/// One recorded call, read back off a trace line. Same mirror, same reason.
#[derive(Deserialize)]
struct TraceCall {
    trace_schema: u64,
    rpc_id: u64,
    method: String,
    #[serde(default)]
    block: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    slot: Option<String>,
    started_ns: u64,
    finished_ns: u64,
    duration_ns: u64,
    success: bool,
    #[serde(default)]
    error_class: Option<String>,
    #[serde(default)]
    error_detail: Option<String>,
    attempts: Vec<TraceAttempt>,
    #[serde(default)]
    dedup_key: Option<String>,
    #[serde(default)]
    key_note: Option<String>,
}

/// [`RpcTraceSource`]'s own word back into the variant, for a line read from disk.
fn source_from_str(word: &str) -> std::result::Result<RpcTraceSource, String> {
    [
        RpcTraceSource::Live,
        RpcTraceSource::Replay,
        RpcTraceSource::Fixture,
    ]
    .into_iter()
    .find(|source| source.as_str() == word)
    .ok_or_else(|| format!("`{word}` is not one of the three sources this build writes"))
}

/// The closed set of failure classes, as the strings the record carries.
const ERROR_CLASSES: [&str; 5] = [
    evm_chain::CLASS_SEND_FAILED,
    evm_chain::CLASS_NON_JSON,
    evm_chain::CLASS_HTTP_STATUS,
    evm_chain::CLASS_NODE_REJECTED,
    evm_chain::CLASS_DECODE_FAILED,
];

/// One class word back into the `&'static str` the recorded type holds.
fn class_from_str(word: &str) -> std::result::Result<&'static str, String> {
    ERROR_CLASSES
        .into_iter()
        .find(|known| *known == word)
        .ok_or_else(|| format!("`{word}` is not one of the five failure classes this build emits"))
}

/// One `key_note` word back into the constant, plus [`evm_chain::CLASS_OK`] for an attempt
/// that did not fail — the two sets an event's `error_class` and an attempt's `outcome`
/// share.
fn note_from_str(word: &str) -> std::result::Result<&'static str, String> {
    [
        evm_chain::DEDUP_KEY_UNAVAILABLE_FOR_METHOD,
        evm_chain::DEDUP_KEY_PARAMS_UNREADABLE,
    ]
    .into_iter()
    .find(|known| *known == word)
    .ok_or_else(|| format!("`{word}` is not one of the two `key_note` reasons this build emits"))
}

/// The calls one trace line holds, as the recorded type.
fn trace_calls(line: &Value) -> std::result::Result<Vec<RpcCallEvent>, String> {
    let list: Vec<TraceCall> =
        serde_json::from_value(line["calls"].clone()).map_err(|error| format!("calls: {error}"))?;
    list.into_iter().map(trace_call).collect()
}

fn trace_call(call: TraceCall) -> std::result::Result<RpcCallEvent, String> {
    if call.trace_schema != evm_chain::RPC_TRACE_SCHEMA {
        return Err(format!(
            "call {} was written by RPC trace schema {} and this build reads {}",
            call.rpc_id,
            call.trace_schema,
            evm_chain::RPC_TRACE_SCHEMA
        ));
    }
    let error_class = match &call.error_class {
        Some(word) => Some(class_from_str(word)?),
        None => None,
    };
    let key_note = match &call.key_note {
        Some(word) => Some(note_from_str(word)?),
        None => None,
    };
    let mut attempts = Vec::with_capacity(call.attempts.len());
    for attempt in call.attempts {
        let outcome = if attempt.outcome == evm_chain::CLASS_OK {
            evm_chain::CLASS_OK
        } else {
            class_from_str(&attempt.outcome)?
        };
        attempts.push(RpcAttempt {
            started_ns: attempt.started_ns,
            finished_ns: attempt.finished_ns,
            duration_ns: attempt.duration_ns,
            outcome,
        });
    }
    Ok(RpcCallEvent {
        trace_schema: call.trace_schema,
        rpc_id: call.rpc_id,
        method: call.method,
        block: call.block,
        target: call.target,
        slot: call.slot,
        started_ns: call.started_ns,
        finished_ns: call.finished_ns,
        duration_ns: call.duration_ns,
        success: call.success,
        error_class,
        error_detail: call.error_detail,
        attempts,
        dedup_key: call.dedup_key,
        key_note,
    })
}

/// §12's tally as the line holds it: the boundary's counts, or the reason it had none.
fn trace_state_reads(line: &Value) -> std::result::Result<Option<StateReadStats>, String> {
    let cache = &line["state_read_cache"];
    if cache["reuse"].is_boolean() {
        let stats: StateReadStats = serde_json::from_value(cache.clone())
            .map_err(|error| format!("state_read_cache: {error}"))?;
        return Ok(Some(stats));
    }
    if cache["reason"].is_string() {
        return Ok(None);
    }
    Err("state_read_cache carries neither a reuse flag nor a reason for having none".to_string())
}

/// A line's own refusals, as the argument to [`SimulationDiagnosis::to_trace_line`] wants
/// them. A replay that dropped them would re-emit a line claiming nothing was refused.
pub fn trace_line_refusals(line: &Value) -> Vec<String> {
    line["diagnosis_refusals"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A field a line has to name, as text.
fn required_str<'a>(line: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    line[key]
        .as_str()
        .ok_or_else(|| format!("`{key}` is missing or is not a string"))
}

/// A field a line has to name, as an integer.
fn required_u64(line: &Value, key: &str) -> std::result::Result<u64, String> {
    line[key]
        .as_u64()
        .ok_or_else(|| format!("`{key}` is missing or is not a non-negative integer"))
}

/// A field a line may leave `null`, as text.
fn optional_str(line: &Value, key: &str) -> std::result::Result<Option<String>, String> {
    match &line[key] {
        Value::Null => Ok(None),
        value => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| format!("`{key}` is neither a string nor null")),
    }
}

/// A field a line may leave `null`, as an integer.
fn optional_u64(line: &Value, key: &str) -> std::result::Result<Option<u64>, String> {
    match &line[key] {
        Value::Null => Ok(None),
        value => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` is neither an integer nor null")),
    }
}

/// §7's answer for a slot this build has no mapping for, and the reason it is the answer.
pub const SEMANTIC_UNKNOWN: &str = "unknown";

/// Why every storage row here is `unknown`, stated once as a fact about the code rather
/// than left as an empty cell a reader might fill in.
///
/// This was looked for, not assumed: the only place these three names exist in this
/// repository is as fields decoded from a pool's `Sync` *event* log and from an
/// `eth_call` of `getReserves()` — neither of which is a storage word a `getStorageAt`
/// asked for. §7's rule is that a semantic label requires a mapping the code already
/// holds, and no `slot → name` mapping exists to hold one.
pub const STORAGE_SEMANTIC_REASON: &str = "no slot-to-name mapping exists in this build: the \
     names reserve0 / reserve1 / blockTimestampLast appear only as fields decoded from a pool's \
     Sync event log and from an eth_call of getReserves(), and neither is the storage word a \
     getStorageAt asked for, so §7 forbids labelling this row from its slot number";

/// §10's three labels.
pub const REQUIRED_BY_SIMULATION: &str = "simulation";
pub const REQUIRED_BY_ORCHESTRATION: &str = "orchestration";
pub const REQUIRED_BY_UNKNOWN: &str = "unknown";

/// The §9 kind a method's row belongs to: the three account legs, named the way the
/// reuse boundary names them, so a matrix column and a cache tally say one thing.
fn account_kind(method: &str) -> Option<&'static str> {
    match method {
        "eth_getCode" => Some("code"),
        "eth_getBalance" => Some("balance"),
        "eth_getTransactionCount" => Some("nonce"),
        _ => None,
    }
}

/// §6's aggregate over storage rows: every row this evidence holds, grouped by address,
/// each group showing the slots it read and what they cost.
///
/// A pure function of the rows so the run that wrote them and the assembly that merges
/// three runs' files compute the same table by calling the same code.
///
/// Groups sort by their normalized address, and slots sort by their padded hex text —
/// which for a 64-digit zero-padded word is the same as numeric order, so the table reads
/// as a memory layout and reproduces byte for byte.
pub fn storage_breakdown(rows: &[Value]) -> Value {
    let mut by_address: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut keyless = 0_usize;
    for row in rows {
        match row.get("address").and_then(Value::as_str) {
            Some(address) => by_address.entry(address.to_string()).or_default().push(row),
            None => keyless += 1,
        }
    }
    let addresses: Vec<Value> = by_address
        .iter()
        .map(|(address, group)| storage_address_row(address, group))
        .collect();
    json!({
        "unit": "ns",
        "reads_total": rows.len(),
        "addresses": addresses,
        "address_count": addresses.len(),
        // A row with no address is a call whose params this build could not read. It is
        // counted here rather than dropped, so the sum over the groups above is not
        // silently a sum over a subset.
        "reads_without_address": keyless,
        "grouping": "§6: by normalized address; a group's slots listed with their counts",
        "semantic": STORAGE_SEMANTIC_REASON,
        "mergeability": storage_mergeability(&addresses, rows.len()),
    })
}

/// One address's §6 group.
fn storage_address_row(address: &str, group: &[&Value]) -> Value {
    let durations: Vec<u64> = group
        .iter()
        .filter_map(|row| row.get("duration_ns").and_then(Value::as_u64))
        .collect();
    let mut slot_counts: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut simulations: Vec<&str> = Vec::new();
    let mut blocks: Vec<&str> = Vec::new();
    let mut endpoints: Vec<&str> = Vec::new();
    let mut chain_ids: Vec<u64> = Vec::new();
    for row in group {
        if let Some(slot) = row.get("slot").and_then(Value::as_str) {
            let entry = slot_counts.entry(slot.to_string()).or_default();
            entry.0 += 1;
            entry.1 = entry
                .1
                .saturating_add(row.get("duration_ns").and_then(Value::as_u64).unwrap_or(0));
        }
        push_unique(
            &mut simulations,
            row.get("simulation_id").and_then(Value::as_str),
        );
        push_unique(&mut blocks, row.get("block").and_then(Value::as_str));
        push_unique(
            &mut endpoints,
            row.get("endpoint_id").and_then(Value::as_str),
        );
        if let Some(chain_id) = row.get("chain_id").and_then(Value::as_u64) {
            if !chain_ids.contains(&chain_id) {
                chain_ids.push(chain_id);
            }
        }
    }
    let slots: Vec<Value> = slot_counts
        .iter()
        .map(|(slot, (count, total))| json!({ "slot": slot, "reads": count, "total_duration_ns": total }))
        .collect();
    json!({
        "address": address,
        "chain_ids": chain_ids,
        "reads": group.len(),
        "slot_count": slots.len(),
        "slots": slots,
        "slots_without_word": group.len() - slot_counts.values().map(|(count, _)| count).sum::<usize>(),
        "simulations": simulations,
        "blocks": blocks,
        "endpoint_ids": endpoints,
        "duration_total_ns": durations.iter().sum::<u64>(),
        "duration_distribution_ns": stats(&durations),
        "semantic": SEMANTIC_UNKNOWN,
    })
}

/// §8's question, answered per address: do this group's reads share an endpoint, a block
/// and a simulation. If they do, the group is a `batch_json_rpc` candidate — a word about
/// a future option, with no code behind it in this milestone (§8 forbids implementing it,
/// §18 forbids the whole class of changes it would belong to).
fn storage_mergeability(addresses: &[Value], reads_total: usize) -> Value {
    let per_address: Vec<Value> = addresses
        .iter()
        .map(|row| {
            let endpoints = row["endpoint_ids"].as_array().map(Vec::len).unwrap_or(0);
            let blocks = row["blocks"].as_array().map(Vec::len).unwrap_or(0);
            let simulations = row["simulations"].as_array().map(Vec::len).unwrap_or(0);
            let same_endpoint = endpoints == 1;
            let same_block = blocks == 1;
            let same_simulation = simulations == 1;
            json!({
                "address": row["address"],
                "reads": row["reads"],
                "distinct_endpoints": endpoints,
                "distinct_blocks": blocks,
                "distinct_simulations": simulations,
                "same_endpoint": same_endpoint,
                "same_block": same_block,
                "same_simulation": same_simulation,
                "per_simulation_batchable": same_endpoint && same_block && same_simulation,
            })
        })
        .collect();
    json!({
        "question": "§8: do one address's storage reads share endpoint, block and simulation?",
        "per_address": per_address,
        "reads_total": reads_total,
        "answer_scope": "one simulation at a time: a batch candidate is a simulation's own \
                         reads, so a group spanning several runs counts as not batchable \
                         here even when each run's reads were batchable on their own",
        "status": "candidate_only_not_implemented",
        "note": "batch JSON-RPC is recorded as a candidate direction and is not built in \
                 M8.3.2 (§8, §18)",
    })
}

/// Add a value to a list of distinct ones, keeping first-seen order.
fn push_unique<T: PartialEq + Copy>(list: &mut Vec<T>, value: Option<T>) {
    if let Some(value) = value {
        if !list.contains(&value) {
            list.push(value);
        }
    }
}

/// §9's `account_state_read_matrix`: the same three legs, grouped by the address they
/// were asked of, so the table answers the question §9 asks — whether 18 reads are six
/// accounts each asked for code, balance and nonce, or something else shaped.
///
/// Counts are per address per kind, and the durations come along because §9's question
/// becomes a cost question in Q4 of the report: an address with all three legs is a
/// candidate for one consolidated read, and what that would save is the sum of those
/// three durations, which has to be in the same table to be checked.
pub fn account_matrix(rows: &[Value]) -> Value {
    let mut by_address: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut reads_without_address = 0_usize;
    for row in rows {
        match row.get("address").and_then(Value::as_str) {
            Some(address) => by_address.entry(address.to_string()).or_default().push(row),
            None => reads_without_address += 1,
        }
    }
    let mut totals: BTreeMap<&'static str, (usize, u64)> = BTreeMap::new();
    let mut required_by: BTreeMap<&str, usize> = BTreeMap::new();
    let mut addresses: Vec<Value> = Vec::new();
    for (address, group) in by_address {
        let mut legs: BTreeMap<&'static str, (usize, u64, Vec<u64>)> = BTreeMap::new();
        let mut simulations: Vec<&str> = Vec::new();
        let mut blocks: Vec<&str> = Vec::new();
        let mut endpoints: Vec<&str> = Vec::new();
        for row in &group {
            let kind = match row.get("kind").and_then(Value::as_str) {
                Some("code") => Some("code"),
                Some("balance") => Some("balance"),
                Some("nonce") => Some("nonce"),
                _ => None,
            };
            let duration = row.get("duration_ns").and_then(Value::as_u64).unwrap_or(0);
            if let Some(kind) = kind {
                let leg = legs.entry(kind).or_insert((0, 0, Vec::new()));
                leg.0 += 1;
                leg.1 = leg.1.saturating_add(duration);
                leg.2.push(duration);
                let total = totals.entry(kind).or_insert((0, 0));
                total.0 += 1;
                total.1 = total.1.saturating_add(duration);
            }
            push_unique(
                &mut simulations,
                row.get("simulation_id").and_then(Value::as_str),
            );
            push_unique(&mut blocks, row.get("block").and_then(Value::as_str));
            push_unique(
                &mut endpoints,
                row.get("endpoint_id").and_then(Value::as_str),
            );
            if let Some(label) = row.get("required_by").and_then(Value::as_str) {
                let label = match label {
                    REQUIRED_BY_SIMULATION | REQUIRED_BY_ORCHESTRATION | REQUIRED_BY_UNKNOWN => {
                        label
                    }
                    _ => "unlabelled",
                };
                *required_by.entry(label).or_insert(0) += 1;
            }
        }
        let count = |kind: &str| legs.get(kind).map(|leg| leg.0).unwrap_or(0);
        let total = |kind: &str| legs.get(kind).map(|leg| leg.1).unwrap_or(0);
        addresses.push(json!({
            "address": address,
            "code": count("code"),
            "balance": count("balance"),
            "nonce": count("nonce"),
            "reads": group.len(),
            "all_three_legs": count("code") > 0 && count("balance") > 0 && count("nonce") > 0,
            "duration_total_ns": total("code") + total("balance") + total("nonce"),
            "duration_by_kind_ns": {
                "code": legs.get("code").map(|leg| stats(&leg.2)).unwrap_or(Value::Null),
                "balance": legs.get("balance").map(|leg| stats(&leg.2)).unwrap_or(Value::Null),
                "nonce": legs.get("nonce").map(|leg| stats(&leg.2)).unwrap_or(Value::Null),
            },
            "simulations": simulations,
            "blocks": blocks,
            "endpoint_ids": endpoints,
        }));
    }
    let address_count = addresses.len();
    let all_three = addresses
        .iter()
        .filter(|row| row["all_three_legs"] == json!(true))
        .count();
    json!({
        "unit": "ns",
        "reads_total": rows.len(),
        "address_count": address_count,
        "addresses": addresses,
        "reads_without_address": reads_without_address,
        "totals": {
            "code": json!({ "reads": totals.get("code").map(|t| t.0).unwrap_or(0), "duration_ns": totals.get("code").map(|t| t.1).unwrap_or(0) }),
            "balance": json!({ "reads": totals.get("balance").map(|t| t.0).unwrap_or(0), "duration_ns": totals.get("balance").map(|t| t.1).unwrap_or(0) }),
            "nonce": json!({ "reads": totals.get("nonce").map(|t| t.0).unwrap_or(0), "duration_ns": totals.get("nonce").map(|t| t.1).unwrap_or(0) }),
        },
        "required_by": required_by,
        "same_account_all_three_legs": {
            "addresses": address_count,
            "with_all_three_legs": all_three,
            "question": "§9: are these reads still mostly the same accounts asked three \
                         things each? A count of addresses holding all three legs, against \
                         the addresses that appear at all",
        },
        "grouping": "§9: by normalized address, one row per address, one column per leg \
                     (code / balance / nonce), over every simulation's rows merged",
        "diagnosis_only": "§9 says do not optimize; this table is the measurement, and no \
                           read in it was reordered, merged, or avoided to produce it",
    })
}

/// §14's six classes, in the spec's own words. A lifecycle call is placed in exactly one of
/// them by the stage whose span held it.
pub const LIFECYCLE_SIMULATION_STATE: &str = "simulation-state";
pub const LIFECYCLE_SIMULATION_CONTEXT: &str = "simulation-context";
pub const LIFECYCLE_ORCHESTRATION: &str = "orchestration";
pub const LIFECYCLE_PREFLIGHT: &str = "preflight";
pub const LIFECYCLE_DETECTION: &str = "detection";
pub const LIFECYCLE_UNKNOWN: &str = "unknown";

/// §19's seventh file: the reads that are not a simulation's, reported apart so a reader
/// never has to add a preflight cost to a state-acquisition cost.
pub const OUTSIDE_FILE: &str = "outside-simulation-rpc.json";

/// The six, in the order a table lists them — the spec's order, not alphabetical, because
/// §14's question is "how much of this is the simulation's business" and that reading goes
/// from the inside out.
const LIFECYCLE_CLASSES: [&str; 6] = [
    LIFECYCLE_SIMULATION_STATE,
    LIFECYCLE_SIMULATION_CONTEXT,
    LIFECYCLE_DETECTION,
    LIFECYCLE_PREFLIGHT,
    LIFECYCLE_ORCHESTRATION,
    LIFECYCLE_UNKNOWN,
];

/// One lifecycle stage's span, copied out of the latency trace M8.1 already stamped.
///
/// §14 wants to know which stage held a call, and the lifecycle already says so: every
/// stage boundary in this run is a reading of the same monotonic clock the RPC sink stamps
/// against (§7's one origin), so the answer is a containment test, not a second measurement.
/// Nothing here takes a clock reading or re-opens a stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpan {
    pub stage: &'static str,
    pub outcome: &'static str,
    /// The interval a call can fall inside. `None` is a fact about the span rather than a
    /// value that went missing: a skipped stage has no interval at all, and a stage whose
    /// duration was read off another system's *millisecond* stamps cannot be claimed to
    /// contain a call stamped in nanoseconds — the two ends of that span are not the run's
    /// own instants, so holding a call would be an inference §14's "不要猜" forbids.
    pub window_ns: Option<(u64, u64)>,
    /// Why `window_ns` is what it is, so a call that fell through to `unknown` can name the
    /// spans it failed to land in rather than leaving the reader to guess which was tried.
    pub window_note: &'static str,
}

impl StageSpan {
    /// The spans of one finished lifecycle, in stage order.
    pub fn of(trace: &evm_metrics::LatencyTrace) -> Vec<Self> {
        trace.stage_records().map(Self::of_record).collect()
    }

    fn of_record(record: &evm_metrics::StageRecord) -> Self {
        let interval = match (record.started_ns, record.ended_ns) {
            (Some(started), Some(ended)) if ended >= started => Some((started, ended)),
            _ => None,
        };
        let nanosecond = record.granularity == Some(evm_metrics::Granularity::Nanosecond);
        let (window_ns, window_note) = match (interval, nanosecond) {
            (Some(interval), true) => (Some(interval), NANOSECOND_SPAN),
            (Some(_), false) => (None, COARSE_SPAN),
            (None, _) => (None, OPEN_SPAN),
        };
        Self {
            stage: record.stage.as_str(),
            outcome: record.outcome.as_str(),
            window_ns,
            window_note,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "stage": self.stage,
            "outcome": self.outcome,
            "started_ns": self.window_ns.map(|(start, _)| start),
            "ended_ns": self.window_ns.map(|(_, end)| end),
            "usable_for_classification": self.window_ns.is_some(),
            "note": self.window_note,
        })
    }
}

const NANOSECOND_SPAN: &str = "both ends are readings of this run's monotonic clock, so a \
     call stamped on that clock either falls inside this span or it does not";
const COARSE_SPAN: &str = "this span's duration was read off millisecond stamps the run took \
     for its own reasons, so it is not claimed to hold a nanosecond-stamped call";
const OPEN_SPAN: &str = "no closed interval: the stage was skipped, was still open when the \
     run ended, or was written as a duration with no instants behind it";

/// §14's class for a call made while one stage's span was open.
///
/// The mapping is a reading of where each stage's reads come from in this build, and the
/// two that could be argued are stated rather than left implicit:
///
/// - `observation` is `simulation-context`, not `orchestration`, because the two calls it
///   holds (`latest_block`, then `get_block_context` at that height) are what fix the pin
///   every state read below is made at — they are context the simulation is defined against.
/// - `simulation` is `simulation-state` and is not expected to appear: those calls go
///   through the *other* sink, attached to the adapter the provider reads through. A row
///   here inside that span would mean the two sinks disagree about which one watched the
///   call, which is a wiring bug the evidence would then show.
fn lifecycle_class(stage: &str) -> &'static str {
    match stage {
        "observation" => LIFECYCLE_SIMULATION_CONTEXT,
        "opportunity_detection" => LIFECYCLE_DETECTION,
        "simulation" => LIFECYCLE_SIMULATION_STATE,
        "preflight" => LIFECYCLE_PREFLIGHT,
        _ => LIFECYCLE_ORCHESTRATION,
    }
}

/// The span holding `at_ns`, if one of the run's nanosecond spans does.
fn holding_span(spans: &[StageSpan], at_ns: u64) -> Option<&StageSpan> {
    spans.iter().find(|span| {
        span.window_ns
            .is_some_and(|(start, end)| start <= at_ns && at_ns <= end)
    })
}

/// What the run's spans did *not* hold, in the space a table cell has: every stage that was
/// tried and why it could not have held the instant. A call with no span is reported as
/// `unknown` with this beside it, which is the difference between "nothing classifies this"
/// and "these stages were open and none of them contains it".
fn unheld_note(spans: &[StageSpan], at_ns: u64) -> String {
    let tried: Vec<String> = spans
        .iter()
        .filter(|span| span.window_ns.is_some())
        .map(|span| {
            let (start, end) = span.window_ns.unwrap_or((0, 0));
            format!("{}({start}–{end})", span.stage)
        })
        .collect();
    if tried.is_empty() {
        return format!(
            "started_ns {at_ns} falls inside no span of this run, and this run wrote no \
             nanosecond stage span at all — the lifecycle trace was not being recorded, so \
             there was nothing to classify against (§14's `unknown`, not a guess)"
        );
    }
    format!(
        "started_ns {at_ns} falls inside none of the run's closed nanosecond spans [{}]; the \
         gaps between spans and the coarse spans are named in `stage_spans`",
        tried.join(", ")
    )
}

/// §14's rows: one per call the *lifecycle* sink recorded, each carrying the stage that
/// held it and the class that follows from it.
///
/// These are the reads the simulation's own sink never sees — the head and header that fix
/// the pin, the reserves priced during detection, the balances and fee facts gathered at the
/// gate — and §14's rule is that they stay out of the 39-call state-read baseline. They are
/// rows in a file of their own for the same reason the simulation's calls are lines of
/// their own: adding them to the 39 would make a preflight saving look like a state-acquisition
/// saving, which is the confusion the spec names and forbids.
pub fn lifecycle_rows(
    events: &[RpcCallEvent],
    spans: &[StageSpan],
    endpoint_id: Option<&str>,
) -> Vec<Value> {
    events
        .iter()
        .map(|event| {
            let held = holding_span(spans, event.started_ns);
            let (stage, class, basis) = match held {
                Some(span) => (
                    Some(span.stage),
                    lifecycle_class(span.stage),
                    format!(
                        "started_ns {} falls inside the `{}` span ({:?}), which this build \
                         reaches by the reads of that stage — §14 files a call with the stage \
                         that held it",
                        event.started_ns,
                        span.stage,
                        span.window_ns
                            .map(|(start, end)| [start, end])
                            .unwrap_or_default(),
                    ),
                ),
                None => (
                    None,
                    LIFECYCLE_UNKNOWN,
                    unheld_note(spans, event.started_ns),
                ),
            };
            json!({
                "rpc_id": event.rpc_id,
                "method": event.method,
                "block": event.block,
                "target": event.target.as_deref().map(evm_chain::normalize_address),
                "slot": event.slot,
                "started_ns": event.started_ns,
                "finished_ns": event.finished_ns,
                "duration_ns": event.duration_ns,
                "success": event.success,
                "error_class": event.error_class,
                "attempts": event.attempts.len(),
                "endpoint_id": endpoint_id,
                "dedup_key": event.dedup_key,
                "key_note": event.key_note,
                "stage": stage,
                "class": class,
                "class_basis": basis,
            })
        })
        .collect()
}

/// §14's file: the lifecycle rows above, grouped by class and by method, beside the spans
/// that did the grouping and the two reads this instrumentation provably cannot reach.
///
/// Every class in [`LIFECYCLE_CLASSES`] gets a row even when nothing landed in it, because
/// §14 asks which of the six there are any of at all — a class that is absent has to read as
/// measured-and-empty rather than as missing.
pub fn outside_simulation_table(
    rows: &[Value],
    spans: &[StageSpan],
    endpoint_id: Option<&str>,
    simulation_calls: u64,
) -> Value {
    let mut table = outside_table(rows, endpoint_id, simulation_calls);
    if let Some(object) = table.as_object_mut() {
        object.insert(
            "stage_spans".to_string(),
            Value::Array(spans.iter().map(StageSpan::to_json).collect()),
        );
    }
    table
}

/// §19's `outside-simulation-rpc.json` for a directory assembled from runs that already
/// exist — the same grouping ([`outside_table`]) over the pooled rows, so merging three runs
/// calls the code that wrote one rather than a second implementation to keep in agreement.
///
/// What cannot pool is the stage spans: a `started_ns` is nanoseconds since *its own* run's
/// monotonic origin, so three runs' spans on one list would be three clocks pretending to be
/// one. They stay with the run they were stamped on, under `assembled_from`, beside that
/// run's endpoint digest and its own call count — and every pooled row names the run it was
/// measured in, so a class in the merged table can be traced back to one `class_basis`.
///
/// `endpoint_id` is stated only when every run agrees on it; two digests is a finding about
/// two providers, not one identity to print.
pub fn outside_simulation_table_assembled(runs: &[Value], simulation_calls: u64) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut provenance: Vec<Value> = Vec::new();
    let mut endpoints: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for run in runs {
        let name = run["run"].as_str().unwrap_or("?");
        if let Some(list) = run["rows"].as_array() {
            for row in list {
                let mut row = row.clone();
                if let Some(object) = row.as_object_mut() {
                    object.insert("assembled_from_run".to_string(), json!(name));
                }
                rows.push(row);
            }
        }
        if let Some(endpoint) = run["endpoint_id"].as_str() {
            endpoints.insert(endpoint);
        }
        provenance.push(json!({
            "run": name,
            "endpoint_id": run["endpoint_id"].clone(),
            "generated_at_unix_ms": run["generated_at_unix_ms"].clone(),
            "calls": run["calls"].clone(),
            "stage_spans": run["stage_spans"].clone(),
        }));
    }
    let endpoint_id = if endpoints.len() == 1 {
        endpoints.iter().next().copied()
    } else {
        None
    };
    let mut table = outside_table(&rows, endpoint_id, simulation_calls);
    if let Some(object) = table.as_object_mut() {
        object.insert("assembled_from".to_string(), Value::Array(provenance));
    }
    table
}

/// The grouping both paths above share: per-class and per-method aggregates over the rows,
/// the duration distribution over them, and the two statements §14 requires beside any
/// count of the lifecycle's other reads.
fn outside_table(rows: &[Value], endpoint_id: Option<&str>, simulation_calls: u64) -> Value {
    let mut by_class: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        let class = row["class"].as_str().unwrap_or(LIFECYCLE_UNKNOWN);
        by_class.entry(class).or_default().push(row);
    }
    let mut by_method: BTreeMap<&str, (u64, u64, Vec<u64>)> = BTreeMap::new();
    for row in rows {
        let method = row["method"].as_str().unwrap_or("?");
        let duration = row["duration_ns"].as_u64().unwrap_or(0);
        let entry = by_method.entry(method).or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(duration);
        entry.2.push(duration);
    }
    let total = rows.len();
    let per_class: Vec<Value> = LIFECYCLE_CLASSES
        .iter()
        .map(|class| {
            let held = by_class.get(class).cloned().unwrap_or_default();
            let durations: Vec<u64> = held
                .iter()
                .filter_map(|row| row["duration_ns"].as_u64())
                .collect();
            let methods: Vec<&str> = held
                .iter()
                .filter_map(|row| row["method"].as_str())
                .collect();
            let stages: Vec<&str> = held
                .iter()
                .filter_map(|row| row["stage"].as_str())
                .collect();
            json!({
                "class": class,
                "calls": held.len(),
                "methods": tidy(&methods),
                "stages": tidy(&stages),
                "duration_total_ns": durations.iter().fold(0u64, |sum, d| sum.saturating_add(*d)),
                "duration_distribution_ns": stats(&durations),
                "share_of_lifecycle_calls_per_mille": share(held.len(), total),
                "note": class_note(class),
            })
        })
        .collect();
    let per_method: Vec<Value> = by_method
        .iter()
        .map(|(method, (calls, total_ns, durations))| {
            json!({
                "method": method,
                "calls": *calls,
                "duration_total_ns": *total_ns,
                "duration_distribution_ns": stats(durations),
                "count_stats": count_stats(&[*calls]),
            })
        })
        .collect();
    let durations: Vec<u64> = rows
        .iter()
        .filter_map(|row| row["duration_ns"].as_u64())
        .collect();
    json!({
        "unit": "ns",
        "calls": total,
        "duration_total_ns": durations.iter().fold(0u64, |sum, d| sum.saturating_add(*d)),
        "duration_distribution_ns": stats(&durations),
        "per_class": Value::Array(per_class),
        "per_method": Value::Array(per_method),
        "rows": Value::Array(rows.to_vec()),
        "endpoint_id": endpoint_id,
        "core_state_reads_not_included": {
            "simulation_calls_recorded_elsewhere": simulation_calls,
            "rule": "§14: the reads a simulation makes through its own sink stay in that \
                     simulation's line and in the 39-call baseline. This file holds the rest \
                     of the lifecycle, and no call is in both — the sink a derived adapter \
                     carries replaces the one it inherited, so each call lands in exactly one \
                     list (crates/chain/tests/rpc_trace_safety.rs)",
        },
        "unreachable_reads": Value::Array(BLIND_SPOTS.iter().map(|(what, site, effect)| {
            json!({
                "reads": what,
                "where": site,
                "why_this_file_cannot_count_them": effect,
                "reported_as": "not counted here, and not counted as zero either",
            })
        }).collect()),
        "diagnosis_only": "§17 and §18: this file is written by an observer. It changes no \
                           cache, adds no batch, opens no second connection and asks the node \
                           for nothing the run was not already going to ask",
    })
}

/// One class's own meaning, beside the row that uses it, so a table explains itself.
fn class_note(class: &str) -> &'static str {
    match class {
        LIFECYCLE_SIMULATION_STATE => {
            "a call inside the simulation's own span, seen by the \
             lifecycle sink — expected to be empty, because those calls go to the \
             simulation's sink; a non-zero count here says the two sinks disagree about who \
             watched the call"
        }
        LIFECYCLE_SIMULATION_CONTEXT => {
            "the head read and the header read that fix the pin \
             every state read below is made at (`latest_block`, `get_block_context`)"
        }
        LIFECYCLE_DETECTION => {
            "the reads that price the route before a simulation exists: \
             the two pools' reserves and anything else `price_legs` asks for"
        }
        LIFECYCLE_PREFLIGHT => {
            "the gate's own reads, gathered through the same adapter the \
             market was priced from: head, balances, allowances, fee facts"
        }
        LIFECYCLE_ORCHESTRATION => {
            "a call made while a stage this milestone does not name \
             was open — the run's bookkeeping rather than its state acquisition"
        }
        _ => {
            "no closed nanosecond stage span held this call, so this evidence does not say \
             who asked for it (§10's `unknown`, stated with the spans that were tried)"
        }
    }
}

/// The reads the instrumentation provably cannot see, named rather than left as an empty
/// space a reader might fill with a zero. Each is a fact about where the choke point is.
const BLIND_SPOTS: [(&str, &str, &str); 3] = [
    (
        "eth_chainId",
        "HttpChainAdapter::connect (crates/chain/src/rpc.rs) — the call that learns the \
         chain id before any adapter exists to be watched",
        "a sink is attached to an adapter, and the adapter is what `connect` returns, so the \
         request that produced it happened first. One per adapter construction; its duration \
         is in no table here",
    ),
    (
        "the execution lane's reads",
        "the lane's own HttpChainAdapter (crates/execution/src/giwa/sequencer_direct.rs) — \
         head, nonce, balance, fee parameters, receipts and the submissions themselves",
        "the lane builds a second adapter from the same URL; a sink attached to the \
         pipeline's handle is not on that one. Their cost is bounded by the run's \
         build/submit/receipt stage spans, which are in the latency trace, not here",
    ),
    (
        "WebSocket reads",
        "the live ingestion path (crates/chain/src/live), which is not this run's route \
         lifecycle",
        "a route run reads the chain over HTTP only; the subscription transport issues no \
         call that this sink could see",
    ),
];

/// A per-mille share as the two integers it is (M8.1's rule: a ratio is never a float).
fn share(part: usize, whole: usize) -> Value {
    match whole {
        0 => json!({
            "per_mille": Value::Null,
            "reason": NOTHING_MEASURED,
        }),
        whole => json!({
            "per_mille": (u64::try_from(part).unwrap_or(0) * 1_000) / u64::try_from(whole).unwrap_or(1),
            "numerator": part,
            "denominator": whole,
            "notation": "per mille: part × 1000 ÷ whole, integer division",
        }),
    }
}

/// A list with repeats folded away and the survivors sorted, so two runs that made the same
/// calls in a different order write the same bytes.
fn tidy(list: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = list.iter().map(|value| (*value).to_string()).collect();
    out.sort();
    out.dedup();
    out
}

/// [`share`]'s shape for a pair of nanosecond figures.
///
/// The two are kept apart because their zero denominators mean different things: no calls is
/// [`NOTHING_MEASURED`], while no *time* measured is the absence §5 says a share cannot be
/// taken of. Both come back `null` with the reason, and neither comes back `0`.
fn ratio_ns(part_ns: u64, whole_ns: u64) -> Value {
    match whole_ns {
        0 => json!({
            "per_mille": Value::Null,
            "numerator": part_ns,
            "denominator": 0_u64,
            "reason": "the denominator is zero nanoseconds: nothing was measured to take a \
                       share of, so this is null rather than 0 (§5)",
        }),
        whole => json!({
            "per_mille": part_ns.saturating_mul(1_000) / whole,
            "numerator": part_ns,
            "denominator": whole,
            "notation": "per mille: part × 1000 ÷ whole, integer division, both in ns",
        }),
    }
}

// ---------------------------------------------------------------------------
// M8.3.2 §19's acquisition tables
// ---------------------------------------------------------------------------

/// §19's fifth file: every storage read traced to its word, then grouped by address (§6).
pub const STORAGE_BREAKDOWN_FILE: &str = "storage-breakdown.json";
/// §19's sixth file: the three account legs per address (§9).
pub const ACCOUNT_MATRIX_FILE: &str = "account-read-matrix.json";
/// §19's seventh file: the idle stretches between calls (§12).
pub const RPC_GAPS_FILE: &str = "rpc-gaps.json";
/// §19's ninth file: §25's A–G verdict with the rule that produced it beside its number.
pub const BOTTLENECK_FILE: &str = "bottleneck-classification.json";

/// The four lists one trace line holds about *calls* rather than about their analysis.
///
/// [`duration_row`] drops them and [`storage_breakdown`] / [`account_matrix`] read them back,
/// so a file that groups over calls and a file that groups over simulations are both views of
/// one published line rather than two computations of it.
const CALL_LIST_KEYS: [&str; 4] = ["calls", "storage", "account_reads", "diagnosis_refusals"];

/// One simulation's analysis, with its call lists left out.
///
/// This is a projection of the line the run already wrote — the same bytes, minus the three
/// lists that belong to the call-shaped tables — and both the run's own writer and the
/// assembly over several runs call it on a line. That is what makes `rpc-gaps.json` and
/// `bottleneck-classification.json` a *regrouping* of published figures rather than a second
/// measurement a reader would have to reconcile against the first: a gap width here and the
/// same width in the line cannot disagree, because one is copied out of the other.
pub fn duration_row(line: &Value) -> Value {
    let mut row = serde_json::Map::new();
    for (key, value) in line.as_object().into_iter().flatten() {
        if !CALL_LIST_KEYS.contains(&key.as_str()) {
            row.insert(key.clone(), value.clone());
        }
    }
    Value::Object(row)
}

/// §12's `rpc-gaps.json`: every idle stretch inside every simulation's call window.
///
/// The distribution is over *stretches*, one sample per wait, and the per-simulation column
/// beside it says how many stretches each run produced — M8.2 found 43–58 ms gaps, and whether
/// that is one gap per simulation or nine is a different finding about the same numbers.
/// Samples are merged, never summaries: a mean of several runs' medians is not this milestone's
/// `gap_median`.
pub fn rpc_gaps(rows: &[Value]) -> Value {
    let mut widths: Vec<u64> = Vec::new();
    let mut per_simulation: Vec<Value> = Vec::new();
    let mut counts_per_simulation: Vec<u64> = Vec::new();
    let mut with_calls = 0_usize;
    let mut with_gaps = 0_usize;
    let mut gap_count_total = 0_u64;
    let mut gap_duration_total_ns = 0_u64;
    let mut wall_total_ns = 0_u64;
    let mut span_total_ns = 0_u64;
    for row in rows {
        let rpc = &row["rpc"];
        let calls = value_u64(rpc, "total_calls").unwrap_or(0);
        if calls > 0 {
            with_calls += 1;
        }
        let own_widths: Vec<u64> = rpc["rpc_gap_widths_ns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect();
        if !own_widths.is_empty() {
            with_gaps += 1;
        }
        let own_count = value_u64(rpc, "rpc_gap_count").unwrap_or(0);
        let own_duration = value_u64(rpc, "rpc_gap_duration_ns").unwrap_or(0);
        counts_per_simulation.push(own_count);
        gap_count_total = gap_count_total.saturating_add(own_count);
        gap_duration_total_ns = gap_duration_total_ns.saturating_add(own_duration);
        wall_total_ns =
            wall_total_ns.saturating_add(value_u64(rpc, "rpc_wall_duration_ns").unwrap_or(0));
        span_total_ns =
            span_total_ns.saturating_add(value_u64(row, "simulation_duration_ns").unwrap_or(0));
        widths.extend(own_widths.iter().copied());
        let own_distribution = stats(&own_widths);
        per_simulation.push(json!({
            "simulation_id": row["simulation_id"],
            "source": row["source"],
            "chain_id": row["chain_id"],
            "block_number": row["block_number"],
            "endpoint_id": row["endpoint_id"],
            "total_calls": calls,
            "simulation_duration_ns": row["simulation_duration_ns"],
            "rpc_wall_duration_ns": rpc["rpc_wall_duration_ns"],
            "rpc_union_duration_ns": rpc["rpc_union_duration_ns"],
            "rpc_gap_count": rpc["rpc_gap_count"],
            "rpc_gap_duration_ns": rpc["rpc_gap_duration_ns"],
            "rpc_gap_widths_ns": own_widths,
            "own_gap_distribution_ns": own_distribution,
            "gap_share_of_call_window_per_mille": ratio_ns(
                own_duration,
                value_u64(rpc, "rpc_wall_duration_ns").unwrap_or(0),
            ),
        }));
    }
    let distribution = stats(&widths);
    json!({
        "unit": "ns",
        "simulations": rows.len(),
        "simulations_with_calls": with_calls,
        "simulations_without_calls": rows.len().saturating_sub(with_calls),
        "simulations_with_idle_stretches": with_gaps,
        // §12's four names, spelled as §12 asks for them, each read out of the merged
        // distribution above. A null here is M8.1's sample rule, not a missing figure: one
        // gap has a min and a max and no median.
        "gap_total_ns": gap_duration_total_ns,
        "gap_count": gap_count_total,
        "gap_min_ns": rank_of(&distribution, "min_ns"),
        "gap_median_ns": rank_of(&distribution, "p50_ns"),
        "gap_max_ns": rank_of(&distribution, "max_ns"),
        "gap_median_is_p50": "the same nearest-rank p50 every other table in this directory \
                              uses. A population of one stretch has a min and a max and no \
                              median, and says `null` with `insufficient_sample` beside it \
                              rather than reprinting its single sample under a second name.",
        "gap_width_distribution_ns": distribution,
        "gaps_per_simulation_distribution": count_stats(&counts_per_simulation),
        "gap_total_over_call_window_per_mille": ratio_ns(gap_duration_total_ns, wall_total_ns),
        "gap_total_over_simulation_span_per_mille": ratio_ns(gap_duration_total_ns, span_total_ns),
        "per_simulation": per_simulation,
        "definitions": "a gap is a maximal stretch inside one simulation's call window — between \
                       its first call's start and its last call's end — with no call in flight \
                       (§12). Time before the first call and after the last is not a gap: it is \
                       in `non_rpc_duration_ns` or outside the window, and counting it here \
                       would let one call at the end of a stage read as a long wait.",
        "measured_at": "the same one sweep that produced `rpc_union_duration_ns` in each \
                        simulation's line (`crates/pipeline/src/diagnosis.rs::scan`); this file \
                        merges the widths those lines publish and takes no new clock reading",
        "diagnosis_only": "§12 and §18: a gap is reported, not filled. Nothing in this build \
                           prefetches, batches or overlaps a call to shorten one",
    })
}

/// A figure named `key` out of a `stats` row, or `null` when that row has no samples.
fn rank_of(distribution: &Value, key: &str) -> Value {
    distribution.get(key).cloned().unwrap_or(Value::Null)
}

/// A `u64` field of a row, `None` when it is absent or null.
fn value_u64(row: &Value, key: &str) -> Option<u64> {
    row.get(key).and_then(Value::as_u64)
}

/// §25's seven categories, in §25's own order and §25's own words.
///
/// A `bottleneck-classification.json` verdict is chosen from this list and from nothing else,
/// which is §19's rule. G is in it because §25 counts it as an answer: mixed or insufficient
/// evidence is a finding, and it is a different finding from a named category.
pub const BOTTLENECK_CATEGORIES: [(&str, &str); 7] = [
    ("A", "RPC volume bound"),
    ("B", "RPC latency bound"),
    ("C", "Serial dependency bound"),
    ("D", "Duplicate state-read bound"),
    ("E", "Provider/connection bound"),
    ("F", "Local simulation compute bound"),
    ("G", "Mixed / insufficient evidence"),
];

/// The four verdicts a category can come to, all of them about *this evidence*:
/// `dominant` and `material` are positions against a threshold over a measured quantity,
/// `ruled_out` is a measurement that landed on the safe side of one, and `not_measured` is an
/// absent figure — which §5 forbids dressing up as either of the other three.
pub const DOMINANT: &str = "dominant";
pub const MATERIAL: &str = "material";
pub const RULED_OUT: &str = "ruled_out_by_measurement";
pub const NOT_MEASURED: &str = "not_measured";

/// A category whose own quantity covers at least half of the measured span is the one a
/// reader leads with.
///
/// Half is a declaration, not a measurement, and it is published in the file beside every
/// figure so anyone who prefers a different line can re-rank the same integers. It is set at
/// half because that is the smallest share for which 「this is where the time went」 is true
/// rather than 「this contributed」.
const DOMINANT_PER_MILLE: u64 = 500;

/// The share at which a category stops being a footnote in a report that has room for two
/// sentences.
const MATERIAL_PER_MILLE: u64 = 50;

/// The per-call cost under which this build stops calling a run latency-bound.
///
/// Also a declaration. It sits at 100 ms because the fastest single call M8.2 measured on
/// this endpoint was 232.5 ms: a run whose cheapest call came in under a tenth of a second
/// would be asking a different question of a different transport, and §15's warning — do not
/// assume slow means provider slow — is exactly the mistake this floor exists to make
/// checkable rather than assumed. The measured floor is published beside every use of it.
const LATENCY_FLOOR_NS: u64 = 100_000_000;

/// Why a category has no share to report.
const NO_SHARE_MEASURED: &str = "no call was recorded in the simulations this table covers, so \
                                 the quantity this rule reads has no measurement behind it — \
                                 null, not 0 (§5)";

/// The figures every category's rule reads, folded over one source's rows.
///
/// Folded from [`duration_row`]s rather than from the runs: a directory assembled from several
/// runs and a single run's own directory therefore go through the same arithmetic, and a
/// figure that moves between the two is a figure the assembly changed, which is what the
/// assembler's byte-comparison test is for.
#[derive(Clone, Debug, Default)]
struct Acquisition {
    simulations: usize,
    simulations_with_calls: usize,
    calls: u64,
    successful: u64,
    failed: u64,
    attempts: u64,
    retried: u64,
    sum_ns: u64,
    union_ns: u64,
    serial_ns: u64,
    overlap_ns: u64,
    non_rpc_ns: u64,
    /// Whether any simulation here had a call to subtract from its span. A fixture whose
    /// state came from a dump has a span and no union, so its time is not evidence about
    /// local compute and is reported as unmeasured rather than as all-local.
    non_rpc_measured: bool,
    span_ns: u64,
    max_concurrency: usize,
    serial_simulations: usize,
    overlap_simulations: usize,
    total_state_reads: u64,
    unique_state_reads: u64,
    duplicate_state_reads: u64,
    unkeyed_calls: u64,
    cache_hits: u64,
    cache_misses: u64,
    /// Which arm the rows in this fold were run under, counted rather than listed: a table
    /// over both arms is a table over two programs, and §16 asks for one.
    reuse_on: u64,
    reuse_off: u64,
    reuse_unreported: u64,
    fastest_call_ns: Option<u64>,
    slowest_call_ns: Option<u64>,
    methods: BTreeMap<String, MethodFold>,
}

/// One method's fold across the simulations of one source.
#[derive(Clone, Copy, Debug, Default)]
struct MethodFold {
    calls: u64,
    total_ns: u64,
    fastest_ns: Option<u64>,
    slowest_ns: Option<u64>,
    failed: u64,
    attempts: u64,
    retried: u64,
}

impl Acquisition {
    fn fold(rows: &[&Value]) -> Self {
        let mut out = Self::default();
        for row in rows {
            let rpc = &row["rpc"];
            out.simulations += 1;
            let calls = value_u64(rpc, "total_calls").unwrap_or(0);
            if calls > 0 {
                out.simulations_with_calls += 1;
            }
            out.calls = out.calls.saturating_add(calls);
            out.successful = out
                .successful
                .saturating_add(value_u64(rpc, "successful_calls").unwrap_or(0));
            out.failed = out
                .failed
                .saturating_add(value_u64(rpc, "failed_calls").unwrap_or(0));
            out.attempts = out
                .attempts
                .saturating_add(value_u64(rpc, "total_attempts").unwrap_or(0));
            out.retried = out
                .retried
                .saturating_add(value_u64(rpc, "retried_calls").unwrap_or(0));
            out.sum_ns = out
                .sum_ns
                .saturating_add(value_u64(rpc, "rpc_sum_duration_ns").unwrap_or(0));
            for (field, total) in [
                ("rpc_union_duration_ns", &mut out.union_ns),
                ("serial_wait_duration_ns", &mut out.serial_ns),
                ("rpc_overlap_duration_ns", &mut out.overlap_ns),
                ("non_rpc_duration_ns", &mut out.non_rpc_ns),
            ] {
                if let Some(value) = value_u64(rpc, field) {
                    *total = total.saturating_add(value);
                    if field == "non_rpc_duration_ns" {
                        out.non_rpc_measured = true;
                    }
                }
            }
            out.span_ns = out
                .span_ns
                .saturating_add(value_u64(row, "simulation_duration_ns").unwrap_or(0));
            out.max_concurrency = out
                .max_concurrency
                .max(value_u64(rpc, "max_concurrency").unwrap_or(0) as usize);
            match rpc["serial_or_overlap"].as_str() {
                Some("serial") => out.serial_simulations += 1,
                Some("overlap") => out.overlap_simulations += 1,
                _ => {}
            }
            let duplicates = &row["duplicates"];
            out.total_state_reads = out
                .total_state_reads
                .saturating_add(value_u64(duplicates, "total_state_reads").unwrap_or(0));
            out.unique_state_reads = out
                .unique_state_reads
                .saturating_add(value_u64(duplicates, "unique_state_reads").unwrap_or(0));
            out.duplicate_state_reads = out
                .duplicate_state_reads
                .saturating_add(value_u64(duplicates, "duplicate_state_reads").unwrap_or(0));
            out.unkeyed_calls = out
                .unkeyed_calls
                .saturating_add(value_u64(duplicates, "unkeyed_calls").unwrap_or(0));
            let cache = &row["state_read_cache"];
            match cache["reuse"].as_bool() {
                Some(reuse) => {
                    out.cache_hits = out
                        .cache_hits
                        .saturating_add(value_u64(cache, "cache_hits").unwrap_or(0));
                    out.cache_misses = out
                        .cache_misses
                        .saturating_add(value_u64(cache, "cache_misses").unwrap_or(0));
                    if reuse {
                        out.reuse_on += 1;
                    } else {
                        out.reuse_off += 1;
                    }
                }
                None => out.reuse_unreported += 1,
            }
            for method in row["methods"].as_array().into_iter().flatten() {
                let name = method["method"].as_str().unwrap_or("?").to_string();
                let fold = out.methods.entry(name).or_default();
                let count = value_u64(method, "count").unwrap_or(0);
                fold.calls = fold.calls.saturating_add(count);
                fold.total_ns = fold
                    .total_ns
                    .saturating_add(value_u64(method, "total_duration_ns").unwrap_or(0));
                fold.failed = fold
                    .failed
                    .saturating_add(value_u64(method, "failure_count").unwrap_or(0));
                fold.attempts = fold
                    .attempts
                    .saturating_add(value_u64(method, "total_attempts").unwrap_or(0));
                fold.retried = fold
                    .retried
                    .saturating_add(value_u64(method, "retried_calls").unwrap_or(0));
                if count > 0 {
                    let min = value_u64(method, "min_duration_ns");
                    let max = value_u64(method, "max_duration_ns");
                    fold.fastest_ns = merge_min(fold.fastest_ns, min);
                    fold.slowest_ns = merge_max(fold.slowest_ns, max);
                    out.fastest_call_ns = merge_min(out.fastest_call_ns, min);
                    out.slowest_call_ns = merge_max(out.slowest_call_ns, max);
                }
            }
        }
        out
    }

    /// The share of the measured span one quantity takes, or the reason it has none.
    fn span_share(&self, part_ns: Option<u64>, measured: bool) -> Value {
        if !measured {
            return json!({
                "per_mille": Value::Null,
                "reason": NO_SHARE_MEASURED,
            });
        }
        ratio_ns(part_ns.unwrap_or(0), self.span_ns)
    }

    /// A count's share of the calls recorded here, for the categories whose quantity is a
    /// number of asks rather than a duration.
    fn call_share(&self, part: u64, measured: bool) -> Value {
        if !measured {
            return json!({
                "per_mille": Value::Null,
                "reason": NO_SHARE_MEASURED,
            });
        }
        share(part as usize, self.calls as usize)
    }
}

/// The smaller of two measurements, where `None` means no measurement at all.
fn merge_min(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (None, candidate) => candidate,
        (current, None) => current,
    }
}

/// The larger of two measurements, on the same rule.
fn merge_max(current: Option<u64>, candidate: Option<u64>) -> Option<u64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (None, candidate) => candidate,
        (current, None) => current,
    }
}

/// §25's verdict for a share measured against this file's declared thresholds.
fn verdict_of(share: &Value, measured: bool) -> &'static str {
    if !measured {
        return NOT_MEASURED;
    }
    match share["per_mille"].as_u64() {
        Some(per_mille) if per_mille >= DOMINANT_PER_MILLE => DOMINANT,
        Some(per_mille) if per_mille >= MATERIAL_PER_MILLE => MATERIAL,
        Some(_) => RULED_OUT,
        None => NOT_MEASURED,
    }
}

/// §19's ninth file: the A–G classification, per source, with the rule beside its number.
///
/// The order is deliberate: figures first, then the threshold applied to them, then the word.
/// §21 forbids a report that decides and then looks, and a reader who disagrees with 500‰ can
/// take the integers in `figures` and apply their own line without opening a second file.
///
/// Sources are classified separately and nothing here adds two of them together: a fixture's
/// span is local compute by construction and a live run's is a node's answer, so one table
/// over both would rank a difference in instrumentation as a difference in cost.
pub fn bottleneck_classification(rows: &[Value], outside: Option<&Value>) -> Value {
    let mut by_source: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        let source = row["source"].as_str().unwrap_or("?");
        by_source.entry(source).or_default().push(row);
    }
    let per_source: Vec<Value> = by_source
        .iter()
        .map(|(source, group)| source_classification(source, group))
        .collect();
    let reuse_on = rows
        .iter()
        .filter(|row| row["state_read_cache"]["reuse"].as_bool() == Some(true))
        .count();
    let reuse_off = rows
        .iter()
        .filter(|row| row["state_read_cache"]["reuse"].as_bool() == Some(false))
        .count();
    json!({
        "unit": "ns",
        "simulations": rows.len(),
        "sources": per_source.iter().filter_map(|row| row["source"].as_str()).collect::<Vec<_>>(),
        "per_source": per_source,
        "categories_are_the_seven": BOTTLENECK_CATEGORIES.iter().map(|(id, name)| json!({
            "id": id, "name": name,
        })).collect::<Vec<_>>(),
        "thresholds_are_declared": {
            "dominant_per_mille": DOMINANT_PER_MILLE,
            "material_per_mille": MATERIAL_PER_MILLE,
            "latency_floor_ns": LATENCY_FLOOR_NS,
            "note": "these three lines are this file's declarations, not measurements. Every \
                     category publishes the integers its rule read beside its verdict, so a \
                     reader who wants a different line can re-rank the same figures",
        },
        "state_read_reuse": {
            "rows_with_reuse_on": reuse_on,
            "rows_with_reuse_off": reuse_off,
            "rows_with_no_boundary_tally": rows.len().saturating_sub(reuse_on + reuse_off),
            "mixed_arms": reuse_on > 0 && reuse_off > 0,
            "rule": "§16 asks for runs with reuse on. A directory holding both arms would be \
                     classifying two programs at once, and says so here rather than blending \
                     them",
        },
        "sink_disjointness_check": sink_check(outside),
        "sources_are_never_blended": true,
        "shares_are_not_additive": "A, B and C are three measurements over overlapping \
                                   intervals, not three slices of the span: when every call ran \
                                   alone (max_concurrency 1) their quantities are the same \
                                   nanoseconds seen three ways, so their per-mille figures do not \
                                   sum to 1000 and adding them names no quantity at all. A \
                                   category's share answers 「how much of the span does this \
                                   explanation cover」, and more than one explanation can cover \
                                   the same minute.",
        "no_float": "every ratio in this file is per-mille integers over two measured figures",
        "diagnosis_only": "§18: this file names where the time went and proposes nothing. Batch \
                           RPC, concurrency, prefetch, connection tuning and any cache change \
                           are not implemented in this build — see the `candidate` fields in \
                           the completion report",
    })
}

/// §14's no-double-counting claim, checked against the file that would show it broken.
///
/// A call belongs to exactly one sink because `with_rpc_trace` replaces the sink on the clone
/// it derives; `simulation-state` in the lifecycle table is where a leak would appear. A
/// non-zero count there is a regression in the instrumentation, not a finding about the chain.
fn sink_check(outside: Option<&Value>) -> Value {
    let Some(table) = outside else {
        return json!({
            "lifecycle_table_present": false,
            "simulation_state_calls": Value::Null,
            "reason": "this run did not attach the lifecycle sink (§17's switch), so there is \
                       no second list to check the first one against",
        });
    };
    let calls = table["per_class"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["class"].as_str() == Some(LIFECYCLE_SIMULATION_STATE))
        .and_then(|row| value_u64(row, "calls"));
    json!({
        "lifecycle_table_present": true,
        "simulation_state_calls": calls,
        "expected": 0,
        "regression": calls.unwrap_or(0) > 0,
        "rule": "a call recorded by the lifecycle sink is not recorded by the simulation's \
                 sink. If `simulation-state` ever holds a call, the two sinks agree about \
                 nothing and the 39-call baseline is not the number this file reports",
    })
}

/// One source's seven verdicts, its primary and secondary, and the method table they came
/// out of.
fn source_classification(source: &str, rows: &[&Value]) -> Value {
    let f = Acquisition::fold(rows);
    let has_calls = f.calls > 0;
    // Whether this source's table has any denominator to classify against at all. A source
    // with calls but no window gives every share below a null, and G then means "insufficient
    // evidence" rather than "no cost" — which is a different sentence and has to print as one.
    let measured_span = has_calls && f.span_ns > 0;
    // A: what this many calls would cost at the cheapest single call in the evidence.
    let volume_floor_ns = f.fastest_call_ns.map(|floor| f.calls.saturating_mul(floor));
    let a_share = f.span_share(volume_floor_ns, has_calls && f.fastest_call_ns.is_some());
    // B: the time a call was outstanding, held to the declared per-call floor.
    let b_share = f.span_share(Some(f.union_ns), has_calls);
    let b_verdict = match f.fastest_call_ns {
        // No call was timed here, so there is no per-call cost to compare with the floor. That
        // is not a pass: §21 forbids reading an absent measurement as a ruled-out category.
        None => NOT_MEASURED,
        Some(floor) if floor < LATENCY_FLOOR_NS => RULED_OUT,
        Some(_) => verdict_of(&b_share, true),
    };
    // C: the part of the span where exactly one call was in flight, which is the wait a
    // serial chain imposes and nothing else can.
    let c_share = f.span_share(Some(f.serial_ns), has_calls);
    let c_verdict = if !has_calls {
        NOT_MEASURED
    } else if f.max_concurrency > 1
        && ratio_ns(f.overlap_ns, f.union_ns)["per_mille"].as_u64() >= Some(DOMINANT_PER_MILLE)
    {
        RULED_OUT
    } else {
        verdict_of(&c_share, true)
    };
    // D: the repeats. Counted over asks, not over time — §19's note makes this the category
    // M8.3.1 was supposed to have ended, so its headline figure is a count.
    let d_share = share(
        f.duplicate_state_reads as usize,
        f.total_state_reads as usize,
    );
    let d_verdict = if f.total_state_reads == 0 {
        NOT_MEASURED
    } else if f.duplicate_state_reads == 0 {
        RULED_OUT
    } else {
        verdict_of(&d_share, true)
    };
    // E: everything a connection, not a node, would show — failures and re-tries.
    let extra_attempts = f.attempts.saturating_sub(f.calls);
    let e_share = f.call_share(f.failed.saturating_add(extra_attempts), has_calls);
    let e_verdict = if !has_calls {
        NOT_MEASURED
    } else if f.failed == 0 && extra_attempts == 0 {
        RULED_OUT
    } else {
        verdict_of(&e_share, true)
    };
    // F: what the simulation did while no call was in flight, on the union basis §13 requires.
    let f_share = f.span_share(Some(f.non_rpc_ns), f.non_rpc_measured);
    let categories = vec![
        json!({
            "id": "A",
            "name": BOTTLENECK_CATEGORIES[0].1,
            "verdict": verdict_of(&a_share, has_calls && f.fastest_call_ns.is_some()),
            "quantity": "calls × fastest single call",
            "denominator": "simulation_span_ns",
            "share_per_mille": a_share,
            "figures": {
                "calls": f.calls,
                "fastest_call_ns": f.fastest_call_ns,
                "volume_floor_ns": volume_floor_ns,
                "measured_rpc_sum_ns": f.sum_ns,
                "rpc_sum_over_span_per_mille": f.span_share(Some(f.sum_ns), has_calls),
            },
            "rule": format!("A is the part only removing calls can remove: \
                             {DOMINANT_PER_MILLE}‰ or more of the span is demanded by this many \
                             calls even priced at the cheapest single call this evidence holds. \
                             It is a lower bound, never an estimate of a saving"),
        }),
        json!({
            "id": "B",
            "name": BOTTLENECK_CATEGORIES[1].1,
            "verdict": b_verdict,
            "quantity": "rpc_union_duration_ns over the span, guarded by the per-call floor",
            "denominator": "simulation_span_ns",
            "share_per_mille": b_share,
            "figures": {
                "union_ns": f.union_ns,
                "fastest_call_ns": f.fastest_call_ns,
                "slowest_call_ns": f.slowest_call_ns,
                "latency_floor_ns": LATENCY_FLOOR_NS,
                "mean_call_ns": integer_mean(f.sum_ns, f.calls as usize),
                "per_method_fastest_and_slowest": "in rpc-summary.json's per_method rows, which \
                                                   carry each method's own distribution",
            },
            "rule": format!("B is ruled out when the fastest call in the evidence costs less \
                             than the declared floor of {LATENCY_FLOOR_NS} ns (§15: slow is not \
                             the same as provider-slow, so a run whose every call is cheap is \
                             not latency-bound however long its span is). Otherwise B's position \
                             is its share of the span on the usual {DOMINANT_PER_MILLE}‰ / \
                             {MATERIAL_PER_MILLE}‰ lines"),
        }),
        json!({
            "id": "C",
            "name": BOTTLENECK_CATEGORIES[2].1,
            "verdict": c_verdict,
            "quantity": "serial_wait_duration_ns (exactly one call in flight) over the span",
            "denominator": "simulation_span_ns",
            "share_per_mille": c_share,
            "figures": {
                "serial_ns": f.serial_ns,
                "overlap_ns": f.overlap_ns,
                "max_concurrency": f.max_concurrency,
                "serial_simulations": f.serial_simulations,
                "overlap_simulations": f.overlap_simulations,
                "simulations_with_calls": f.simulations_with_calls,
            },
            "rule": format!("C is the part an overlap could have hidden and did not. It is ruled \
                             out where two or more calls were in flight for {DOMINANT_PER_MILLE}‰ \
                             or more of the span; elsewhere its share is read on the usual lines. \
                             A `dominant` C is not a separate cost — it is why A and B cannot be \
                             paid for in parallel"),
        }),
        json!({
            "id": "D",
            "name": BOTTLENECK_CATEGORIES[3].1,
            "verdict": d_verdict,
            "quantity": "duplicate_state_reads over keyed state reads",
            "denominator": "total_state_reads",
            "share_per_mille": d_share,
            "figures": {
                "total_state_reads": f.total_state_reads,
                "unique_state_reads": f.unique_state_reads,
                "duplicate_state_reads": f.duplicate_state_reads,
                "unkeyed_calls": f.unkeyed_calls,
                "cache_hits": f.cache_hits,
                "cache_misses": f.cache_misses,
            },
            "rule": "D is the avoidable asks: one identity, same pinned block, asked again. §19 \
                     expects this to be ruled out since M8.3.1 — a non-zero count here is read as \
                     a regression in reuse, or as an arm that was left with reuse off, and the \
                     `state_read_reuse` field beside it says which",
            "m8_3_1_regression": f.duplicate_state_reads > 0,
        }),
        json!({
            "id": "E",
            "name": BOTTLENECK_CATEGORIES[4].1,
            "verdict": e_verdict,
            "quantity": "failed calls plus attempts beyond one per call, over calls",
            "denominator": "recorded_calls",
            "share_per_mille": e_share,
            "figures": {
                "calls": f.calls,
                "attempts": f.attempts,
                "extra_attempts": extra_attempts,
                "failed_calls": f.failed,
                "retried_calls": f.retried,
            },
            "rule": "E is ruled out by measurement when every call got one attempt and every \
                     attempt succeeded: that says the time was paid to a node answering, not to \
                     a connection failing. A non-zero figure here makes B's durations unreadable \
                     as node cost, because a timeout and a slow answer are one number at the wire",
        }),
        json!({
            "id": "F",
            "name": BOTTLENECK_CATEGORIES[5].1,
            "verdict": verdict_of(&f_share, f.non_rpc_measured),
            "quantity": "non_rpc_duration_ns (span minus the interval union) over the span",
            "denominator": "simulation_span_ns",
            "share_per_mille": f_share,
            "figures": {
                "non_rpc_ns": if f.non_rpc_measured { json!(f.non_rpc_ns) } else { Value::Null },
                "span_ns": f.span_ns,
                "union_ns": f.union_ns,
                "gap_in_window_ns": "in rpc-gaps.json: the idle stretches *between* calls are \
                                     inside the union's window and are not local compute",
            },
            "rule": format!("F is what the EVM and the state conversion cost. Its figure is \
                             span minus union, never span minus sum (§13), and it is ruled out \
                             when that comes back under {MATERIAL_PER_MILLE}‰ of the span"),
        }),
    ];
    // G has no figure of its own: it is what `primary` becomes when the six above do not
    // settle the question between them.
    let mut ranked: Vec<(&Value, u64)> = categories
        .iter()
        .filter(|row| row["verdict"] == json!(DOMINANT))
        .filter(|row| row["denominator"] == json!("simulation_span_ns"))
        .filter_map(|row| {
            value_u64(&row["share_per_mille"], "per_mille").map(|per_mille| (row, per_mille))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0["id"].as_str().cmp(&b.0["id"].as_str()))
    });
    let span_dominant: Vec<String> = ranked.iter().map(|(row, _)| id_of(row)).collect();
    let d_dominant = d_verdict == DOMINANT;
    let any_material = categories
        .iter()
        .any(|row| row["verdict"] == json!(MATERIAL));
    let (primary, rationale) = match (span_dominant.len(), d_dominant) {
        (1, false) => (
            ranked[0].0["id"].as_str().unwrap_or("G").to_string(),
            format!("one category reaches {DOMINANT_PER_MILLE}‰ of the measured span by itself"),
        ),
        (0, true) => (
            "D".to_string(),
            "no span-denominated category reaches the line, and half or more of the asks were \
             repeats: that is the avoidable half of the time, which is why §25 lets a count \
             outrank a duration"
                .to_string(),
        ),
        (n, d) if n > 1 || d => (
            "G".to_string(),
            format!(
                "{n} span-denominated categor{} each reach {DOMINANT_PER_MILLE}‰ of the span{}, \
                 so no single one of them is where the time went; §25's G is the honest word \
                 for that and its components are listed as secondary",
                if n == 1 { "y" } else { "ies" },
                if d { ", and so do the repeats" } else { "" }
            ),
        ),
        _ => (
            "G".to_string(),
            if !measured_span {
                "no category has a measured quantity over the span, so this table reports \
                 insufficient evidence rather than an absence of cost"
                    .to_string()
            } else if any_material {
                format!(
                    "several categories are material (at least {MATERIAL_PER_MILLE}‰) and none \
                     is dominant: mixed"
                )
            } else {
                format!(
                    "nothing reaches {MATERIAL_PER_MILLE}‰ of the span, which this build cannot \
                     explain by naming a bound"
                )
            },
        ),
    };
    let mut secondary: Vec<String> = categories
        .iter()
        .filter(|row| {
            row["id"] != json!(primary.as_str())
                && (row["verdict"] == json!(DOMINANT) || row["verdict"] == json!(MATERIAL))
        })
        .map(id_of)
        .collect();
    secondary.sort();
    let mut rows_out = categories;
    rows_out.push(json!({
        "id": "G",
        "name": BOTTLENECK_CATEGORIES[6].1,
        "verdict": if primary == "G" { DOMINANT } else { NOT_MEASURED },
        "quantity": "no figure of its own: it is the verdict the six above add up to",
        "denominator": "simulation_span_ns",
        "share_per_mille": Value::Null,
        "rule": "G is chosen when two or more categories are each dominant (mixed) or when none \
                 has a measured quantity at all (insufficient evidence). It is never a fallback \
                 for a rule nobody liked",
    }));
    // §5's table is already sorted by summed duration, so the expensive method is that table's
    // first row rather than a second maximum over the same map: one ordering, read once.
    let ranking = method_ranking(&f);
    let most_time = ranking
        .as_array()
        .and_then(|rows| rows.first())
        .map(|row| {
            json!({
                "method": row["method"],
                "calls": row["calls"],
                "total_duration_ns": row["total_duration_ns"],
                "field": "sum of this method's recorded call durations",
                "not_wall_time": "a sum double-counts an instant two calls shared, so this \
                                  names the expensive method rather than a slice of the wall \
                                  clock. Read `rpc_union_duration_ns` for the wall figure",
                "method_ranking_field": "same integers as this row's method, calls and \
                                        total_duration_ns, in `method_ranking_by_summed_duration`",
            })
        })
        .unwrap_or(Value::Null);
    json!({
        "source": source,
        "simulations": f.simulations,
        "simulations_with_calls": f.simulations_with_calls,
        "primary": primary,
        "primary_name": BOTTLENECK_CATEGORIES
            .iter()
            .find(|(id, _)| *id == primary.as_str())
            .map(|(_, name)| name.to_string())
            .unwrap_or_default(),
        "primary_rationale": rationale,
        "secondary": secondary,
        "dominant_span_categories": span_dominant,
        "method_ranking_by_summed_duration": ranking,
        "most_time_by_summed_call_duration": most_time,
        "aggregated_figures": {
            "calls": f.calls,
            "successful_calls": f.successful,
            "failed_calls": f.failed,
            "attempts": f.attempts,
            "retried_calls": f.retried,
            "rpc_sum_duration_ns": f.sum_ns,
            "rpc_union_duration_ns": f.union_ns,
            "serial_wait_duration_ns": f.serial_ns,
            "rpc_overlap_duration_ns": f.overlap_ns,
            "non_rpc_duration_ns": f.non_rpc_ns,
            "non_rpc_measured": f.non_rpc_measured,
            "simulation_span_ns": f.span_ns,
            "rpc_union_over_span_per_mille": f.span_share(Some(f.union_ns), has_calls),
            "max_concurrency": f.max_concurrency,
            "serial_simulations": f.serial_simulations,
            "overlap_simulations": f.overlap_simulations,
            "state_reads": {
                "total": f.total_state_reads,
                "unique": f.unique_state_reads,
                "duplicate": f.duplicate_state_reads,
                "unkeyed": f.unkeyed_calls,
            },
            "cache_hits": f.cache_hits,
            "cache_misses": f.cache_misses,
        },
        "categories": rows_out,
    })
}

/// One category row's id, for the lists that name categories rather than describe them.
fn id_of(row: &Value) -> String {
    row["id"].as_str().unwrap_or("?").to_string()
}

/// §5's method table, in cost order: the answer to 「谁最耗时」 with its denominator stated.
fn method_ranking(fold: &Acquisition) -> Value {
    let mut rows: Vec<(&String, &MethodFold)> = fold.methods.iter().collect();
    rows.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns).then_with(|| a.0.cmp(b.0)));
    Value::Array(
        rows.iter()
            .map(|(name, row)| {
                json!({
                    "method": name.as_str(),
                    "calls": row.calls,
                    "total_duration_ns": row.total_ns,
                    "fastest_call_ns": row.fastest_ns,
                    "slowest_call_ns": row.slowest_ns,
                    "mean_call_ns": integer_mean(row.total_ns, row.calls as usize),
                    "failed_calls": row.failed,
                    "attempts": row.attempts,
                    "retried_calls": row.retried,
                    "share_of_rpc_sum_per_mille": share(
                        row.total_ns as usize,
                        fold.sum_ns as usize,
                    ),
                    "share_of_span_per_mille": ratio_ns(row.total_ns, fold.span_ns),
                    "percentiles": "this method's own distribution is in rpc-summary.json's \
                                    per_method rows, merged over every simulation of this source",
                })
            })
            .collect(),
    )
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
/// Returns `(union, serial, overlap, max_concurrency, gap_widths)`.
///
/// `gap_widths` is §12's per-gap measurement: the width of each *maximal* stretch inside
/// the call window with nothing in flight. Consecutive zero-in-flight segments are merged
/// into one entry (a single call that never opened its interval, or two calls separated by
/// an instant of coverage, is one wait, not two), and the list holds only positive widths,
/// so `gap_count` is a count of real waits rather than of sweep segments. A window whose
/// calls all touch end to end yields an empty list, which is the measurement that says
/// there was no idle time inside it — not a missing figure.
fn scan(spans: &[Span]) -> (u64, u64, u64, usize, Vec<u64>) {
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
    let mut pending_gap = 0_u64;
    let mut gaps: Vec<u64> = Vec::new();
    let mut index = 0;
    while index < points.len() {
        let at = points[index].0;
        if let Some(before) = previous {
            let width = at.saturating_sub(before);
            match in_flight {
                // Nothing in flight: this is a `gap` (§12), accumulated so the stretch is
                // measured rather than the segment. Time before the first call and after
                // the last one is *not* here — the sweep starts at the first call and ends
                // at the last, so this is idle time inside the call window, exactly the
                // quantity `wall − union` reports the total of.
                0 => pending_gap = pending_gap.saturating_add(width),
                1 => {
                    union = union.saturating_add(width);
                    serial = serial.saturating_add(width);
                    if pending_gap > 0 {
                        gaps.push(pending_gap);
                        pending_gap = 0;
                    }
                }
                n => {
                    union = union.saturating_add(width);
                    // A stretch with two calls in flight is one stretch of covered time
                    // and two stretches of waiting: the difference is what concurrency
                    // saved, which is `overlap` by §9's definition (`sum − union`).
                    overlap =
                        overlap.saturating_add(width.saturating_mul(n.saturating_sub(1) as u64));
                    // Not serial wait — nothing was waiting alone here.
                    if pending_gap > 0 {
                        gaps.push(pending_gap);
                        pending_gap = 0;
                    }
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
    // The sweep can only end with a pending stretch if the last segment had nothing in
    // flight, which needs a trailing zero-width span collapsed onto the final instant;
    // such a span covers no time, so its wait is real and gets reported.
    if pending_gap > 0 {
        gaps.push(pending_gap);
    }
    (union, serial, overlap, max_in_flight, gaps)
}

/// §9's and §11's timeline for one simulation.
pub fn timeline(window: &SimulationWindow, events: &[RpcCallEvent]) -> RpcTimeline {
    let mut out = RpcTimeline {
        total_calls: events.len(),
        simulation_duration_ns: window.duration_ns(),
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
    let (union, serial, overlap, max_concurrency, gaps) = scan(&spans);
    out.wall_duration_ns = Some(wall);
    out.union_duration_ns = Some(union);
    out.overlap_duration_ns = Some(overlap);
    out.serial_wait_duration_ns = Some(serial);
    out.gap_duration_ns = Some(wall.saturating_sub(union));
    out.gap_count = Some(gaps.len());
    out.gap_widths_ns = gaps;
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
    /// §14's lifecycle half: the rows the run's *other* sink recorded, the stage spans that
    /// classify them, and the digest of the endpoint they went to.
    ///
    /// `None` until a run with §17's switch attaches a lifecycle sink — and a run without one
    /// writes no [`OUTSIDE_FILE`]. That is M8.2's file set unchanged rather than a directory
    /// missing a §19 name: the switch is what asks for this half, so a directory written by a
    /// build or a command line that never asked for it is not holding a §14 answer yet.
    lifecycle: Option<LifecycleHalf>,
    /// §19's assembly provenance: the runs this directory was folded together from, when it
    /// was not one run. `None` for a directory a single run wrote.
    assembled_from: Option<Value>,
    /// §17's switch, kept so `finish` knows whether §19's acquisition tables were asked for.
    ///
    /// The four files it gates are regroupings of the trace lines every run already writes, so
    /// turning it on changes which files exist and not what any of them says: with the flag off
    /// the directory holds M8.2's five names, byte for byte as they were before M8.3.2.
    state_acquisition: bool,
    /// One [`duration_row`] per recorded simulation, in the order they finished: the rows
    /// [`rpc_gaps`] and [`bottleneck_classification`] group over.
    duration_rows: Vec<Value>,
    /// Every simulation's `storage` list, concatenated in the same order — §6's rows.
    storage_rows: Vec<Value>,
    /// Every simulation's `account_reads` list — §9's rows.
    account_rows: Vec<Value>,
}

/// What §14's sweep found, kept until `finish` turns it into a file.
enum LifecycleHalf {
    /// One run's own rows, classified against that run's stage spans. The table is built at
    /// `finish` rather than at `record_lifecycle` because it also names how many calls the
    /// simulations recorded *elsewhere*, and that total is only known once they are all in.
    Run {
        rows: Vec<Value>,
        spans: Vec<StageSpan>,
        endpoint_id: Option<String>,
    },
    /// A table over several runs' rows — [`DiagnosisEvidence::attach_outside`]'s path, for an
    /// assembled directory. Already aggregated, so `finish` only puts the directory's header
    /// on it.
    Assembled(Value),
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
    /// §12's widths, folded across this source's simulations: one entry per idle stretch,
    /// so the distribution is over gaps rather than over simulations. `gap_wide_simulations`
    /// beside it says how many of the runs produced any, so a list of ten gaps from one run
    /// is not read as ten runs' worth.
    gap_widths_ns: Vec<u64>,
    gap_count_total: u64,
    gap_wide_simulations: u64,
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
            gap_widths_ns: Vec::new(),
            gap_count_total: 0,
            gap_wide_simulations: 0,
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
        // §12 across the source: every idle stretch this source's runs left is one sample
        // here, so the distribution is over gaps rather than over simulations.
        if !timeline.gap_widths_ns.is_empty() {
            self.gap_wide_simulations += 1;
        }
        self.gap_count_total += timeline.gap_count.unwrap_or(0) as u64;
        for width in &timeline.gap_widths_ns {
            push_sample(&mut self.gap_widths_ns, *width);
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
            // §12: the widths themselves, one sample per idle stretch, so `gap_min`,
            // `gap_median` and `gap_max` are about gaps rather than about runs.
            "gap_count_total": self.gap_count_total,
            "gap_wide_simulations": self.gap_wide_simulations,
            "gap_width_distribution_ns": stats(&self.gap_widths_ns),
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
    ///
    /// `state_acquisition` is §17's switch: it decides only which files [`Self::finish`]
    /// writes. Nothing read here changes a call, a duration, or a count.
    pub fn open(
        dir: &Path,
        git_revision: &str,
        execution_mode: &str,
        state_acquisition: bool,
    ) -> Result<Self> {
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
            lifecycle: None,
            assembled_from: None,
            state_acquisition,
            duration_rows: Vec::new(),
            storage_rows: Vec::new(),
            account_rows: Vec::new(),
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
        if self.state_acquisition {
            // Copied out of the line about to be written, not re-derived: §19's tables are
            // therefore the same bytes a reader gets by parsing `simulation-traces.jsonl` and
            // calling the same functions over it.
            self.duration_rows.push(duration_row(&line));
            if let Some(rows) = line["storage"].as_array() {
                self.storage_rows.extend(rows.iter().cloned());
            }
            if let Some(rows) = line["account_reads"].as_array() {
                self.account_rows.extend(rows.iter().cloned());
            }
        }
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

    /// §14's other half, kept until [`Self::finish`] turns it into a file.
    ///
    /// The calls here are the run's, not a simulation's: they were recorded by the sink on
    /// the handle the observation, detection and preflight reads went through, which is a
    /// different adapter from the one the provider reads through — and `with_rpc_trace`
    /// *replaces* the sink on the clone it derives, so a call is in one list or the other,
    /// never twice. The spans come from the latency trace the run already wrote; nothing
    /// here takes a clock reading to make a classification.
    pub fn record_lifecycle(
        &mut self,
        events: &[RpcCallEvent],
        spans: &[StageSpan],
        endpoint_id: Option<&str>,
    ) {
        self.lifecycle = Some(LifecycleHalf::Run {
            rows: lifecycle_rows(events, spans, endpoint_id),
            spans: spans.to_vec(),
            endpoint_id: endpoint_id.map(str::to_string),
        });
    }

    /// §14's other half for a directory assembled from several runs: the table is already
    /// built ([`outside_simulation_table_assembled`] pooled them), so this only hands it to
    /// `finish`, which puts the directory's header on it like every other file here.
    ///
    /// The single-run path classifies events against spans of its own; an assembly cannot do
    /// that across runs, because each run stamps against its own monotonic origin. Pooling
    /// the *rows* is sound precisely because the classification was already done per run, on
    /// that run's clock, before anything was merged.
    pub fn attach_outside(&mut self, table: Value) {
        self.lifecycle = Some(LifecycleHalf::Assembled(table));
    }

    /// Describe this directory as a folding together of runs that already exist.
    ///
    /// Two things follow, and both are what makes §19's merged directory a gate rather than
    /// a convenience: every file here carries the header naming its source runs, so a figure
    /// in a merged table is traceable to the run that measured it; and the one wall-clock
    /// stamp is the sources' rather than the moment of assembly, so assembling the same runs
    /// twice writes byte-identical files. An assembly-time timestamp would make that gate
    /// unrunnable, and no duration in these files is computed from the stamp anyway (§7).
    pub fn assemble_from(&mut self, generated_at_unix_ms: u64, runs: Vec<Value>) {
        self.generated_at_unix_ms = generated_at_unix_ms;
        self.assembled_from = Some(Value::Array(runs));
    }

    /// Fold one already-written trace line into this directory.
    ///
    /// §19's merged evidence is assembled this way: each run's lines are replayed through the
    /// same writer that produced them, so every table in the merged directory is the same
    /// function of the same recorded calls as the tables in the per-run ones. The alternative
    /// is a second set of aggregations, correct only while nobody changes the first.
    ///
    /// A line this build cannot read fails the assembly rather than skipping it: a directory
    /// missing a simulation would report a smaller run than happened, and the missing line is
    /// the one thing a reader could not see.
    pub fn replay(&mut self, line: &Value) -> Result<()> {
        let diagnosis = SimulationDiagnosis::from_trace_line(line).map_err(|reason| {
            PipelineError::Evidence {
                path: self.dir.join(TRACES_FILE),
                detail: format!("the trace line could not be replayed: {reason}"),
            }
        })?;
        let refusals = trace_line_refusals(line);
        self.record(diagnosis, &refusals)
    }

    /// Write the summaries and the README. Returns the directory, which is what the
    /// run's own record names so a reader can find the traces.
    pub fn finish(&mut self) -> Result<PathBuf> {
        let mut metadata = json!({
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
        // §19's assembly provenance, in the header every file here carries rather than in one
        // file a reader has to know to open: a merged directory has to say which runs its
        // figures came from before any figure in it can be checked against a run.
        if let Some(runs) = self.assembled_from.clone() {
            if let Some(object) = metadata.as_object_mut() {
                object.insert("assembled_from".to_string(), runs);
            }
        }
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

        // §14's file, and only for a run that asked for this half: a directory written by a
        // command line without §17's switch keeps M8.2's five names rather than growing a
        // sixth that holds an empty table nobody asked for.
        let lifecycle = self.lifecycle.take();
        let outside = lifecycle.map(|half| match half {
            LifecycleHalf::Run {
                rows,
                spans,
                endpoint_id,
            } => {
                let simulation_calls: u64 = self.buckets.values().map(|bucket| bucket.calls).sum();
                let table = outside_simulation_table(
                    &rows,
                    &spans,
                    endpoint_id.as_deref(),
                    simulation_calls,
                );
                with_metadata(table, &metadata)
            }
            LifecycleHalf::Assembled(table) => with_metadata(table, &metadata),
        });
        if let Some(table) = outside.as_ref() {
            self.write_whole(OUTSIDE_FILE, table)?;
        }

        // §19's four acquisition tables, and only under §17's switch. Each is a grouping over
        // the lines this directory already holds — no new clock reading, no new call, and no
        // figure that is not also in `simulation-traces.jsonl` — so a directory written without
        // the switch keeps M8.2's five names unchanged rather than growing a ninth.
        let bottleneck = if self.state_acquisition {
            let storage_table = with_metadata(storage_breakdown(&self.storage_rows), &metadata);
            let account_table = with_metadata(account_matrix(&self.account_rows), &metadata);
            let gaps_table = with_metadata(rpc_gaps(&self.duration_rows), &metadata);
            let classification = with_metadata(
                bottleneck_classification(&self.duration_rows, outside.as_ref()),
                &metadata,
            );
            for (name, table) in [
                (STORAGE_BREAKDOWN_FILE, &storage_table),
                (ACCOUNT_MATRIX_FILE, &account_table),
                (RPC_GAPS_FILE, &gaps_table),
                (BOTTLENECK_FILE, &classification),
            ] {
                self.write_whole(name, table)?;
            }
            Some(classification)
        } else {
            None
        };

        std::fs::write(
            self.dir.join(README_FILE),
            readme(
                &sources,
                &rpc_summary,
                self.simulations,
                outside.as_ref(),
                bottleneck.as_ref(),
            ),
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

/// Put the directory's shared header into one table.
///
/// The header is what makes a file in this directory self-describing on its own — the schema
/// version, the revision that wrote it, the clock, the sample policy — and it is the same
/// object for every file, so a reader who has one file has the rules for all of them. The
/// header's keys win where they agree with a table's own: `unit` is `"ns"` in both, and a file
/// that disagreed with its own directory would be a bug to find in the header, not a per-file
/// variation to preserve.
fn with_metadata(mut table: Value, metadata: &Value) -> Value {
    if let (Some(object), Some(header)) = (table.as_object_mut(), metadata.as_object()) {
        // §19's run list is the one header key a table can also carry, and the two copies say
        // different things: the directory's names each run and what it contributed, while
        // §14's pooled table additionally keeps each run's own stage spans, which nothing else
        // in the directory can hold. Neither list is a duplicate of the other to overwrite, so
        // they are merged per run — a reader of the lifecycle file gets both halves of the
        // provenance in one place.
        let merged =
            merge_assembled_from(object.get("assembled_from"), header.get("assembled_from"));
        for (key, value) in header {
            object.insert(key.clone(), value.clone());
        }
        if let Some(merged) = merged {
            object.insert("assembled_from".to_string(), merged);
        }
    }
    table
}

/// Union of the directory's run list and a table's own, matched on `run`, keeping the fields of
/// both. Returns `None` when the table carries no list of its own, which leaves the header's.
fn merge_assembled_from(table_runs: Option<&Value>, header_runs: Option<&Value>) -> Option<Value> {
    let own = table_runs?.as_array()?.clone();
    let header = header_runs
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut merged: Vec<Value> = Vec::new();
    for entry in &own {
        let name = entry.get("run").cloned();
        let mut row = header
            .iter()
            .find(|row| name.is_some() && row.get("run") == name.as_ref())
            .cloned()
            .unwrap_or_else(|| json!({}));
        if let (Some(object), Some(own_object)) = (row.as_object_mut(), entry.as_object()) {
            for (key, value) in own_object {
                object.insert(key.clone(), value.clone());
            }
        }
        merged.push(row);
    }
    // A run the directory lists but this table has nothing for is still a run this file came
    // from, so it keeps its row rather than quietly disappearing from one file's provenance.
    for row in header {
        let name = row.get("run").cloned();
        if !merged
            .iter()
            .any(|entry| name.is_some() && entry.get("run") == name.as_ref())
        {
            merged.push(row);
        }
    }
    Some(Value::Array(merged))
}

/// §23's README: what the numbers are, how they were taken, and what they cannot say.
fn readme(
    sources: &[Value],
    summary: &Value,
    simulations: usize,
    outside: Option<&Value>,
    bottleneck: Option<&Value>,
) -> String {
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
           (§17). What separates them here is the attempt list, not a sub-timer.\n",
    );
    limitations.push_str(match outside {
        Some(_) => {
            "- Detection-stage reads (the reserves priced before a simulation is built) \
           and the gate's own reads are **not** in these traces: the sink here is attached to \
           the adapter the simulation itself reads through, so a call in a trace line is a \
           call that simulation made. The rest of the lifecycle is reported beside them, in \
           `{OUTSIDE_FILE}`, and classified by the stage that held it.\n"
        }
        None => {
            "- Detection-stage reads (the reserves priced before a simulation is built) \
           and the gate's own reads are not in these traces: the sink is attached to the \
           adapter the simulation itself reads through, so a call in this directory is a call \
           that simulation made. This run did not ask for the lifecycle half \n\
           (`{OUTSIDE_FILE}` is written only when §17's switch is on), so nothing here says \
           what those other reads cost.\n"
        }
    });
    limitations.push_str(
        "- Submission, receipt and header reads on other transports — the WebSocket client's \
           own request path, and everything `crates/execution` sends through its submitter — \
           are outside what this adapter sees, and are named here rather than counted as zero.\n\
         - A source whose adapter cannot hand out a traced clone reports no sink at all. That \
           is recorded as `diagnosis_refusals`, not as a simulation with zero calls.\n",
    );
    lines.push(limitations);
    if let Some(outside) = outside {
        let classes: Vec<String> = outside["per_class"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        format!(
                            "{}={}",
                            row["class"].as_str().unwrap_or("?"),
                            row["calls"].as_u64().unwrap_or(0)
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        lines.push(format!(
            "## Outside-simulation RPC\n\n\
             `{OUTSIDE_FILE}` counts the lifecycle's other reads and keeps them out of the \
             39-call state-read baseline, which is §14's rule: a saving on the gate's reads \
             and a saving on state acquisition are different findings, and adding one to the \
             other would make the first look like the second. A call is classified by the \
             stage span that holds its `started_ns` — the latency trace's own stamps, on the \
             same monotonic origin, so containment is a comparison rather than a second \
             measurement. This run: {}.\n\n\
             Three reads no sink of this build can see are listed in the file as \
             `unreachable_reads` instead of being counted as zero: `eth_chainId` inside \
             `HttpChainAdapter::connect` (it is the call that produces the adapter a sink \
             could be attached to), the execution lane's own adapter, and the WebSocket \
             transport. A span written from millisecond stamps holds nothing here: it is \
             marked unusable rather than widened to fit a nanosecond call.\n",
            classes.join(", ")
        ));
    }
    if let Some(classification) = bottleneck {
        let per_source: Vec<String> = classification["per_source"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        format!(
                            "{} → primary {} ({}), secondary [{}]",
                            row["source"].as_str().unwrap_or("?"),
                            row["primary"].as_str().unwrap_or("?"),
                            row["primary_name"].as_str().unwrap_or("?"),
                            row["secondary"]
                                .as_array()
                                .map(|list| list
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", "))
                                .unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        lines.push(format!(
            "## Acquisition tables and bottleneck classification\n\n\
             §17's switch was on for this run, so the directory also holds four tables grouped \
             over the lines above rather than measured again: `{STORAGE_BREAKDOWN_FILE}` (every \
             storage read with its address, slot and duration, then grouped by address — §6), \
             `{ACCOUNT_MATRIX_FILE}` (code, balance and nonce per address — §9), \
             `{RPC_GAPS_FILE}` (every idle stretch between calls, one sample per wait — §12) \
             and `{BOTTLENECK_FILE}` (§25's A–G verdict). Each is built from the \
             `duration_row` projection of these same lines, so a figure that appears in both a \
             line and a table is one figure copied, not two computed. This run: {}.\n\n\
             The classification file publishes the threshold it applied beside the integers it \
             applied it to, per source, and never adds two sources together. A `not_measured` \
             category is an absent figure; `ruled_out_by_measurement` is a figure that landed on \
             the safe side of a declared line. §18: naming a category is the whole of what this \
             file does — the directions it makes possible are written in the completion report as \
             candidates and none of them is implemented here.\n",
            per_source.join("; ")
        ));
    }
    if let Some(runs) = summary.get("assembled_from").filter(|runs| runs.is_array()) {
        let names: Vec<String> = runs
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|run| {
                        format!(
                            "{} ({} simulation(s), {} call(s))",
                            run["run"].as_str().unwrap_or("?"),
                            run["simulations"].as_u64().unwrap_or(0),
                            run["simulation_calls"]
                                .as_u64()
                                .unwrap_or_else(|| run["calls"].as_u64().unwrap_or(0)),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let list = names
            .iter()
            .map(|name| format!("- {name}"))
            .collect::<Vec<_>>()
            .join("\n");
        lines.push(format!(
            "## How this directory was assembled\n\n\
             This is not one run's directory. It is {} folded together by replaying each source\n\
             run's `{TRACES_FILE}` lines through the writer that produced them, so every table\n\
             here is the same function of the same recorded calls as the tables in the per-run\n\
             directories — the runs' own figures are not re-typed, averaged or re-derived from a\n\
             second implementation.\n\n\
             {list}\n\n\
             Pooling simulations is sound because every timeline figure is measured *inside one\n\
             simulation's own window*: a gap, a serial wait and an overlap are differences of\n\
             stamps that share that simulation's origin, so a run contributes whole simulations\n\
             and never half a clock. What is not poolable is a figure that spans two runs' clocks,\n\
             and there is one such figure here: `{OUTSIDE_FILE}` keeps each run's stage spans with\n\
             that run, under `assembled_from`, and tags each pooled row with the run it was\n\
             measured in.\n\n\
             `generated_at_unix_ms` is the source runs' stamp rather than the moment of assembly,\n\
             so re-assembling the same runs writes byte-identical files. No duration is computed\n\
             from it (§7).\n",
            names.len()
        ));
    }
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
        slot: None,
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

    /// §12: the gaps are measured one by one, and their widths add back to the total the
    /// window's covered time says they must. The three-run case is the one a real run
    /// produces: a serial chain of calls with idle stretches of different sizes between
    /// them, where "the biggest gap" and "how many gaps" are different questions.
    #[test]
    fn each_idle_stretch_inside_the_call_window_is_counted_and_measured() {
        let window = window(0, 10_000);
        let events = vec![
            call(1, "eth_getCode", 1_000, 1_500, Some("code|a")),
            // 1 500 → 2 000: idle, 500 ns.
            call(2, "eth_getCode", 2_000, 2_200, Some("code|b")),
            // 2 200 → 4 700: idle, 2 500 ns — the widest.
            call(3, "eth_getCode", 4_700, 5_000, Some("code|c")),
            // Nothing idle after the last call: the window's tail is outside the call
            // window, so it is not a gap and must not be counted as one.
        ];
        let seen = timeline(&window, &events);
        assert_eq!(seen.gap_count, Some(2));
        assert_eq!(seen.gap_widths_ns, vec![500, 2_500]);
        assert_eq!(
            seen.gap_widths_ns.iter().sum::<u64>(),
            seen.gap_duration_ns.unwrap_or_default(),
            "the widths are a partition of the idle time, not a sample of it"
        );
        assert_eq!(seen.wall_duration_ns, Some(4_000), "1 000 → 5 000");
        assert_eq!(seen.union_duration_ns, Some(1_000));
        assert_eq!(seen.gap_duration_ns, Some(3_000));
        assert_eq!(seen.non_rpc_duration_ns, Some(9_000), "10 000 − 1 000");
        // A row of widths is a distribution over gaps, so a single gap says so rather
        // than printing a min and a max that look like a spread.
        let row = seen.to_json();
        assert_eq!(row["rpc_gap_count"], 2);
        assert_eq!(row["rpc_gap_distribution_ns"]["samples"], 2);
        assert_eq!(row["rpc_gap_distribution_ns"]["min_ns"], 500);
        assert_eq!(row["rpc_gap_distribution_ns"]["max_ns"], 2_500);
    }

    /// §12, the negative case: a run whose calls never stopped is a measurement that
    /// there was no idle time, and must not print as a missing figure. A window with no
    /// calls at all is the opposite case and has to read differently.
    #[test]
    fn a_window_that_never_idled_reports_no_gap_rather_than_a_missing_one() {
        let window = window(0, 1_000);
        let events = vec![
            call(1, "eth_getCode", 0, 500, Some("code|a")),
            call(2, "eth_getCode", 500, 1_000, Some("code|b")),
        ];
        let seen = timeline(&window, &events);
        assert_eq!(seen.gap_count, Some(0));
        assert!(seen.gap_widths_ns.is_empty());
        let row = seen.to_json();
        assert_eq!(row["rpc_gap_distribution_ns"]["measured"], true);
        assert_eq!(row["rpc_gap_distribution_ns"]["samples"], 0);

        let empty = timeline(&window, &[]);
        assert_eq!(empty.gap_count, None, "no calls, so no between them");
        let row = empty.to_json();
        assert_eq!(row["rpc_gap_count"], Value::Null);
        assert_eq!(row["rpc_gap_distribution_ns"]["measured"], false);
    }

    /// §13: the RPC-versus-local share is taken on the interval union, so it cannot
    /// report more RPC time than the simulation had. The `sum − total` shape the clause
    /// forbids would here say the simulation spent 140 % of itself on RPC.
    #[test]
    fn the_rpc_share_is_the_union_over_the_window_and_never_a_sum() {
        // Bound before the local below shadows the helper of the same name.
        let instant_window = window(5, 5);
        let window = window(0, 1_000);
        // Three calls of 700 ns each, all inside one 1 000 ns window: sum 2 100, union 700.
        let events = vec![
            call(1, "eth_getCode", 0, 700, Some("code|a")),
            call(2, "eth_getCode", 0, 700, Some("code|b")),
            call(3, "eth_getCode", 0, 700, Some("code|c")),
        ];
        let seen = timeline(&window, &events);
        assert_eq!(seen.sum_duration_ns, 2_100);
        assert_eq!(seen.union_duration_ns, Some(700));
        let ratio = seen.to_json()["rpc_duration_ratio"].clone();
        assert_eq!(ratio["numerator"], 700, "union, not sum");
        assert_eq!(ratio["numerator_field"], "rpc_union_duration_ns");
        assert_eq!(ratio["denominator"], 1_000);
        assert_eq!(ratio["per_mille"], 700);
        assert!(
            ratio["per_mille"].as_u64().unwrap_or(0) <= 1_000,
            "a share of a span cannot exceed the span"
        );

        // A window that spans no time leaves the quotient null instead of inventing it.
        let instant = timeline(&instant_window, &[call(1, "eth_getCode", 5, 5, Some("k"))]);
        let ratio = instant.to_json()["rpc_duration_ratio"].clone();
        assert_eq!(ratio["per_mille"], Value::Null);
        assert_eq!(ratio["denominator"], 0);
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
        let mut evidence = DiagnosisEvidence::open(&dir, "deadbeef", "disabled", false)
            .expect("the directory opens");
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
        let mut evidence =
            DiagnosisEvidence::open(&dir, "revision", "disabled", false).expect("opens");
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
        let mut evidence =
            DiagnosisEvidence::open(&dir, "revision", "disabled", false).expect("opens");
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
            let mut evidence =
                DiagnosisEvidence::open(dir, "revision", "disabled", false).expect("opens");
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
        let mut evidence =
            DiagnosisEvidence::open(&dir, "revision", "disabled", false).expect("opens");
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

    /// A storage call as the wire recorded it: the address and the word the params named,
    /// in the shapes `describe_call` puts on an event. `address_in_wire_case` is what the
    /// call carried, which the row then normalizes the same way the dedup key does.
    fn storage_call(
        rpc_id: u64,
        address_in_wire_case: &str,
        slot: &str,
        started_ns: u64,
        finished_ns: u64,
    ) -> RpcCallEvent {
        let address = address_in_wire_case.to_lowercase();
        let key = format!("storage|91342|37594591|{address}|{slot}");
        let mut event = call(
            rpc_id,
            "eth_getStorageAt",
            started_ns,
            finished_ns,
            Some(&key),
        );
        event.target = Some(address_in_wire_case.to_string());
        event.slot = Some(slot.to_string());
        event
    }

    /// §6: the 20 reads are listed one per word, and grouping them by address is what
    /// turns "20 × getStorageAt" into the thing that decides a next step — which pool,
    /// which slots, what each cost. The checksum case in the second address is the point
    /// of testing this: one pool written two ways must be one group.
    #[test]
    fn every_storage_read_is_listed_with_its_word_and_groups_by_address() {
        let window = window(0, 10_000);
        let events = vec![
            storage_call(1, "0xAa11", "0x00", 0, 400),
            storage_call(2, "0xAa11", "0x01", 400, 900),
            storage_call(3, "0xAA11", "0x02", 900, 1_400),
            storage_call(4, "0xBB22", "0x08", 1_400, 2_400),
            call(
                5,
                "eth_getCode",
                2_400,
                2_900,
                Some("code|91342|37594591|0xaa11"),
            ),
        ];
        let diagnosis =
            SimulationDiagnosis::new(window, events).with_endpoint(Some("rpc-test".to_string()));
        let rows = diagnosis.storage_rows();
        assert_eq!(rows.len(), 4, "the code read is not a storage row");
        assert_eq!(rows[0]["slot"], "0x00");
        assert_eq!(rows[0]["address"], "0xaa11", "lowercased like the key");
        assert_eq!(rows[0]["chain_id"], 91_342);
        assert_eq!(rows[0]["block"], "37594591");
        assert_eq!(rows[0]["duration_ns"], 400);
        assert_eq!(rows[0]["rpc_id"], 1);
        assert_eq!(rows[0]["endpoint_id"], "rpc-test");
        assert_eq!(rows[0]["semantic"], "unknown");
        assert!(rows[0]["semantic_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("no slot-to-name mapping"));

        let breakdown = storage_breakdown(&rows);
        assert_eq!(breakdown["reads_total"], 4);
        assert_eq!(
            breakdown["address_count"], 2,
            "0xAa11 and 0xAA11 are one pool"
        );
        assert_eq!(breakdown["reads_without_address"], 0);
        let first = &breakdown["addresses"][0];
        assert_eq!(first["address"], "0xaa11");
        assert_eq!(first["reads"], 3);
        assert_eq!(first["slot_count"], 3);
        assert_eq!(first["duration_total_ns"], 1_400, "400 + 500 + 500");
        // §8's three sharing questions, answered from the rows rather than asserted.
        let merge = &breakdown["mergeability"]["per_address"][0];
        assert_eq!(merge["distinct_endpoints"], 1);
        assert_eq!(merge["distinct_blocks"], 1);
        assert_eq!(merge["distinct_simulations"], 1);
        assert_eq!(merge["per_simulation_batchable"], true);
        assert_eq!(
            breakdown["mergeability"]["status"],
            "candidate_only_not_implemented"
        );
    }

    /// §6's list is over the calls, so a call whose params this build could not read is
    /// a row with a null address — counted, and said to be counted, rather than dropped
    /// from a table whose total would then quietly be a subtotal.
    #[test]
    fn a_storage_read_with_no_readable_word_is_counted_and_not_dropped() {
        let events = vec![
            storage_call(1, "0xAa11", "0x00", 0, 400),
            call(2, "eth_getStorageAt", 400, 800, None),
        ];
        let diagnosis = SimulationDiagnosis::new(window(0, 1_000), events);
        let rows = diagnosis.storage_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["address"], Value::Null);
        assert_eq!(rows[1]["slot"], Value::Null);
        let breakdown = storage_breakdown(&rows);
        assert_eq!(breakdown["reads_total"], 2);
        assert_eq!(breakdown["address_count"], 1);
        assert_eq!(breakdown["reads_without_address"], 1);
    }

    /// §9: the three account legs of one simulation, grouped by address so the table says
    /// whether 18 reads are 6 accounts asked three things each or 18 accounts asked one.
    /// `kind` is the column; the row is the address.
    #[test]
    fn the_account_legs_group_by_address_into_one_matrix() {
        let mut code = call(1, "eth_getCode", 0, 300, Some("code|91342|37594591|0xaa11"));
        code.target = Some("0xAA11".to_string());
        let mut balance = call(
            2,
            "eth_getBalance",
            300,
            600,
            Some("balance|91342|37594591|0xaa11"),
        );
        balance.target = Some("0xaa11".to_string());
        let mut nonce = call(
            3,
            "eth_getTransactionCount",
            600,
            900,
            Some("nonce|91342|37594591|0xbb22"),
        );
        nonce.target = Some("0xBB22".to_string());
        let mut second_code = call(
            10,
            "eth_getCode",
            900,
            1_200,
            Some("code|91342|37594591|0xbb22"),
        );
        second_code.target = Some("0xBB22".to_string());
        let events = vec![
            code,
            balance,
            nonce,
            second_code,
            storage_call(4, "0xCC33", "0x00", 1_200, 1_500),
        ];
        let diagnosis = SimulationDiagnosis::new(window(0, 2_000), events);
        let rows = diagnosis.account_rows();
        assert_eq!(rows.len(), 4, "the storage read is not an account leg");
        assert_eq!(rows[0]["kind"], "code");
        assert_eq!(rows[1]["kind"], "balance");
        assert_eq!(rows[2]["kind"], "nonce");

        let matrix = account_matrix(&rows);
        assert_eq!(matrix["reads_total"], 4);
        assert_eq!(matrix["address_count"], 2);
        let aa = &matrix["addresses"][0];
        assert_eq!(aa["address"], "0xaa11");
        assert_eq!(aa["code"], 1);
        assert_eq!(aa["balance"], 1);
        assert_eq!(aa["nonce"], 0, "this address was never asked for a nonce");
        let bb = &matrix["addresses"][1];
        assert_eq!(bb["nonce"], 1);
        assert_eq!(bb["code"], 1);
        assert_eq!(bb["balance"], 0);
        assert_eq!(matrix["totals"]["code"]["reads"], 2);
        assert_eq!(matrix["totals"]["balance"]["reads"], 1);
        assert_eq!(matrix["totals"]["nonce"]["reads"], 1);
        // Two legs of one address cost 300 + 300: the matrix carries the sum, because §9's
        // question is about an account's whole read cost, not about a method's count.
        assert_eq!(matrix["totals"]["code"]["duration_ns"], 600);
        assert_eq!(
            matrix["same_account_all_three_legs"]["with_all_three_legs"],
            0
        );
    }

    /// §10: `required_by` is the two counts agreeing, and the row prints both. Where they
    /// disagree — as they do in the reuse-off arm, where the boundary is never consulted
    /// for the account kinds — the answer is `unknown` with the numbers beside it, not a
    /// guess that happens to be right.
    #[test]
    fn required_by_is_the_wire_count_measured_against_the_boundarys_misses() {
        let events = vec![
            storage_call(1, "0xAa11", "0x00", 0, 400),
            storage_call(2, "0xAa11", "0x01", 400, 800),
        ];
        let mut tally = StateReadStats::new(true);
        tally.storage = ReuseTally { hits: 3, misses: 2 };
        let matched = SimulationDiagnosis::new(window(0, 1_000), events.clone())
            .with_state_reads(Some(tally));
        let rows = matched.storage_rows();
        assert_eq!(rows[0]["required_by"], "simulation");
        assert!(rows[0]["required_by_basis"]
            .as_str()
            .unwrap_or_default()
            .contains("2 call(s) on the wire, 2 miss(es)"));

        // The same two calls with no boundary attached: nothing says who asked.
        let unattached = SimulationDiagnosis::new(window(0, 1_000), events.clone());
        let rows = unattached.storage_rows();
        assert_eq!(rows[0]["required_by"], "unknown");

        // And with the boundary reporting a different number of misses, still `unknown` —
        // the disagreement is the finding, printed with both counts.
        let mut off = StateReadStats::new(false);
        off.storage = ReuseTally { hits: 0, misses: 5 };
        let disagreed =
            SimulationDiagnosis::new(window(0, 1_000), events).with_state_reads(Some(off));
        let rows = disagreed.storage_rows();
        assert_eq!(rows[0]["required_by"], "unknown");
        assert!(rows[0]["required_by_basis"]
            .as_str()
            .unwrap_or_default()
            .contains("2 call(s) on the wire against 5 miss(es)"));
    }

    /// One stage window as §14 sees it, given directly rather than through a trace, so a
    /// classification test says which spans existed without also re-testing M8.1's stamps.
    fn span(stage: &'static str, window: Option<(u64, u64)>) -> StageSpan {
        StageSpan {
            stage,
            outcome: "completed",
            window_ns: window,
            window_note: match window {
                Some(_) => NANOSECOND_SPAN,
                None => OPEN_SPAN,
            },
        }
    }

    /// §14: a lifecycle read is filed with the stage whose span held it, and the row says
    /// which span that was. The five cases below are the five a route run actually
    /// produces — the head read at `observation`, the reserves read while detection ran,
    /// a read inside the simulation's own span, the gate's balance read, and a read made
    /// while the run was doing its bookkeeping.
    #[test]
    fn a_lifecycle_read_is_filed_with_the_stage_whose_span_held_it() {
        let spans = vec![
            span("observation", Some((0, 1_000))),
            StageSpan {
                stage: "state_update",
                outcome: "skipped",
                window_ns: None,
                window_note: OPEN_SPAN,
            },
            span("opportunity_detection", Some((1_000, 2_000))),
            span("simulation", Some((2_000, 3_000))),
            span("preflight", Some((3_000, 4_000))),
            span("settlement", Some((4_000, 5_000))),
        ];
        let events = {
            let mut storage = call(
                3,
                "eth_getStorageAt",
                2_500,
                2_900,
                Some("storage|91342|37594591|0xaa11|0x00"),
            );
            storage.target = Some("0xAa11".to_string());
            storage.slot = Some("0x00".to_string());
            vec![
                call(1, "eth_blockNumber", 100, 500, None),
                call(2, "eth_call", 1_500, 1_900, None),
                storage,
                call(
                    4,
                    "eth_getBalance",
                    3_500,
                    3_800,
                    Some("balance|91342|37594591|0xbb22"),
                ),
                call(5, "eth_getTransactionReceipt", 4_500, 4_800, None),
            ]
        };
        let rows = lifecycle_rows(&events, &spans, Some("rpc-0123456789abcdef"));
        assert_eq!(rows.len(), 5, "one row per call the lifecycle made");
        assert_eq!(rows[0]["stage"], "observation");
        assert_eq!(rows[0]["class"], LIFECYCLE_SIMULATION_CONTEXT);
        assert_eq!(rows[1]["class"], LIFECYCLE_DETECTION);
        assert_eq!(rows[2]["class"], LIFECYCLE_SIMULATION_STATE);
        assert_eq!(rows[3]["class"], LIFECYCLE_PREFLIGHT);
        assert_eq!(
            rows[4]["class"], LIFECYCLE_ORCHESTRATION,
            "a stage this milestone does not name is bookkeeping, not state acquisition"
        );
        for row in &rows {
            assert_eq!(row["endpoint_id"], "rpc-0123456789abcdef");
            assert_eq!(row["attempts"], 1);
            assert!(
                row["class_basis"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("falls inside the `"),
                "the basis names the span it used rather than asserting a class"
            );
        }
        // The call's own facts travel unchanged: §14 classifies, it does not re-measure.
        assert_eq!(rows[3]["duration_ns"], 300);
        assert_eq!(rows[3]["dedup_key"], "balance|91342|37594591|0xbb22");
        assert_eq!(rows[0]["dedup_key"], Value::Null);
        assert_eq!(rows[2]["slot"], "0x00");
        assert_eq!(
            rows[2]["target"], "0xaa11",
            "the address is normalized the way the dedup key normalizes it"
        );

        // A skipped stage holds nothing, and a call inside its time of day is still filed
        // by whatever span did hold it — here the one that follows.
        let during_skip = lifecycle_rows(
            &[call(1, "eth_blockNumber", 500, 600, None)],
            &[
                span("observation", Some((0, 400))),
                span("state_update", None),
                span("opportunity_detection", Some((500, 900))),
            ],
            None,
        );
        assert_eq!(during_skip[0]["stage"], "opportunity_detection");
    }

    /// §14's `unknown` is a measurement and not a shrug: the row names the spans that were
    /// tried, so a reader can tell "this call fell between two stages" from "this run
    /// recorded no stage span at all". The second is what a run without §17's switch would
    /// look like, and it must not read as the first.
    #[test]
    fn a_call_no_span_holds_is_unknown_and_says_which_spans_it_missed() {
        let spans = vec![
            span("observation", Some((0, 1_000))),
            span("simulation", Some((2_000, 3_000))),
        ];
        let rows = lifecycle_rows(
            &[call(1, "eth_blockNumber", 1_500, 1_600, None)],
            &spans,
            None,
        );
        assert_eq!(rows[0]["class"], LIFECYCLE_UNKNOWN);
        assert_eq!(rows[0]["stage"], Value::Null);
        let basis = rows[0]["class_basis"].as_str().unwrap_or_default();
        assert!(basis.contains("observation(0–1000)"), "{basis}");
        assert!(basis.contains("simulation(2000–3000)"), "{basis}");

        let blind = lifecycle_rows(&[call(1, "eth_blockNumber", 1_500, 1_600, None)], &[], None);
        assert_eq!(blind[0]["class"], LIFECYCLE_UNKNOWN);
        assert!(
            blind[0]["class_basis"]
                .as_str()
                .unwrap_or_default()
                .contains("wrote no nanosecond stage span at all"),
            "no spans to try is a different finding from none of them fitting: {}",
            blind[0]["class_basis"]
        );
    }

    /// §14 reads its windows off the trace M8.1 already wrote, and a span whose ends are
    /// *millisecond* readings is declared unusable rather than widened to three zeros so a
    /// nanosecond call could be claimed to fall inside it. A skipped stage has no interval
    /// at all; a closed nanosecond stage has one.
    #[test]
    fn a_millisecond_stage_span_is_declared_unusable_rather_than_widened() {
        let mut trace = evm_metrics::LatencyTrace::new(
            evm_metrics::TraceSource::Live,
            91_342,
            Some(37_594_591),
            None,
        );
        trace
            .begin(evm_metrics::Stage::Observation, 1_000)
            .expect("opening the first stage");
        trace
            .complete(evm_metrics::Stage::Observation, 2_000)
            .expect("closing it");
        trace
            .skip(
                evm_metrics::Stage::StateUpdate,
                "a route run applies no state updates",
            )
            .expect("skipping a stage is a record");
        trace
            .record(evm_metrics::StageRecord::measured_ms(
                evm_metrics::Stage::Build,
                5,
                6,
            ))
            .expect("a duration read from M7's evidence");

        let spans = StageSpan::of(&trace);
        let by_stage: BTreeMap<&str, &StageSpan> =
            spans.iter().map(|held| (held.stage, held)).collect();
        let observation = by_stage["observation"];
        assert_eq!(observation.window_ns, Some((1_000, 2_000)));
        assert_eq!(observation.outcome, "completed");
        assert_eq!(observation.window_note, NANOSECOND_SPAN);
        assert_eq!(by_stage["state_update"].window_ns, None);
        assert_eq!(by_stage["state_update"].window_note, OPEN_SPAN);
        let build = by_stage["build"];
        assert_eq!(
            build.window_ns, None,
            "5 ms to 6 ms is not an interval a call stamped in nanoseconds fell inside"
        );
        assert_eq!(build.window_note, COARSE_SPAN);

        // The shape the file publishes, so a reader can check the classification's inputs.
        let row = build.to_json();
        assert_eq!(row["usable_for_classification"], false);
        assert_eq!(row["started_ns"], Value::Null);
        assert_eq!(
            by_stage["observation"].to_json()["usable_for_classification"],
            true
        );
    }

    /// §14's file as §19 asks for it: every one of the six classes listed even when four
    /// are empty (an empty class must read as measured-and-empty), the 39 core reads
    /// counted as *not here* by name, and the three reads this observer provably cannot
    /// reach named rather than left as a blank a reader might fill with a zero.
    #[test]
    fn the_outside_table_lists_every_class_and_excludes_the_core_reads_by_name() {
        let spans = vec![
            span("observation", Some((0, 1_000))),
            span("preflight", Some((2_000, 3_000))),
        ];
        let events = vec![
            call(1, "eth_blockNumber", 100, 500, None),
            call(
                2,
                "eth_getBalance",
                2_100,
                2_900,
                Some("balance|91342|37594591|0xbb22"),
            ),
        ];
        let rows = lifecycle_rows(&events, &spans, Some("rpc-0123456789abcdef"));
        let table = outside_simulation_table(&rows, &spans, Some("rpc-0123456789abcdef"), 39);

        assert_eq!(table["calls"], 2);
        assert_eq!(table["duration_total_ns"], 1_200, "400 + 800");
        assert_eq!(table["endpoint_id"], "rpc-0123456789abcdef");
        assert_eq!(table["stage_spans"].as_array().map(Vec::len), Some(2));

        let classes = table["per_class"].as_array().cloned().unwrap_or_default();
        let names: Vec<&str> = classes
            .iter()
            .filter_map(|row| row["class"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                LIFECYCLE_SIMULATION_STATE,
                LIFECYCLE_SIMULATION_CONTEXT,
                LIFECYCLE_DETECTION,
                LIFECYCLE_PREFLIGHT,
                LIFECYCLE_ORCHESTRATION,
                LIFECYCLE_UNKNOWN,
            ],
            "§14's own order, inside out"
        );
        assert_eq!(classes[0]["calls"], 0);
        assert_eq!(classes[0]["methods"], Value::Array(Vec::new()));
        assert_eq!(classes[0]["duration_total_ns"], 0);
        // 0 of 2 is a measured zero, not a missing figure: this class was asked about and
        // held nothing. Only a lifecycle that made *no* calls has no share at all, and that
        // is the empty table further down.
        let empty_class = &classes[0]["share_of_lifecycle_calls_per_mille"];
        assert_eq!(empty_class["per_mille"], 0);
        assert_eq!(empty_class["numerator"], 0);
        assert_eq!(empty_class["denominator"], 2);
        assert_eq!(classes[1]["calls"], 1);
        assert_eq!(classes[1]["methods"], json!(["eth_blockNumber"]));
        assert_eq!(classes[1]["stages"], json!(["observation"]));
        assert_eq!(
            classes[1]["share_of_lifecycle_calls_per_mille"]["per_mille"],
            500
        );
        assert_eq!(classes[3]["calls"], 1);
        assert_eq!(classes[3]["duration_total_ns"], 800);

        let methods = table["per_method"].as_array().cloned().unwrap_or_default();
        assert_eq!(methods.len(), 2, "one row per method, sorted by name");
        assert_eq!(methods[0]["method"], "eth_blockNumber");
        assert_eq!(methods[0]["duration_total_ns"], 400);
        assert_eq!(methods[1]["method"], "eth_getBalance");

        assert_eq!(
            table["core_state_reads_not_included"]["simulation_calls_recorded_elsewhere"],
            39
        );
        assert!(table["core_state_reads_not_included"]["rule"]
            .as_str()
            .unwrap_or_default()
            .contains("no call is in both"));
        assert_eq!(table["unreachable_reads"].as_array().map(Vec::len), Some(3));
        assert_eq!(
            table["unreachable_reads"][0]["reads"], "eth_chainId",
            "the one call the sink structurally cannot see comes first"
        );

        // And a lifecycle that asked the node for nothing is a measured zero: no calls, no
        // duration, and — because 0 ÷ 0 is not a share — no share either.
        let empty = outside_simulation_table(&[], &[], None, 0);
        assert_eq!(empty["calls"], 0);
        assert_eq!(empty["duration_distribution_ns"]["measured"], false);
        assert!(
            empty["per_class"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|row| row["calls"] == 0),
            "no per_class row carries a call count"
        );
        assert_eq!(
            empty["per_class"][0]["share_of_lifecycle_calls_per_mille"]["per_mille"],
            Value::Null
        );
        assert_eq!(
            empty["per_class"][0]["share_of_lifecycle_calls_per_mille"]["reason"],
            NOTHING_MEASURED
        );
    }

    // -----------------------------------------------------------------------
    // M8.3.2 §19: the acquisition tables and the A–G classification
    // -----------------------------------------------------------------------

    /// One simulation as §19's grouped tables read it: the projection of the line a run
    /// writes, with nothing added to it.
    fn acquisition_row(diagnosis: &SimulationDiagnosis) -> Value {
        duration_row(&diagnosis.to_trace_line(&[]))
    }

    /// A window on the source the rules were written for — a node answering over the
    /// network. `window`'s fixture default would file the row in another bucket, and the
    /// classification is per source by construction.
    fn node_window(started_ns: u64, finished_ns: u64) -> SimulationWindow {
        let mut out = window(started_ns, finished_ns);
        out.source = RpcTraceSource::Live;
        out
    }

    /// An account-leg call with its address set, so §9's matrix has something to group.
    fn account_call(
        rpc_id: u64,
        method: &str,
        address: &str,
        started_ns: u64,
        finished_ns: u64,
    ) -> RpcCallEvent {
        let kind = account_kind(method).unwrap_or("state");
        let key = format!("{kind}|91342|37594591|{}", address.to_lowercase());
        let mut event = call(rpc_id, method, started_ns, finished_ns, Some(&key));
        event.target = Some(address.to_string());
        event
    }

    /// A boundary tally, zero-filled, for the rows whose arm is the point rather than
    /// whose hit count is. `spec_tally` is for §12's worked numbers; this is for §25's.
    fn named_arm(reuse: bool) -> StateReadStats {
        StateReadStats::new(reuse)
    }

    fn category(block: &Value, id: &str) -> Value {
        block["categories"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("no category {id} in {block}"))
    }

    fn source_block(table: &Value, source: &str) -> Value {
        table["per_source"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["source"] == json!(source))
            .cloned()
            .unwrap_or_else(|| panic!("no block for source {source} in {table}"))
    }

    /// The projection is the line minus its four call lists, with every field it keeps
    /// unchanged: §19's grouped tables are a regrouping of published bytes, so a value that
    /// moved on the way in would make the table a second measurement after all.
    #[test]
    fn the_projection_drops_the_call_lists_and_moves_nothing_else() {
        let diagnosis = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                storage_call(1, "0xAA11", "0x01", 0, 300),
                account_call(2, "eth_getCode", "0xBB22", 400, 700),
            ],
        )
        .with_state_reads(Some(named_arm(true)))
        .with_endpoint(Some("rpc-0123456789abcdef".to_string()));
        let line = diagnosis.to_trace_line(&["a refusal of the writer's own".to_string()]);
        for name in CALL_LIST_KEYS {
            assert!(
                line[name].is_array() || line.get(name).is_some(),
                "{name} is missing from the line the projection reads"
            );
        }
        let row = duration_row(&line);
        for name in CALL_LIST_KEYS {
            assert!(
                row.get(name).is_none(),
                "{name} survived the projection: the gap and classification tables would be \
                 reading a call list they do not need"
            );
        }
        assert!(row["rpc"].is_object() && row["duplicates"].is_object());
        assert!(row["methods"].is_array() && row["state_read_cache"].is_object());
        assert_eq!(
            row["simulation_duration_ns"],
            line["simulation_duration_ns"]
        );
        assert_eq!(row["endpoint_id"], json!("rpc-0123456789abcdef"));
        assert_eq!(row["source"], json!("live"));
        for (key, value) in row.as_object().expect("the projection is an object") {
            assert_eq!(
                &line[key], value,
                "{key} changed on the way into the projection"
            );
        }
    }

    /// §12's widths are merged as samples. A directory of three runs has one population of
    /// gaps, not three medians to average — and a run whose single gap cannot support a median
    /// still contributes its width to the population that can.
    #[test]
    fn the_gap_table_merges_widths_and_not_the_summaries_above_them() {
        let one = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 300, Some("storage|a")),
                call(2, "eth_getStorageAt", 400, 700, Some("storage|b")),
            ],
        );
        let two = SimulationDiagnosis::new(
            node_window(0, 1_000_000),
            vec![
                call(1, "eth_getStorageAt", 0, 100, Some("storage|c")),
                call(2, "eth_getStorageAt", 300, 400, Some("storage|d")),
                call(3, "eth_getStorageAt", 900, 950, Some("storage|e")),
            ],
        );
        let table = rpc_gaps(&[acquisition_row(&one), acquisition_row(&two)]);
        assert_eq!(table["simulations"], 2);
        assert_eq!(table["simulations_with_calls"], 2);
        assert_eq!(table["simulations_with_idle_stretches"], 2);
        assert_eq!(
            table["gap_count"], 3,
            "one stretch from each of the first two, two \
                                          from the third"
        );
        assert_eq!(table["gap_total_ns"], 100 + 200 + 500);
        assert_eq!(table["gap_min_ns"], 100);
        assert_eq!(table["gap_max_ns"], 500);
        assert_eq!(table["gap_median_ns"], 200);
        assert_eq!(table["gap_width_distribution_ns"]["samples"], 3);
        assert_eq!(
            table["per_simulation"][0]["own_gap_distribution_ns"]["p50_ns"],
            Value::Null,
            "one stretch cannot support a median, which is exactly why the merged figure is \
             taken over widths"
        );
        assert_eq!(
            table["per_simulation"][1]["own_gap_distribution_ns"]["p50_ns"], 200,
            "two samples: nearest-rank p50 is the first of the pair, which is why the merged \
             population is the figure a report leads with — the wider stretch is still in `max_ns`"
        );
        assert_eq!(
            table["per_simulation"][1]["rpc_gap_widths_ns"],
            json!([200, 500])
        );
        assert_eq!(table["gaps_per_simulation_distribution"]["samples"], 2);
        assert_eq!(
            table["gap_total_over_simulation_span_per_mille"]["denominator"],
            1_001_000
        );
        assert!(
            !has_a_float(&table),
            "§13: no float in the gap table — every ratio is per-mille integers"
        );
    }

    /// Whether any number in a JSON tree was built as a float. `is_f64` is true for an
    /// integer-valued `Number` too, so the test asks serde_json what the number *is*, not
    /// whether its text happens to contain a decimal point — the prose in these files is
    /// full of periods, and a check that keyed on them would fail on a sentence.
    fn has_a_float(value: &Value) -> bool {
        match value {
            Value::Number(number) => number.is_f64(),
            Value::Array(values) => values.iter().any(has_a_float),
            Value::Object(entries) => entries.values().any(has_a_float),
            _ => false,
        }
    }

    /// A simulation that asked the node for nothing is not a simulation that waited for
    /// nothing: its row counts as no call window, and its gap figures are null rather than 0.
    #[test]
    fn a_simulation_with_no_calls_is_counted_as_no_window_not_as_a_zero_gap() {
        let empty = SimulationDiagnosis::new(node_window(0, 1_000), Vec::new());
        let with = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![call(1, "eth_getStorageAt", 0, 500, Some("storage|a"))],
        );
        let table = rpc_gaps(&[acquisition_row(&empty), acquisition_row(&with)]);
        assert_eq!(table["simulations"], 2);
        assert_eq!(table["simulations_with_calls"], 1);
        assert_eq!(table["simulations_without_calls"], 1);
        assert_eq!(table["simulations_with_idle_stretches"], 0);
        assert_eq!(table["gap_count"], 0);
        assert_eq!(table["gap_min_ns"], Value::Null);
        assert_eq!(table["gap_width_distribution_ns"]["samples"], 0);
        assert_eq!(table["gap_width_distribution_ns"]["measured"], false);
        assert_eq!(table["per_simulation"][0]["rpc_gap_widths_ns"], json!([]));
        assert_eq!(
            table["gap_total_over_call_window_per_mille"]["per_mille"], 0,
            "the second simulation did cover its whole window with a call: a measured zero"
        );

        let none = rpc_gaps(&[]);
        assert_eq!(none["simulations"], 0);
        assert_eq!(none["gap_total_ns"], 0);
        assert_eq!(none["gap_min_ns"], Value::Null);
        assert_eq!(
            none["gap_total_over_simulation_span_per_mille"]["per_mille"],
            Value::Null,
            "0 ÷ 0 is not a share (§5)"
        );
    }

    /// §19's rule list, §25's seven names, and one block per source. A fixture's span is local
    /// compute by construction and a live run's is a node's answer, so the two are never folded
    /// into one table — and a source with nothing measured gets `not_measured` on all six rather
    /// than six pass marks.
    #[test]
    fn the_classification_holds_the_seven_categories_per_source_and_blends_no_sources() {
        let fixture = SimulationDiagnosis::new(window(0, 1_000), Vec::new());
        let live = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![call(1, "eth_getStorageAt", 0, 500, Some("storage|a"))],
        );
        let table =
            bottleneck_classification(&[acquisition_row(&fixture), acquisition_row(&live)], None);
        assert_eq!(table["simulations"], 2);
        assert_eq!(table["sources"], json!(["fixture", "live"]));
        assert_eq!(table["sources_are_never_blended"], json!(true));
        assert_eq!(
            table["categories_are_the_seven"].as_array().map(Vec::len),
            Some(7)
        );
        assert_eq!(
            table["thresholds_are_declared"]["dominant_per_mille"],
            json!(500)
        );
        assert_eq!(
            table["thresholds_are_declared"]["latency_floor_ns"],
            json!(LATENCY_FLOOR_NS)
        );
        assert_eq!(
            table["sink_disjointness_check"]["lifecycle_table_present"],
            json!(false)
        );

        let fixture_block = source_block(&table, "fixture");
        assert_eq!(fixture_block["simulations_with_calls"], 0);
        assert_eq!(fixture_block["aggregated_figures"]["calls"], 0);
        assert_eq!(
            fixture_block["aggregated_figures"]["non_rpc_measured"],
            json!(false)
        );
        for row in fixture_block["categories"].as_array().expect("the seven") {
            if row["id"] == json!("G") {
                continue;
            }
            assert_eq!(
                row["verdict"],
                json!(NOT_MEASURED),
                "{} has no measurement behind it and must not print a ruling",
                row["id"]
            );
            assert_eq!(row["share_per_mille"]["per_mille"], Value::Null);
        }
        assert_eq!(fixture_block["primary"], json!("G"));
        assert!(fixture_block["primary_rationale"]
            .as_str()
            .unwrap_or_default()
            .contains("insufficient evidence"));

        // The live block's numbers are its own one call, not the two rows' sum.
        let live_block = source_block(&table, "live");
        assert_eq!(live_block["aggregated_figures"]["calls"], 1);
        assert_eq!(live_block["simulations_with_calls"], 1);
    }

    /// §15: slow is not the same as provider-slow. A run whose every call answers in hundreds of
    /// nanoseconds is not latency-bound however long its span turned out to be, so B is ruled out
    /// by the measured floor and the time goes to the chain instead.
    #[test]
    fn a_run_of_cheap_calls_is_the_chain_s_bottleneck_not_the_provider_s() {
        // 0→100 and 100→1000: fully serial, whole span covered, both calls cheap.
        let diagnosis = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 100, Some("storage|a")),
                call(2, "eth_getStorageAt", 100, 1_000, Some("storage|b")),
            ],
        );
        let block = source_block(
            &bottleneck_classification(&[acquisition_row(&diagnosis)], None),
            "live",
        );
        let b = category(&block, "B");
        assert_eq!(b["figures"]["fastest_call_ns"], 100);
        assert_eq!(
            b["verdict"],
            json!(RULED_OUT),
            "the cheapest call is under the declared \
                                                    floor, which is §15's whole point"
        );
        assert_eq!(
            category(&block, "C")["verdict"],
            json!(DOMINANT),
            "and the wait that is left is the serial chain: 1 000‰ of the span had exactly one \
             call in flight"
        );
        assert_eq!(block["dominant_span_categories"], json!(["C"]));
        assert_eq!(block["primary"], json!("C"));
        assert_eq!(block["primary_name"], json!("Serial dependency bound"));
        assert_eq!(
            block["secondary"],
            json!(["A"]),
            "A is material ({})",
            category(&block, "A")["share_per_mille"]["per_mille"]
        );
        assert_eq!(
            category(&block, "F")["verdict"],
            json!(RULED_OUT),
            "the union covers the whole span, so nothing was left for local compute"
        );
    }

    /// The same shape priced at or above the floor: B is no longer ruled out, and here it and C
    /// and A all hold the span, which is what a real serial chain of slow answers looks like.
    /// §25's G is the honest word for three explanations of one interval — none of them is
    /// discarded, all three are listed, and the file says the shares do not add.
    #[test]
    fn a_chain_of_slow_answers_outranks_a_single_explanation_and_says_mixed() {
        let floor = LATENCY_FLOOR_NS;
        let diagnosis = SimulationDiagnosis::new(
            node_window(0, floor * 2),
            vec![
                call(1, "eth_getStorageAt", 0, floor, Some("storage|a")),
                call(2, "eth_getStorageAt", floor, floor * 2, Some("storage|b")),
            ],
        );
        let table = bottleneck_classification(&[acquisition_row(&diagnosis)], None);
        let block = source_block(&table, "live");
        assert_eq!(category(&block, "B")["verdict"], json!(DOMINANT));
        assert_eq!(category(&block, "C")["verdict"], json!(DOMINANT));
        assert_eq!(category(&block, "A")["verdict"], json!(DOMINANT));
        assert_eq!(
            block["dominant_span_categories"],
            json!(["A", "B", "C"]),
            "ranked by share, and the three are equal here so the tie breaks by id"
        );
        assert_eq!(block["primary"], json!("G"));
        assert_eq!(block["secondary"], json!(["A", "B", "C"]));
        assert!(block["primary_rationale"]
            .as_str()
            .unwrap_or_default()
            .contains("3 span-denominated"));
        assert!(table["shares_are_not_additive"]
            .as_str()
            .unwrap_or_default()
            .contains("do not sum"));
    }

    /// §19's expectation, tested both ways: reuse that worked prints D as ruled out with the
    /// regression flag false, and a repeat that survived the boundary prints it as a regression
    /// rather than as a finding about the market.
    #[test]
    fn the_duplicate_category_is_the_regression_detector_for_reuse() {
        let reused = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 100, Some("storage|a")),
                call(2, "eth_getStorageAt", 100, 200, Some("storage|b")),
            ],
        )
        .with_state_reads(Some(named_arm(true)));
        let block = source_block(
            &bottleneck_classification(&[acquisition_row(&reused)], None),
            "live",
        );
        let d = category(&block, "D");
        assert_eq!(d["figures"]["duplicate_state_reads"], 0);
        assert_eq!(d["figures"]["total_state_reads"], 2);
        assert_eq!(d["verdict"], json!(RULED_OUT));
        assert_eq!(d["m8_3_1_regression"], json!(false));
        assert_eq!(block["aggregated_figures"]["state_reads"]["duplicate"], 0);

        let repeated = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 100, Some("storage|a")),
                call(2, "eth_getStorageAt", 100, 200, Some("storage|a")),
            ],
        )
        .with_state_reads(Some(named_arm(true)));
        let repeated_table = bottleneck_classification(&[acquisition_row(&repeated)], None);
        let repeated_d = category(&source_block(&repeated_table, "live"), "D");
        assert_eq!(repeated_d["figures"]["duplicate_state_reads"], 1);
        assert_eq!(
            repeated_d["share_per_mille"]["denominator"], 2,
            "the denominator is the keyed state reads the numerator came out of, not every call"
        );
        assert_eq!(repeated_d["m8_3_1_regression"], json!(true));
        assert_ne!(repeated_d["verdict"], json!(RULED_OUT));
        assert_eq!(repeated_table["state_read_reuse"]["rows_with_reuse_on"], 1);
        assert_eq!(
            repeated_table["state_read_reuse"]["mixed_arms"],
            json!(false)
        );
    }

    /// E is the category §15 asks not to confuse with B. One attempt per call and none failed is
    /// a measurement that the time went to a node answering; a second attempt is what makes the
    /// durations unreadable as node cost.
    #[test]
    fn a_clean_attempt_record_rules_the_connection_out_by_measurement() {
        let clean = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![call(1, "eth_getStorageAt", 0, 500, Some("storage|a"))],
        );
        let block = source_block(
            &bottleneck_classification(&[acquisition_row(&clean)], None),
            "live",
        );
        let e = category(&block, "E");
        assert_eq!(e["figures"]["calls"], 1);
        assert_eq!(e["figures"]["attempts"], 1);
        assert_eq!(e["figures"]["extra_attempts"], 0);
        assert_eq!(e["figures"]["failed_calls"], 0);
        assert_eq!(e["verdict"], json!(RULED_OUT));

        let mut failed = call(1, "eth_getStorageAt", 0, 900, Some("storage|a"));
        failed.success = false;
        failed.error_class = Some(evm_chain::CLASS_SEND_FAILED);
        failed.attempts = vec![
            evm_chain::RpcAttempt {
                started_ns: 0,
                finished_ns: 400,
                duration_ns: 400,
                outcome: evm_chain::CLASS_SEND_FAILED,
            },
            evm_chain::RpcAttempt {
                started_ns: 500,
                finished_ns: 900,
                duration_ns: 400,
                outcome: evm_chain::CLASS_SEND_FAILED,
            },
        ];
        let retried = SimulationDiagnosis::new(node_window(0, 1_000), vec![failed]);
        let retried_block = source_block(
            &bottleneck_classification(&[acquisition_row(&retried)], None),
            "live",
        );
        let retried_e = category(&retried_block, "E");
        assert_eq!(retried_e["figures"]["extra_attempts"], 1);
        assert_eq!(retried_e["figures"]["failed_calls"], 1);
        assert_eq!(
            retried_e["share_per_mille"]["per_mille"], 2_000,
            "two trouble figures over one call: a share over counts may exceed 1000‰, and \
               this one is printed as the integers that made it"
        );
        assert_eq!(retried_e["verdict"], json!(DOMINANT));
    }

    /// F on §13's basis — span minus the union, never span minus the sum — and unmeasured rather
    /// than dominant for a source whose calls were never timed.
    #[test]
    fn local_compute_is_the_span_minus_the_union_and_never_minus_the_sum() {
        // Two overlapping 500 ns calls inside a 1 000 ns span: sum 1 000, union 500.
        // Span minus sum would say the simulation spent no time locally; span minus union says
        // half of it was local, and that is the number this build publishes.
        let overlapping = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 500, Some("storage|a")),
                call(2, "eth_getStorageAt", 0, 500, Some("storage|b")),
            ],
        );
        let block = source_block(
            &bottleneck_classification(&[acquisition_row(&overlapping)], None),
            "live",
        );
        assert_eq!(block["aggregated_figures"]["rpc_sum_duration_ns"], 1_000);
        assert_eq!(block["aggregated_figures"]["rpc_union_duration_ns"], 500);
        assert_eq!(block["aggregated_figures"]["non_rpc_duration_ns"], 500);
        let f = category(&block, "F");
        assert_eq!(f["verdict"], json!(DOMINANT));
        assert_eq!(f["share_per_mille"]["per_mille"], 500);
        assert_eq!(
            category(&block, "C")["verdict"],
            json!(RULED_OUT),
            "overlap is half the span, so the serial wait is not what held this run open"
        );

        // A source with a span and no calls has no union to subtract: its time is not evidence
        // about local compute, and F says so instead of claiming the whole span.
        let dump = SimulationDiagnosis::new(window(0, 1_000_000), Vec::new());
        let dump_block = source_block(
            &bottleneck_classification(&[acquisition_row(&dump)], None),
            "fixture",
        );
        assert_eq!(
            dump_block["aggregated_figures"]["non_rpc_measured"],
            json!(false)
        );
        assert_eq!(category(&dump_block, "F")["verdict"], json!(NOT_MEASURED));
        assert_eq!(
            category(&dump_block, "F")["share_per_mille"]["reason"],
            NO_SHARE_MEASURED
        );
    }

    /// One directory, written by the switch's own two settings. The same three calls go in
    /// each time, so the only difference between the two directories is `state_acquisition`.
    fn write_a_directory(dir: &Path, state_acquisition: bool) -> PathBuf {
        let diagnosis = || {
            SimulationDiagnosis::new(
                node_window(0, 1_000),
                vec![
                    storage_call(1, "0xAA11", "0x01", 0, 300),
                    storage_call(2, "0xBB22", "0x02", 300, 500),
                    account_call(3, "eth_getCode", "0xAA11", 600, 900),
                ],
            )
            .with_state_reads(Some(named_arm(true)))
            .with_endpoint(Some("rpc-0123456789abcdef".to_string()))
        };
        let mut evidence =
            DiagnosisEvidence::open(dir, "revision", "build-only", state_acquisition)
                .expect("the directory opens");
        evidence.generated_at_unix_ms = 1_700_000_000_000;
        evidence.record(diagnosis(), &[]).expect("records");
        evidence.record(diagnosis(), &[]).expect("records again");
        evidence.finish().expect("writes")
    }

    /// A name list in the order a directory listing has to be compared in.
    fn sorted_names(names: &[&str]) -> Vec<String> {
        let mut list: Vec<String> = names.iter().map(|name| name.to_string()).collect();
        list.sort();
        list
    }

    /// §17's switch, tested where it is claimed. A run without it writes M8.2's five files and
    /// nothing else; a run with it writes four more; and the four data files the two directories
    /// share come out byte-identical — only the README changes, and only by gaining the section
    /// that names the four new tables. That is the whole of what this layer can prove about
    /// "the switch cannot change the diagnosis": the analysis a reader already had cannot have
    /// been reworked just because four tables were asked for.
    #[test]
    fn the_switch_adds_four_tables_and_rewrites_none_of_the_other_four() {
        let off = temp_dir("switch-off");
        let on = temp_dir("switch-on");
        let five = [
            TRACES_FILE,
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            README_FILE,
        ];
        let four = [
            STORAGE_BREAKDOWN_FILE,
            ACCOUNT_MATRIX_FILE,
            RPC_GAPS_FILE,
            BOTTLENECK_FILE,
        ];
        let written_off = write_a_directory(&off, false);
        let written_on = write_a_directory(&on, true);
        let names = |dir: &std::path::Path| -> Vec<String> {
            let mut list: Vec<String> = std::fs::read_dir(dir)
                .expect("the directory is readable")
                .map(|entry| {
                    entry
                        .expect("one entry")
                        .file_name()
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            list.sort();
            list
        };
        assert_eq!(
            names(&written_off),
            sorted_names(&five),
            "a directory written without §17's switch grew a file it should not have"
        );
        let nine: Vec<&str> = five.iter().chain(four.iter()).copied().collect();
        assert_eq!(
            names(&written_on),
            sorted_names(&nine),
            "§19 names four more files and one of them is not on disk"
        );
        assert!(
            !names(&written_on).contains(&OUTSIDE_FILE.to_string()),
            "this helper writes no lifecycle half: outside-simulation-rpc.json belongs to §14's \
             sink, not to §17's acquisition switch"
        );

        for name in five {
            if name == README_FILE {
                continue;
            }
            let a = std::fs::read(written_off.join(name)).expect("off side readable");
            let b = std::fs::read(written_on.join(name)).expect("on side readable");
            assert_eq!(
                a, b,
                "{name} differs between a switched-off and a switched-on run given \
                              the same calls — the switch changed an existing file"
            );
        }
        let readme_off = std::fs::read_to_string(written_off.join(README_FILE)).expect("readable");
        let readme_on = std::fs::read_to_string(written_on.join(README_FILE)).expect("readable");
        assert!(
            !readme_off.contains("## Acquisition tables and bottleneck classification"),
            "the README advertises tables the directory does not hold"
        );
        assert!(readme_on.contains("## Acquisition tables and bottleneck classification"));
        for name in four {
            assert!(
                readme_on.contains(name),
                "§23's README does not name {name}"
            );
            assert!(!readme_off.contains(name));
            let text = std::fs::read_to_string(written_on.join(name)).expect("readable");
            let table: Value = serde_json::from_str(&text).expect("{name} is one JSON object");
            assert_eq!(table["git_revision"], "revision");
            assert_eq!(table["diagnosis_schema"], DIAGNOSIS_SCHEMA);
            assert_eq!(table["unit"], "ns");
            assert!(!has_a_float(&table), "{name} carries a float (§13)");
        }
        let _ = std::fs::remove_dir_all(&off);
        let _ = std::fs::remove_dir_all(&on);
    }

    /// The four derived tables go through the same determinism gate as M8.2's five, for the
    /// same reason: each is a regrouping of the lines the run wrote, so two writers given
    /// identical calls must produce identical bytes. A figure that moved between two such runs
    /// was computed a second time, and the directory's own claim that the tables are copies
    /// rather than measurements would be false.
    #[test]
    fn the_acquisition_tables_are_byte_identical_across_two_writers() {
        let first = temp_dir("acquisition-determinism-a");
        let second = temp_dir("acquisition-determinism-b");
        let written_first = write_a_directory(&first, true);
        let written_second = write_a_directory(&second, true);
        for name in [
            TRACES_FILE,
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            README_FILE,
            STORAGE_BREAKDOWN_FILE,
            ACCOUNT_MATRIX_FILE,
            RPC_GAPS_FILE,
            BOTTLENECK_FILE,
        ] {
            let a = std::fs::read(written_first.join(name)).expect("readable");
            let b = std::fs::read(written_second.join(name)).expect("readable");
            assert_eq!(
                a, b,
                "{name} differs between two writers given identical calls"
            );
            assert!(!a.is_empty(), "{name} was written empty");
        }
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }

    /// §5's 「谁最耗时」 answer, with its denominator stated. The ranking is ordered by a
    /// *sum* of per-method call durations, which double-counts an instant two calls shared — so
    /// the headline names the expensive method and says, beside it, that this is not a slice of
    /// the wall clock. Here three calls cost 450 ns of duration inside 350 ns of union.
    #[test]
    fn the_method_ranking_orders_by_summed_duration_and_says_that_is_not_wall_time() {
        let diagnosis = SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                call(1, "eth_getStorageAt", 0, 100, Some("storage|a")),
                call(2, "eth_getStorageAt", 0, 100, Some("storage|b")),
                call(3, "eth_getCode", 400, 650, Some("code|c")),
            ],
        );
        let block = source_block(
            &bottleneck_classification(&[acquisition_row(&diagnosis)], None),
            "live",
        );
        let ranking = block["method_ranking_by_summed_duration"]
            .as_array()
            .expect("a list");
        assert_eq!(ranking.len(), 2);
        assert_eq!(
            ranking[0]["method"], "eth_getCode",
            "250 ns of one call outranks \
                                                        200 ns of two"
        );
        assert_eq!(ranking[1]["method"], "eth_getStorageAt");
        assert_eq!(ranking[0]["calls"], 1);
        assert_eq!(ranking[0]["total_duration_ns"], 250);
        assert_eq!(ranking[0]["mean_call_ns"], 250);
        assert_eq!(ranking[0]["share_of_rpc_sum_per_mille"]["numerator"], 250);
        assert_eq!(ranking[0]["share_of_rpc_sum_per_mille"]["denominator"], 450);
        assert_eq!(ranking[1]["fastest_call_ns"], 100);
        assert_eq!(ranking[1]["slowest_call_ns"], 100);
        assert_eq!(ranking[1]["mean_call_ns"], 100);
        assert_eq!(ranking[1]["share_of_span_per_mille"]["per_mille"], 200);

        let headline = &block["most_time_by_summed_call_duration"];
        assert_eq!(headline["method"], "eth_getCode");
        assert_eq!(headline["total_duration_ns"], 250);
        assert!(headline["not_wall_time"]
            .as_str()
            .unwrap_or_default()
            .contains("double-counts"));
        assert!(headline["method_ranking_field"]
            .as_str()
            .unwrap_or_default()
            .contains("method_ranking_by_summed_duration"));
        // The claim in those two sentences is checkable in the same file: the wall figure this
        // headline refuses to be is a third smaller than the sum it is ranked on.
        assert_eq!(block["aggregated_figures"]["rpc_sum_duration_ns"], 450);
        assert_eq!(block["aggregated_figures"]["rpc_union_duration_ns"], 350);

        let no_methods = acquisition_row(&SimulationDiagnosis::new(window(0, 1_000), Vec::new()));
        let empty = source_block(&bottleneck_classification(&[no_methods], None), "fixture");
        assert_eq!(
            empty["most_time_by_summed_call_duration"],
            Value::Null,
            "a source with no methods has no expensive one; it does not get a row of zeros"
        );
    }

    /// §14's no-double-counting claim is a rule the two sinks satisfy or break, and this is the
    /// file where breaking it would show: a `simulation-state` class in the lifecycle table means
    /// the simulation's sink and the run's sink both listed one call, which would make the
    /// 39-call baseline two different numbers about the same calls.
    #[test]
    fn the_disjointness_check_flags_a_call_that_both_sinks_recorded() {
        let rows = [acquisition_row(&SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![call(1, "eth_getStorageAt", 0, 500, Some("storage|a"))],
        ))];
        let clean = bottleneck_classification(
            &rows,
            Some(&json!({ "per_class": [
                { "class": LIFECYCLE_ORCHESTRATION, "calls": 3 },
                { "class": LIFECYCLE_SIMULATION_STATE, "calls": 0 },
            ] })),
        );
        let check = &clean["sink_disjointness_check"];
        assert_eq!(check["lifecycle_table_present"], json!(true));
        assert_eq!(check["simulation_state_calls"], 0);
        assert_eq!(check["regression"], json!(false));

        let leaking = bottleneck_classification(
            &rows,
            Some(&json!({ "per_class": [
                { "class": LIFECYCLE_SIMULATION_STATE, "calls": 2 },
            ] })),
        );
        let leaking_check = &leaking["sink_disjointness_check"];
        assert_eq!(leaking_check["simulation_state_calls"], 2);
        assert_eq!(leaking_check["regression"], json!(true));

        let absent = bottleneck_classification(&rows, None);
        assert_eq!(
            absent["sink_disjointness_check"]["lifecycle_table_present"],
            json!(false)
        );
        assert_eq!(
            absent["sink_disjointness_check"]["simulation_state_calls"],
            Value::Null
        );
    }

    /// §14's per-class row for one lifecycle class. `category` reads the bottleneck table;
    /// this reads the outside table's own six rows.
    fn class_row(table: &Value, class: &str) -> Value {
        table["per_class"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["class"] == json!(class))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("no `{class}` row in the per-class list"))
    }

    /// The three calls, on a node's window, with a tally and an endpoint — the whole of what
    /// a line has to carry for a replay to rebuild it.
    fn replayable_diagnosis() -> SimulationDiagnosis {
        SimulationDiagnosis::new(
            node_window(0, 1_000),
            vec![
                storage_call(1, "0xAA11", "0x01", 0, 300),
                storage_call(2, "0xBB22", "0x02", 300, 500),
                account_call(3, "eth_getCode", "0xAA11", 600, 900),
            ],
        )
        .with_state_reads(Some(named_arm(true)))
        .with_endpoint(Some("rpc-0123456789abcdef".to_string()))
    }

    /// The lines a directory wrote, as the values they were written from.
    fn lines_of(dir: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(dir.join(TRACES_FILE))
            .expect("the traces a directory just wrote are readable");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }

    /// §19's assembly is built on one claim: a trace line is a complete account of the
    /// simulation it describes. Decoding a line and emitting it again therefore has to give
    /// the same bytes back — timeline, duplicate tally, method rows and both acquisition
    /// lists rebuilt from the calls, not carried across. If any figure were only a copy, the
    /// re-emit would lose it; if any were re-derived twice over differently, the bytes would
    /// move.
    #[test]
    fn a_trace_line_decodes_and_re_emits_the_same_bytes() {
        let refusals = vec!["the sink could not be derived for one call".to_string()];
        let line = replayable_diagnosis().to_trace_line(&refusals);
        let replayed = SimulationDiagnosis::from_trace_line(&line)
            .expect("a line this build wrote is a line this build can read");
        assert_eq!(replayed.events.len(), 3);
        assert_eq!(replayed.timeline.total_calls, 3);
        assert_eq!(replayed.duplicates.duplicate_state_reads, 0);
        assert_eq!(
            replayed.state_reads.as_ref().map(|stats| stats.reuse),
            Some(true)
        );
        assert_eq!(
            replayed.endpoint_id.as_deref(),
            Some("rpc-0123456789abcdef")
        );
        assert_eq!(
            serde_json::to_string(&line).expect("the original serializes"),
            serde_json::to_string(&replayed.to_trace_line(&trace_line_refusals(&line)))
                .expect("the replay serializes")
        );
    }

    /// What a replay refuses, and why: a field this build does not name would otherwise
    /// become a missing call or a zero, and a wrong figure in an evidence file is a finding
    /// nobody can retract afterwards.
    #[test]
    fn a_line_this_build_cannot_read_is_refused_rather_than_guessed() {
        let line = replayable_diagnosis().to_trace_line(&[]);

        let mut schema = line.clone();
        schema["diagnosis_schema"] = json!(DIAGNOSIS_SCHEMA + 1);
        let error = SimulationDiagnosis::from_trace_line(&schema)
            .expect_err("a schema this build has never seen is not decoded by guessing");
        assert!(error.contains("schema"), "{error}");

        let mut trace_schema = line.clone();
        trace_schema["calls"][0]["trace_schema"] = json!(evm_chain::RPC_TRACE_SCHEMA + 1);
        assert!(SimulationDiagnosis::from_trace_line(&trace_schema).is_err());

        let mut class = line.clone();
        class["calls"][0]["error_class"] = json!("mystery_failure");
        let error = SimulationDiagnosis::from_trace_line(&class).expect_err(
            "an error class outside the closed set is not a class this summary can group",
        );
        assert!(error.contains("mystery_failure"), "{error}");

        let mut note = line.clone();
        note["calls"][0]["key_note"] = json!("borrowed_from_somewhere_else");
        assert!(SimulationDiagnosis::from_trace_line(&note).is_err());

        let mut window_end = line.clone();
        window_end["finished_ns"] = json!(null);
        assert!(
            SimulationDiagnosis::from_trace_line(&window_end).is_err(),
            "a window with no end has no duration, and `null` is not a substitute for one"
        );

        let mut source = line.clone();
        source["source"] = json!("production");
        assert!(SimulationDiagnosis::from_trace_line(&source).is_err());

        let mut cache = line.clone();
        cache["state_read_cache"] = json!({ "reason": STATE_READS_UNAVAILABLE });
        let replayed = SimulationDiagnosis::from_trace_line(&cache)
            .expect("a line that refused the tally is readable, with the tally absent");
        assert_eq!(replayed.state_reads, None);
    }

    /// The assembly's own gate, on a scratch pair: a directory written by replaying another
    /// directory's lines is byte-for-byte the directory it replayed. Every table in §19 is
    /// then shown to be a function of the calls, not of the run that happened to be live when
    /// they were made.
    #[test]
    fn replaying_a_directorys_lines_reproduces_the_directory() {
        let source = temp_dir("replay-source");
        write_a_directory(&source, true);
        let target = temp_dir("replay-target");
        let mut evidence = DiagnosisEvidence::open(&target, "revision", "build-only", true)
            .expect("the replay directory opens");
        evidence.generated_at_unix_ms = 1_700_000_000_000;
        for line in lines_of(&source) {
            evidence
                .replay(&line)
                .expect("a line of the source replays");
        }
        evidence.finish().expect("the replay writes");
        for name in [
            TRACES_FILE,
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            STORAGE_BREAKDOWN_FILE,
            ACCOUNT_MATRIX_FILE,
            RPC_GAPS_FILE,
            BOTTLENECK_FILE,
            README_FILE,
        ] {
            assert_eq!(
                std::fs::read(source.join(name)).expect("the source file is readable"),
                std::fs::read(target.join(name)).expect("the replayed file is readable"),
                "{name} is not what the run that recorded these calls wrote"
            );
        }
    }

    /// §14's rows for one run, as an assembly receives them: a method, a duration and the
    /// class that run's own stage spans decided.
    fn lifecycle_row(method: &str, duration_ns: u64, class: &str) -> Value {
        json!({
            "method": method,
            "duration_ns": duration_ns,
            "class": class,
            "stage": "observation",
            "started_ns": 0,
        })
    }

    /// One source run's §14 material, as its file holds it.
    fn source_run(name: &str, endpoint: &str, rows: Vec<Value>) -> Value {
        json!({
            "run": name,
            "endpoint_id": endpoint,
            "generated_at_unix_ms": 1_700_000_000_000_u64,
            "calls": rows.len(),
            "stage_spans": [
                { "stage": "observation", "started_ns": 0, "ended_ns": 1_000 }
            ],
            "rows": rows,
        })
    }

    /// §19's merged `outside-simulation-rpc.json`: the same grouping over pooled rows, and
    /// each run's clock kept with its own run. Two runs' `started_ns` are offsets of two
    /// different monotonic origins, so the spans cannot be laid on one list — and a reader
    /// has to be able to tell which run's `class_basis` any pooled row was decided by.
    #[test]
    fn an_assembled_outside_table_pools_rows_and_keeps_each_run_on_its_own_clock() {
        let first = source_run(
            "run-001",
            "rpc-aaaa",
            vec![
                lifecycle_row("eth_blockNumber", 100, LIFECYCLE_SIMULATION_CONTEXT),
                lifecycle_row("eth_getBalance", 200, LIFECYCLE_DETECTION),
            ],
        );
        let second = source_run(
            "run-002",
            "rpc-aaaa",
            vec![lifecycle_row("eth_getBalance", 300, LIFECYCLE_DETECTION)],
        );
        let table = outside_simulation_table_assembled(&[first, second], 117);
        assert_eq!(table["calls"], json!(3));
        assert_eq!(table["duration_total_ns"], json!(600));
        assert_eq!(class_row(&table, LIFECYCLE_DETECTION)["calls"], json!(2));
        assert_eq!(
            class_row(&table, LIFECYCLE_DETECTION)["duration_total_ns"],
            json!(500),
            "the two runs' detection reads are one class and their durations add"
        );
        assert_eq!(
            class_row(&table, LIFECYCLE_SIMULATION_CONTEXT)["calls"],
            json!(1)
        );
        assert_eq!(
            class_row(&table, LIFECYCLE_SIMULATION_STATE)["calls"],
            json!(0),
            "an absent class still gets its row, so §14's six are all answerable"
        );
        assert_eq!(
            table["core_state_reads_not_included"]["simulation_calls_recorded_elsewhere"],
            json!(117)
        );
        assert_eq!(
            table["assembled_from"].as_array().map(|list| list
                .iter()
                .filter(|run| run["run"] == json!("run-001"))
                .count()),
            Some(1)
        );
        assert_eq!(
            table["assembled_from"][0]["stage_spans"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "each run carries its own spans rather than a pooled set"
        );
        assert!(table.get("stage_spans").is_none());
        let runs: Vec<&str> = table["rows"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|row| row["assembled_from_run"].as_str().unwrap_or("?"))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(runs, vec!["run-001", "run-001", "run-002"]);

        let two_endpoints = outside_simulation_table_assembled(
            &[
                source_run("run-001", "rpc-aaaa", vec![]),
                source_run("run-002", "rpc-bbbb", vec![]),
            ],
            0,
        );
        assert_eq!(
            two_endpoints["endpoint_id"],
            Value::Null,
            "two digests is a finding about two providers, not one identity to print"
        );
    }

    /// §19's assembly provenance has to be in the header every file carries, not in one file
    /// a reader has to know to open — and the directory's one wall-clock stamp is its
    /// sources', which is what lets the same runs be assembled twice to the same bytes.
    #[test]
    fn an_assembled_directory_names_its_runs_in_every_file() {
        let source = temp_dir("assembly-source");
        write_a_directory(&source, true);
        let runs = vec![json!({
            "run": "run-001",
            "simulations": 2,
            "calls": 6,
            "git_revision": "revision",
            "generated_at_unix_ms": 1_700_000_000_000_u64,
        })];
        let dir = temp_dir("assembly-header");
        let mut evidence =
            DiagnosisEvidence::open(&dir, "revision", "build-only", true).expect("opens");
        evidence.assemble_from(1_700_000_000_000, runs);
        for line in lines_of(&source) {
            evidence.replay(&line).expect("a line replays");
        }
        evidence.finish().expect("writes");
        for name in [
            SIMULATION_SUMMARY_FILE,
            RPC_SUMMARY_FILE,
            DUPLICATES_FILE,
            STORAGE_BREAKDOWN_FILE,
            ACCOUNT_MATRIX_FILE,
            RPC_GAPS_FILE,
            BOTTLENECK_FILE,
        ] {
            let table: Value = serde_json::from_str(
                &std::fs::read_to_string(dir.join(name)).expect("the table is readable"),
            )
            .expect("the table is JSON");
            assert_eq!(table["generated_at_unix_ms"], json!(1_700_000_000_000_u64));
            assert_eq!(
                table["assembled_from"][0]["run"],
                json!("run-001"),
                "{name}"
            );
        }
        let readme =
            std::fs::read_to_string(dir.join(README_FILE)).expect("the README is readable");
        assert!(
            readme.contains("## How this directory was assembled"),
            "the README has to say the directory is not one run's"
        );
        assert!(readme.contains("run-001 (2 simulation(s), 6 call(s))"));
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
