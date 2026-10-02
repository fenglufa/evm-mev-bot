//! M8.1 §25–§29, §38: the trace recorder and the latency evidence directory.
//!
//! Two pieces in one file because they are the two ends of the same bypass.
//! [`TraceRecorder`] is what a running lifecycle holds: it turns a stage boundary
//! the code already passes into one map write, and does *nothing at all* — not even
//! a clock read — when the run was not asked to measure. [`LatencyEvidence`] is what
//! the run writes: `traces.jsonl`, one trace per line, and `summary.json`, the
//! per-source percentile tables §25 names, plus the directory's `README.md`.
//!
//! Why a recorder rather than letting each stage hold a `LatencyTrace`: every call
//! below sits on a path that already has to work, and §2.1 says the measurement must
//! not be able to change that path. So a refused write is remembered as a string and
//! travels out in the trace's own line as `instrumentation_refusals` — never dropped,
//! never a panic, never returned to a caller whose job is to decide something (§35).
//! There is no shared state and no lock anywhere in this module (§39): a trace is
//! owned by the lifecycle that made it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use evm_execution::{ExecutionMode, ExecutionRecord, ExecutionStatus};
use evm_metrics::{
    unix_ms, BaselineSet, Clock, Domain, Granularity, LatencyTrace, PipelineTiming, Stage,
    StageRecord,
};
use serde_json::{json, Value};

use crate::error::{PipelineError, Result};

/// The commit this binary was built from, recorded by whoever compiled it
/// (§44's `git_revision`). It comes from the environment rather than from asking
/// git at run time, because an evidence file should name the revision the *binary*
/// was made from, not the checkout the reader happens to be standing in:
/// `GIT_REVISION=$(git rev-parse HEAD) cargo build` puts the real revision in, and
/// a build that did not say one says `unrecorded` in the file rather than inventing
/// a value. `build.rs` declares the variable to cargo, so a build that changes it
/// recompiles instead of reusing an artifact baked with a different revision.
pub fn git_revision() -> &'static str {
    option_env!("GIT_REVISION").unwrap_or("unrecorded")
}

/// The three files §25 names.
pub const TRACES_FILE: &str = "traces.jsonl";
pub const SUMMARY_FILE: &str = "summary.json";
pub const README_FILE: &str = "README.md";

/// §42.8's limitation, stated next to the number it applies to: this build learns
/// that a transaction was included and reads its receipt from the same RPC answer,
/// so §23's two hops have one observable instant here. Separating them would mean a
/// second query, and §15 forbids buying a latency point with one.
pub const RECEIPT_NOT_SEPARABLE: &str = "this build learns inclusion and its receipt from one \
     eth_getTransactionReceipt answer, so the two hops §23 splits apart share one observable \
     instant here; splitting them would need a second query, which §15 forbids";

/// §22's caveat, on the one record it applies to: a revert is an inclusion with a failed
/// status, so the span is real and the outcome is not a success.
pub const REVERTED_INCLUSION: &str = "the receipt's status was a revert: the chain included and \
     executed these bytes and they failed, so this span is a failed inclusion rather than a \
     successful one";

/// §11's honest zero, on the one stage of the discovery half that has one: the engine
/// takes one stamp for "this block's events decoded" and "this block's reserves are
/// applied", because the replay call that returns the second is what produced the first.
/// The figure is therefore the resolution of the stamp rather than a claim that applying
/// state costs nothing, and it travels beside the number so a reader of a percentile
/// table sees the caveat with the sample.
pub const DECODE_AND_APPLY_STAMP: &str = "this build stamps a block decoded and its state \
     applied at one instant, because both are the answer to one replay call: the span is the \
     resolution of the stamp, not a measurement that applying state costs no time";

/// A millisecond pair whose two stamps are one instant. §9 forbids a *skipped* stage
/// from reporting 0 ms; this is the other half of the same honesty rule — a stage that
/// did run and did answer can legitimately land inside one millisecond, and the number
/// then measures the clock, not a stage that costs nothing. Without the sentence beside
/// it a reader sums a 0 into a budget and concludes the observation is free.
pub const SAME_MILLISECOND: &str = "both stamps are the same millisecond, so this span is the \
     resolution of this clock rather than a measurement that this stage costs no time";

/// §20 asks Preflight to be its own stage. On the route path it is, measured
/// (`crates/pipeline/src/arbitrage.rs`); on the live path this build goes from the risk
/// decision straight to the lane, so the stage has no span to read and is recorded as
/// the absence it is rather than folded into the decision that preceded it.
pub const NO_GATE_ON_LIVE: &str = "this build runs §26's preflight on the route path only: a \
     live run handed its risk decision straight to the execution lane, so no gate span exists \
     on this lifecycle";

/// §11's override sentence, on the one stage whose span can hold a node: the state a
/// simulation computes on may arrive over RPC *while the span is open*, so filing that
/// duration under local processing would count a node's latency as this binary's CPU
/// time. The run's own `state_source` string follows this sentence in the note, so the
/// claim names the field it read rather than asking the reader to trust the label.
pub const STATE_READ_INSIDE_SPAN: &str = "the state this span computed on was read from a node \
     while the span was open, so §11 files it as reads-and-compute rather than as local \
     processing; the run's state source is ";

/// §42.2's recorder. A `None` trace means this run measures nothing: each method
/// below becomes a no-op, including the clock read, so a run without the flag takes
/// the same code path it took before this module existed (§40).
#[derive(Debug)]
pub struct TraceRecorder {
    clock: Clock,
    trace: Option<LatencyTrace>,
    refusals: Vec<String>,
}

/// What a recorder held when its lifecycle ended.
pub struct RecordedTrace {
    pub trace: Option<LatencyTrace>,
    pub session_id: Option<String>,
    /// The writes this instrumentation refused, in the order it refused them.
    ///
    /// A refusal is a fact about the instrumentation — a stage written twice, or
    /// written after the trace closed — and it is reported as one rather than being
    /// dropped or turned into a crash. It says nothing about the run's verdict.
    pub refusals: Vec<String>,
}

impl TraceRecorder {
    /// A recorder that records nothing.
    pub fn off(clock: Clock) -> Self {
        Self {
            clock,
            trace: None,
            refusals: Vec::new(),
        }
    }

    /// A recorder for one named opportunity's lifecycle.
    pub fn on(clock: Clock, trace: LatencyTrace) -> Self {
        Self {
            clock,
            trace: Some(trace),
            refusals: Vec::new(),
        }
    }

    pub fn is_on(&self) -> bool {
        self.trace.is_some()
    }

    /// The clock this recorder stamps from, so a caller that already holds a reading
    /// (M5's `PipelineTiming`, M7's `detected_at`) can hand it over instead of taking
    /// a second one for the same instant.
    pub fn clock(&self) -> Clock {
        self.clock
    }

    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    /// Open a stage at the current instant.
    pub fn begin(&mut self, stage: Stage) {
        if self.is_on() {
            let at = self.clock.now_ns();
            self.begin_at(stage, at);
        }
    }

    pub fn begin_at(&mut self, stage: Stage, started_ns: u64) {
        if let Some(trace) = self.trace.as_mut() {
            let outcome = trace.begin(stage, started_ns);
            self.defer(outcome, stage, "open");
        }
    }

    /// Open a stage from a millisecond stamp the evidence path already wrote.
    pub fn begin_ms(&mut self, stage: Stage, started_ms: u64) {
        if let Some(trace) = self.trace.as_mut() {
            let outcome = trace.begin_ms(stage, started_ms);
            self.defer(outcome, stage, "open at millisecond resolution");
        }
    }

    /// Close a stage at the current instant.
    pub fn end(&mut self, stage: Stage) {
        if self.is_on() {
            let at = self.clock.now_ns();
            self.end_at(stage, at);
        }
    }

    pub fn end_at(&mut self, stage: Stage, ended_ns: u64) {
        if let Some(trace) = self.trace.as_mut() {
            let outcome = trace.complete(stage, ended_ns);
            self.defer(outcome, stage, "close");
        }
    }

    /// §11: say on the row this recorder already wrote whether that span is this
    /// process working or a node answering while the span was open.
    ///
    /// `state_source` is the string the run itself carried into its evidence —
    /// `RpcStateProvider` names itself `rpc:chain-N`, a dump names its file — so this
    /// is not a second policy about where state comes from, and it is not a guess from
    /// the stage's name. Simulation is the one stage that needs it: it computes on
    /// state, and on the route path every account, code and storage slot it computes on
    /// arrives over RPC inside the span, which the stage's nominal class would file
    /// under CPU.
    ///
    /// A stage this lifecycle never measured is left exactly as it stands: a skip has no
    /// span to class, and refusing the write here would report an instrumentation fault
    /// for what is only a lifecycle that stopped early.
    pub fn classify_reads(&mut self, stage: Stage, state_source: &str) {
        let Some(domain) = read_domain(state_source) else {
            return;
        };
        let measured = self
            .trace()
            .and_then(|trace| trace.stage(stage))
            .is_some_and(|record| record.duration_ns.is_some());
        if !measured {
            return;
        }
        let note = format!("{STATE_READ_INSIDE_SPAN}{state_source}");
        if let Some(trace) = self.trace.as_mut() {
            let outcome = trace.reclassify(stage, domain, &note);
            self.defer(outcome, stage, "class a span");
        }
    }

    /// Close a stage as having run and not answered — §16's requirement that a
    /// revert, a timeout and an error each terminate the trace, each with its own
    /// state rather than as a silent absence.
    pub fn fail(&mut self, stage: Stage, note: &str) {
        if let Some(trace) = self.trace.as_mut() {
            let at = self.clock.now_ns();
            let outcome = trace.fail(stage, at, note);
            self.defer(outcome, stage, "fail");
        }
    }

    /// §9: the lifecycle did not reach this stage. A skip carries a reason and gets
    /// no duration, which is what keeps it from being averaged as a fast stage.
    pub fn skip(&mut self, stage: Stage, note: &str) {
        if let Some(trace) = self.trace.as_mut() {
            let outcome = trace.skip(stage, note);
            self.defer(outcome, stage, "skip");
        }
    }

    /// Skip every stage after `last`, in lifecycle order.
    ///
    /// This is the call that makes §34's T2–T5 mean something: a run declined at Risk
    /// does not merely stop writing, it writes that Preflight, Build, Sign and Submit
    /// were never attempted.
    pub fn skip_after(&mut self, last: Stage, note: &str) {
        for stage in Stage::ALL {
            if stage > last {
                self.skip(stage, note);
            }
        }
    }

    /// A duration with no instants behind it: M7's `latency_ms` entries, which are
    /// measured figures a run computed and wrote while its two readings were never
    /// written down. §30 forbids reconstructing those, so the record holds a span and
    /// nothing else, at millisecond granularity.
    pub fn duration_ms(&mut self, stage: Stage, duration_ms: Option<u64>, note: &str) {
        let Some(duration_ms) = duration_ms else {
            self.skip(stage, note);
            return;
        };
        if let Some(trace) = self.trace.as_mut() {
            let record =
                StageRecord::duration_only(stage, duration_ms, Granularity::Millisecond, note);
            let outcome = trace.record(record);
            self.defer(outcome, stage, "record a duration");
        }
    }

    /// Place a record the caller assembled. Every method above is a shape this
    /// module already knows how to make; the execution ladder needs two shapes of
    /// its own — a completed span that carries what the span *means* (a revert is an
    /// inclusion that failed), and a stage that was entered and never answered, which
    /// has a start stamp and no end stamp to put a duration on.
    pub fn put(&mut self, record: StageRecord) {
        if let Some(trace) = self.trace.as_mut() {
            let stage = record.stage;
            let outcome = trace.record(record);
            self.defer(outcome, stage, "place a record");
        }
    }

    /// End the lifecycle: stages still open become `Cancelled` (§9) and the trace's
    /// own wall-clock total gets its far end (§10).
    pub fn close(&mut self) {
        if let Some(trace) = self.trace.as_mut() {
            let at = self.clock.now_ns();
            trace.close(at);
        }
    }

    /// Hand the trace out. The recorder is consumed because the lifecycle is over; a
    /// caller that wanted to keep measuring would be describing a second lifecycle,
    /// which needs a second trace (§4).
    pub fn finish(mut self) -> RecordedTrace {
        self.close();
        RecordedTrace {
            trace: self.trace.take(),
            session_id: None,
            refusals: std::mem::take(&mut self.refusals),
        }
    }

    /// The trace, still owned by the recorder — for the live path, where one
    /// trace's stages are written from two places while the block is being processed.
    pub fn trace(&self) -> Option<&LatencyTrace> {
        self.trace.as_ref()
    }

    /// The trace, for the one thing that cannot go through a stage boundary: naming
    /// the opportunity the lifecycle has just found (§4's identity rule).
    pub fn trace_mut(&mut self) -> Option<&mut LatencyTrace> {
        self.trace.as_mut()
    }

    /// One refused write, remembered rather than acted on.
    fn defer(
        &mut self,
        outcome: std::result::Result<(), evm_metrics::TraceError>,
        stage: Stage,
        verb: &str,
    ) {
        if let Err(error) = outcome {
            self.refusals
                .push(format!("could not {verb} stage {stage}: {error}"));
        }
    }
}

impl RecordedTrace {
    /// Name the run this trace belongs to, so a reader holding one line of
    /// `traces.jsonl` can find the session directory that wrote it (§46).
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// The §25 directory: `traces.jsonl` appended one lifecycle at a time, and at the
/// end `summary.json` with the percentile tables and §44's metadata.
///
/// A run's directory is a subdirectory of the one the caller named, named after the
/// caller's session, so an earlier baseline is never rewritten (§46). The files
/// append rather than truncate, which is the same §49 policy M5's evidence writer
/// follows: a run that has to restart must not destroy what it already recorded.
pub struct LatencyEvidence {
    dir: PathBuf,
    set: BaselineSet,
    traces: File,
    count: usize,
}

impl LatencyEvidence {
    /// Open `dir` for one run's traces.
    pub fn open(dir: &Path, git_revision: &str, execution_mode: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|error| PipelineError::Evidence {
            path: dir.to_path_buf(),
            detail: format!("the latency directory could not be created: {error}"),
        })?;
        let path = dir.join(TRACES_FILE);
        let traces = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| PipelineError::Evidence {
                path,
                detail: format!("the latency traces could not be opened for appending: {error}"),
            })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            // §44's metadata. `generated_at` is the one wall-clock reading in these
            // files and is used for nothing else: no duration here is computed from
            // it, because every figure in the tables is monotonic (§2.2).
            set: BaselineSet::new(git_revision, execution_mode, unix_ms()),
            traces,
            count: 0,
        })
    }

    /// Fold one finished lifecycle into the file and the tables.
    pub fn record(&mut self, recorded: RecordedTrace) -> Result<()> {
        let RecordedTrace {
            trace,
            session_id,
            refusals,
        } = recorded;
        let Some(trace) = trace else {
            return Ok(());
        };
        let trace_id = trace.trace_id().to_string();
        self.set.record(&trace);
        self.count += 1;
        let mut line = trace.to_json();
        if let Some(object) = line.as_object_mut() {
            object.insert("session_id".to_string(), json!(session_id));
            object.insert("instrumentation_refusals".to_string(), json!(refusals));
        }
        let text = serde_json::to_string(&line).map_err(|error| PipelineError::Evidence {
            path: self.dir.join(TRACES_FILE),
            detail: format!("trace {trace_id} does not serialize: {error}"),
        })?;
        self.traces
            .write_all(format!("{text}\n").as_bytes())
            .and_then(|_| self.traces.flush())
            .map_err(|error| write_failed(TRACES_FILE, error))?;
        Ok(())
    }

    /// Write the tables and the README. Returns the directory, which is what the
    /// run's own record names so a reader can find the traces.
    ///
    /// Both files are written whole, through a temporary name, for §49's reason: a
    /// `summary.json` that stops mid-object reads like evidence about a completed
    /// baseline when it is not.
    pub fn finish(&mut self) -> Result<PathBuf> {
        let summary = self.set.to_json();
        self.write_whole(SUMMARY_FILE, &summary)?;
        let readme = readme(&summary, self.count);
        std::fs::write(self.dir.join(README_FILE), readme)
            .map_err(|error| write_failed(README_FILE, error))?;
        Ok(self.dir.clone())
    }

    fn write_whole(&mut self, name: &str, value: &Value) -> Result<()> {
        let text =
            serde_json::to_string_pretty(value).map_err(|error| PipelineError::Evidence {
                path: self.dir.join(name),
                detail: format!("the latency summary does not serialize: {error}"),
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

    pub fn traces_recorded(&self) -> usize {
        self.count
    }
}

/// The execution half, read off the ladder the lane already stamped (§15).
///
/// Every figure here comes from a stamp M6/M7 wrote for their own reasons, on the
/// same monotonic clock this run uses — which is what lets a baseline carry a
/// build/sign/submit latency without pretending to a nanosecond *resolution* it did
/// not take: each record's own `granularity` field says millisecond.
///
/// The three-way rule per rung pair is §9's, and it is what makes §34's T5–T7
/// expressible without a second measurement:
///
/// - both stamps → the stage completed, and the span is their difference;
/// - the entering stamp and no leaving stamp → the stage **ran and did not answer**
///   (`Failed`, no duration — the ladder holds no instant for the moment it gave up,
///   and inventing one is §46's forbidden synthetic timestamp);
/// - no entering stamp → the lifecycle never reached it (`Skipped`, `detail` says why).
///
/// The middle state needs one more question answered before it can be believed: a
/// `build-only` run has a `built_at_ms` and no `signed_at_ms` because its mode forbids
/// reading a key, not because signing ran and gave up. So `mode` is what §19/§20's own
/// predicates say about the stage, and a rung above that ceiling is `Skipped` like any
/// other unattempted stage (§9) — otherwise M7's default mode would put a `Failed` Sign
/// in every one of its traces and a reader would count failures that were policy.
///
/// A revert is the one case that is *not* a missing stamp: [`ExecutionRecord::advance`]
/// stamps `included_at_ms` for a reverted receipt, because the chain did include and
/// execute the bytes. So Inclusion reads as completed with a note saying it failed —
/// a fast revert must not quietly become a fast success in a latency table.
pub fn record_ladder(
    recorder: &mut TraceRecorder,
    record: Option<&ExecutionRecord>,
    mode: ExecutionMode,
    detail: &str,
) {
    let Some(record) = record else {
        // The attempt never became a record: nothing was built, so nothing in the
        // execution half ran.
        for stage in [
            Stage::Build,
            Stage::Sign,
            Stage::Submit,
            Stage::Inclusion,
            Stage::Receipt,
            Stage::Settlement,
            Stage::ProfitVerification,
        ] {
            recorder.skip(stage, &never_reached(detail));
        }
        return;
    };
    let reverted = record.status == ExecutionStatus::Reverted;
    // `entered` is the witness that the stage began at all. For six of the seven it is the
    // previous rung's stamp, which the ladder only writes once this stage has succeeded in
    // producing it. Build is the one that has no such witness: `created_at_ms` is the record
    // opening, and §26's gate runs between that and the first bytes, so a record with no
    // `built_at_ms` cannot be told apart from one whose builder ran and failed. It is recorded
    // as not reached, with the report's own reason beside it, rather than as a span this
    // build never measured.
    rung(
        recorder,
        Stage::Build,
        record.built_at_ms.map(|_| record.created_at_ms),
        record.built_at_ms,
        None,
        mode,
        detail,
    );
    rung(
        recorder,
        Stage::Sign,
        record.built_at_ms,
        record.signed_at_ms,
        None,
        mode,
        detail,
    );
    rung(
        recorder,
        Stage::Submit,
        record.signed_at_ms,
        record.submitted_at_ms,
        None,
        mode,
        detail,
    );
    rung(
        recorder,
        Stage::Inclusion,
        record.submitted_at_ms,
        record.included_at_ms,
        reverted.then_some(REVERTED_INCLUSION),
        mode,
        detail,
    );
    recorder.skip(Stage::Receipt, RECEIPT_NOT_SEPARABLE);
    rung(
        recorder,
        Stage::Settlement,
        record.included_at_ms,
        record.settled_at_ms,
        None,
        mode,
        detail,
    );
    rung(
        recorder,
        Stage::ProfitVerification,
        record.settled_at_ms,
        record.profit_verified_at_ms,
        None,
        mode,
        detail,
    );
}

/// The discovery half, read off the stamps M5 already writes (§15).
///
/// [`PipelineTiming`] is carried with the block and then with the job, and its eight
/// instants are taken on this run's own monotonic clock for M5's reasons. So the
/// observation→risk part of a lifecycle needs no new clock reading, no extra RPC and
/// no re-ordering of any stage to measure: this function only re-describes stamps that
/// are already on their way to `blocks.jsonl`, which is exactly what §15 asks for and
/// what makes its granularity `millisecond` rather than a nanosecond claim.
///
/// The pairs are the engine's own stage order (`crates/pipeline/src/engine.rs`,
/// [`crate::engine::MarketEngine::on_canonical`]) and the simulation worker's two
/// stamps (`crates/pipeline/src/sim.rs`):
///
/// ```text
/// Observation            received_at   → decoded_at
/// StateUpdate            decoded_at    → state_updated_at
/// GraphUpdate            state_...     → graph_updated_at
/// OpportunityDetection   graph_...     → opportunity_detected_at
/// Simulation             started_at    → finished_at
/// Risk                   finished_at   → risk_decided_at
/// ```
///
/// One of those pairs describes one instant rather than a span and says so in its note
/// ([`DECODE_AND_APPLY_STAMP`]) instead of pretending to a measurement this build does
/// not have. Any other pair whose two stamps fall inside one millisecond carries
/// [`SAME_MILLISECOND`] for the same reason: the row's 0 is a reading of this clock, and
/// a reader who sums it into a budget would learn that the stage is free.
///
/// Every stage with both instants gets a span; the answer is the last one it timed,
/// which is where this lifecycle's story ends if it ended above the decision. A stage
/// missing either instant is left unwritten here — the caller's `skip_after` gives it
/// the row it deserves with this run's own reason beside it, which is §9's state for a
/// stage a lifecycle never got through, rather than this function guessing at a state
/// the timing record cannot support.
/// One discovery stage's row as this function reads it: the stage, the instant it began,
/// the instant it answered, and the caveat it carries beyond the difference of the two.
type DiscoveryPair = (Stage, Option<u64>, Option<u64>, Option<&'static str>);

pub fn record_discovery(recorder: &mut TraceRecorder, timing: &PipelineTiming) -> Option<Stage> {
    let pairs: [DiscoveryPair; 6] = [
        (
            Stage::Observation,
            Some(timing.received_at),
            timing.decoded_at,
            None,
        ),
        (
            Stage::StateUpdate,
            timing.decoded_at,
            timing.state_updated_at,
            Some(DECODE_AND_APPLY_STAMP),
        ),
        (
            Stage::GraphUpdate,
            timing.state_updated_at,
            timing.graph_updated_at,
            None,
        ),
        (
            Stage::OpportunityDetection,
            timing.graph_updated_at,
            timing.opportunity_detected_at,
            None,
        ),
        (
            Stage::Simulation,
            timing.simulation_started_at,
            timing.simulation_finished_at,
            None,
        ),
        (
            Stage::Risk,
            timing.simulation_finished_at,
            timing.risk_decided_at,
            None,
        ),
    ];
    let mut last = None;
    for (stage, entered, ended, note) in pairs {
        // The stamps are taken in lifecycle order, so the first pair without both is
        // the first stage this run never got through, and nothing after it can be
        // timed from this record either.
        let (Some(started), Some(finished)) = (entered, ended) else {
            break;
        };
        recorder.put(StageRecord {
            note: span_note(note, started, finished),
            ..StageRecord::measured_ms(stage, started, finished)
        });
        last = Some(stage);
    }
    last
}

/// A whole lifecycle for a run that goes source → state → graph → opportunity →
/// simulation → risk: the discovery half off [`PipelineTiming`], §20's gate as the
/// absence it is on this path, and the execution ladder only where a lane left a record.
///
/// This is §14's separation stated inside one trace rather than across two files: a
/// finding's own latencies are timed whether or not anything was ever built, and the
/// stages below the decision answer for a transaction this opportunity did not have.
///
/// A lifecycle that stopped above the risk decision — the simulation refused, the
/// finding went stale while its own run was in flight — gets its reason on every row
/// after its last timed stage, and nothing is claimed about a gate or a ladder it never
/// reached.
///
/// `mode` is the execution lane's own, and it reaches only the ladder: a run whose mode
/// forbids a rung gets that rung as §9's `Skipped`, never as a stage that ran and failed
/// to answer. With no lane at all the caller passes the default mode, which is also what
/// a lane-less run attempts — nothing below Risk.
pub fn record_lifecycle(
    recorder: &mut TraceRecorder,
    timing: &PipelineTiming,
    record: Option<&ExecutionRecord>,
    mode: ExecutionMode,
    detail: &str,
) {
    match record_discovery(recorder, timing) {
        Some(Stage::Risk) => {
            recorder.skip(Stage::Preflight, NO_GATE_ON_LIVE);
            record_ladder(recorder, record, mode, detail);
        }
        Some(last) => recorder.skip_after(last, detail),
        // Not one stage of the discovery half has two stamps: this run never carried a
        // block through its own decoding, so every stage is a skip.
        None => {
            for stage in Stage::ALL {
                recorder.skip(stage, &never_reached(detail));
            }
        }
    }
}

/// The note a measured row carries: the caller's caveat where it gave one, and otherwise
/// the resolution sentence when its two stamps are one instant. A pair that already
/// carries its own caveat is left alone — [`DECODE_AND_APPLY_STAMP`] and the history
/// loader's [`crate::history::SAME_INSTANT`] both already say what this would add.
fn span_note(note: Option<&str>, started: u64, ended: u64) -> Option<String> {
    match note {
        Some(existing) => Some(existing.to_string()),
        None if started == ended => Some(SAME_MILLISECOND.to_string()),
        None => None,
    }
}

/// One rung pair of the execution ladder, placed under whichever of §9's states the two
/// stamps and the run's mode describe. `entered` is the instant the stage began, present
/// only when a stamp proves it began; `ended` is the instant it handed the next rung its
/// stamp. `note` is what the reader needs beyond the numbers — the revert, in M7's case —
/// and `detail` is why this attempt stopped where it did. Two equal stamps get
/// [`SAME_MILLISECOND`] when the caller gave no note of its own.
fn rung(
    recorder: &mut TraceRecorder,
    stage: Stage,
    entered: Option<u64>,
    ended: Option<u64>,
    note: Option<&str>,
    mode: ExecutionMode,
    detail: &str,
) {
    match (entered, ended) {
        (Some(started), Some(ended)) => recorder.put(StageRecord {
            note: span_note(note, started, ended),
            ..StageRecord::measured_ms(stage, started, ended)
        }),
        (Some(started), None) if attempts(mode, stage) => recorder.put(StageRecord::failed_ms(
            stage,
            started,
            &format!("this stage was entered and never gave the ladder its next rung: {detail}"),
        )),
        (Some(_), None) => recorder.skip(stage, &not_attempted(mode, detail)),
        (None, _) => recorder.skip(stage, &never_reached(detail)),
    }
}

/// Whether a run's state source means a node answered *inside* the span that read it.
///
/// The two providers that exist name themselves: `RpcStateProvider` is `rpc:chain-N`
/// (`crates/simulation/src/state.rs`) and a dump carries the file it was read from
/// (`dump:fixtures/…`, `crates/pipeline/src/sim.rs`). Nothing else in the workspace
/// produces the string, so this reads a label the provider wrote about itself rather
/// than deciding where state came from a second time.
fn read_domain(state_source: &str) -> Option<Domain> {
    state_source.starts_with("rpc:").then_some(Domain::Mixed)
}

/// Whether this mode ever attempts this stage, read from the two predicates the
/// execution lane itself stops on (§19/§20). Nothing here is a second policy: a stage
/// the lane will not try is a stage the trace must not report as having failed, and the
/// lane's `may_read_key` / `may_submit` are the one definition of that line.
fn attempts(mode: ExecutionMode, stage: Stage) -> bool {
    match stage {
        Stage::Sign => mode.may_read_key(),
        Stage::Submit | Stage::Inclusion | Stage::Settlement | Stage::ProfitVerification => {
            mode.may_submit()
        }
        _ => true,
    }
}

/// §9's wording for a stage above the mode's ceiling: the ladder holds the entering
/// stamp, and what stopped this run was the decision made before it started, not a stage
/// that ran and gave up.
fn not_attempted(mode: ExecutionMode, detail: &str) -> String {
    format!(
        "not attempted: this run's execution mode `{mode}` stops before this stage, so §9's \
         Skipped applies and no duration is claimed; the run's own words are: {detail}"
    )
}

/// §9's wording for a stage of the execution half this run never entered.
fn never_reached(detail: &str) -> String {
    format!("the lifecycle never reached this stage: {detail}")
}

/// The README §25 asks the directory to carry: what each file is, which sources are
/// in it, and the two rules that make a `null` in these files mean something.
fn readme(summary: &Value, traces: usize) -> String {
    let sources: Vec<String> = summary["sources"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    format!(
                        "`{}` — {} trace(s), chain id {}",
                        row["source"].as_str().unwrap_or("unknown"),
                        row["sample_count"].as_u64().unwrap_or(0),
                        row["chain_id"]
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let minimum = |rank: &str| {
        summary["minimum_samples_for_rank"][rank]
            .as_u64()
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    format!(
        "# M8.1 latency baseline\n\n\
         Written by git revision `{}` with execution mode `{}`.\n\n\
         | file | contents |\n\
         | --- | --- |\n\
         | `{TRACES_FILE}` | {traces} line(s): one JSON trace per opportunity lifecycle, always \
         fourteen stages per line, nanoseconds |\n\
         | `{SUMMARY_FILE}` | the same traces folded into per-source percentile tables, plus the \
         §44 metadata |\n\n\
         Sources in this directory: {}.\n\n\
         ## Reading rules\n\n\
         - `null` means **not measured**, never zero. A stage the lifecycle never reached is \
         `skipped` and has no duration; a percentile with too few samples behind it is `null` \
         with an `insufficient_sample` reason.\n\
         - Percentiles are nearest-rank — the same computation M5's `metrics.json` uses. A rank \
         is reported only when its index is strictly below the last sample, so it is never a \
         reprint of `max`: p50 needs {} sample(s), p90 {}, p95 {}, p99 {}.\n\
         - Durations come from one monotonic clock, never from wall time. `granularity` says how \
         finely that stage's instants were actually taken; `millisecond` means the figure is a \
         reading of a stamp the run wrote anyway, not a nanosecond measurement.\n\
         - `live`, `replay` and `fixture` rows are never blended into one percentile, and a \
         fixture's number is a test's latency rather than a market's.\n\
         - `domain` is §11's cost class for the span as it was actually earned, and \
         `nominal_domain` is the class the stage's name suggests; where they differ, the span \
         held a node (a simulation that read its state over RPC), and `totals_ns` files that \
         time under `mixed_reads_and_compute` rather than under `local_processing`.\n\
         - `generated_at_unix_ms` is metadata about the file and enters no duration.\n",
        summary["git_revision"].as_str().unwrap_or("unknown"),
        summary["execution_mode"].as_str().unwrap_or("unknown"),
        if sources.is_empty() {
            "none yet".to_string()
        } else {
            sources.join("; ")
        },
        minimum("p50"),
        minimum("p90"),
        minimum("p95"),
        minimum("p99"),
    )
}

fn write_failed(file: &str, error: std::io::Error) -> PipelineError {
    PipelineError::Evidence {
        path: PathBuf::from(file),
        detail: format!("the latency evidence file could not be written: {error}"),
    }
}

/// §34's T4–T7 at the one seam the execution half enters the trace: a real
/// [`ExecutionRecord`] that climbed a real ladder, folded in by the same
/// [`record_ladder`] a route run calls.
///
/// The stamps are round numbers on purpose. They are inputs to a test of *shaping* —
/// which state a stage gets when its rung is or is not stamped — and nothing written
/// here may be mistaken for a measured figure from M7's evidence (§46).
#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use evm_core::BlockNumber;
    use evm_execution::TransactionIntent;
    use evm_metrics::{StageOutcome, TraceSource};
    use evm_simulation::BlockPin;

    const CHAIN: u64 = 91_342;
    const BLOCK: u64 = 37_503_978;
    /// One millisecond, in the nanoseconds every record is held in.
    const MS: u64 = 1_000_000;

    /// A record opened at `1_000` ms and advanced through these rungs.
    fn ladder(rungs: &[(ExecutionStatus, u64)]) -> ExecutionRecord {
        let intent = TransactionIntent::validation_call(
            BlockPin::new(BlockNumber(BLOCK), B256::left_padding_from(&[7u8; 20])),
            CHAIN,
            Address::from_slice(&[0x11u8; 20]),
            Address::from_slice(&[0x22u8; 20]),
            U256::ZERO,
            21_000,
        )
        .expect("a validation call is an intent");
        let mut record = ExecutionRecord::open(&intent, ExecutionStatus::RiskApproved, 1_000);
        for (rung, at) in rungs {
            record
                .advance(*rung, *at)
                .expect("the ladder accepts this step");
        }
        record
    }

    fn recorder() -> TraceRecorder {
        TraceRecorder::on(
            Clock::new(),
            LatencyTrace::for_block(TraceSource::Live, CHAIN, BLOCK),
        )
    }

    /// The record one stage got, with its outcome resolved or a panic naming the
    /// stage that is missing entirely — a stage the trace never heard of is a wiring
    /// bug in this file, not a §9 state.
    fn record_of(recorder: &TraceRecorder, wanted: Stage) -> StageRecord {
        recorder
            .trace()
            .and_then(|trace| trace.stage(wanted))
            .unwrap_or_else(|| panic!("{} got no record at all", wanted))
            .clone()
    }

    /// §34's T4, in its narrowest case: the attempt never became a record, so every
    /// stage of the execution half is `Skipped` and none of them is zero.
    #[test]
    fn an_attempt_that_never_became_a_record_skips_the_execution_half() {
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            None,
            ExecutionMode::Submit,
            "the gate refused the attempt",
        );
        for stage in [
            Stage::Build,
            Stage::Sign,
            Stage::Submit,
            Stage::Inclusion,
            Stage::Receipt,
            Stage::Settlement,
            Stage::ProfitVerification,
        ] {
            let record = record_of(&recorder, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(record.duration_ns, None, "{stage} must not read as 0 ns");
            assert!(record.note.is_some(), "{stage} must say why");
        }
    }

    /// A record that exists but stopped at the gate: `created_at_ms` is the record
    /// opening, not a builder being called, so Build is still not reached here.
    #[test]
    fn a_run_that_stopped_before_the_first_rung_skips_build() {
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&ladder(&[])),
            ExecutionMode::Submit,
            "the gate refused",
        );
        assert_eq!(
            record_of(&recorder, Stage::Build).outcome,
            StageOutcome::Skipped
        );
    }

    /// §34's T5: the signed bytes were sent and the endpoint never acknowledged them.
    /// Sign has both rungs, Submit has its entering stamp and no leaving one, and
    /// Inclusion is a stage this lifecycle never entered.
    #[test]
    fn a_submit_that_never_answered_is_failed_and_inclusion_is_not_reached() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "the endpoint refused the bytes",
        );

        let sign = record_of(&recorder, Stage::Sign);
        assert_eq!(sign.outcome, StageOutcome::Completed);
        assert_eq!(sign.duration_ns, Some(1_000 * MS));
        assert_eq!(sign.granularity, Some(Granularity::Millisecond));

        let submit = record_of(&recorder, Stage::Submit);
        assert_eq!(submit.outcome, StageOutcome::Failed);
        assert_eq!(submit.started_ns, Some(4_000 * MS));
        assert_eq!(
            submit.duration_ns, None,
            "the ladder holds no instant for when the send gave up, and inventing one \
             would be §46's synthetic timestamp"
        );
        assert_eq!(
            record_of(&recorder, Stage::Inclusion).outcome,
            StageOutcome::Skipped
        );
    }

    /// §9's line that a ladder read without its mode gets wrong: a `build-only` run holds
    /// a `built_at_ms` and no `signed_at_ms` because §19 forbids it reading a key, not
    /// because signing ran and gave up. Those two shapes share the same missing stamp, so
    /// the mode is the only thing that tells them apart — and M7's *default* mode is the
    /// one that stops there, which would put a `Failed` Sign in every trace of a run that
    /// did exactly what it was told.
    #[test]
    fn a_mode_that_stops_before_a_stage_skips_it_instead_of_failing_it() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::BuildOnly,
            "build-only stops at Built",
        );

        let sign = record_of(&recorder, Stage::Sign);
        assert_eq!(sign.outcome, StageOutcome::Skipped);
        assert_eq!(
            sign.duration_ns, None,
            "§9: a skipped stage reports no 0 ms"
        );
        assert_eq!(
            sign.started_ns, None,
            "a stage this mode never attempted did not begin, so it has no instant to hold"
        );
        assert!(
            sign.note
                .as_deref()
                .unwrap_or_default()
                .contains("build-only"),
            "the skip must name the ceiling that caused it: {:?}",
            sign.note
        );
        assert_eq!(
            record_of(&recorder, Stage::Build).outcome,
            StageOutcome::Completed,
            "the mode does attempt Build, and its span stays measured"
        );
    }

    /// The other half of the pair: these same two stamps under the mode that *does* sign
    /// are §9's `Failed`. Read together with the test above, they show the state comes
    /// from the mode and not from the missing stamp.
    #[test]
    fn the_same_missing_rung_under_a_mode_that_signs_is_a_stage_that_never_answered() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "the signer gave up",
        );
        let sign = record_of(&recorder, Stage::Sign);
        assert_eq!(sign.outcome, StageOutcome::Failed);
        assert_eq!(sign.started_ns, Some(3_000 * MS));
        assert_eq!(sign.duration_ns, None);
    }

    /// `sign-only` holds the signed bytes and never sends them (§19's middle mode), so
    /// Submit has its entering stamp and no leaving one — the shape of a send that gave
    /// up — and is still a stage that was not attempted.
    #[test]
    fn bytes_signed_and_kept_leave_submit_unattempted_not_unanswered() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::SignOnly,
            "sign-only keeps the bytes in the process",
        );

        assert_eq!(
            record_of(&recorder, Stage::Sign).outcome,
            StageOutcome::Completed,
            "this mode does attempt signing, and its span is a real measurement"
        );
        for stage in [
            Stage::Submit,
            Stage::Inclusion,
            Stage::Settlement,
            Stage::ProfitVerification,
        ] {
            let skipped = record_of(&recorder, stage);
            assert_eq!(skipped.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(skipped.duration_ns, None, "{stage}");
        }
    }

    /// §34's T6: the node took the bytes and no receipt ever arrived. Of the two
    /// readings §34 offers — `Pending` or `Failed` — this model has one state for "the
    /// stage ran and did not finish", and a receipt tracker that gave up is that.
    #[test]
    fn a_submitted_transaction_without_a_receipt_fails_inclusion() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "no receipt inside the window",
        );

        assert_eq!(
            record_of(&recorder, Stage::Submit).outcome,
            StageOutcome::Completed
        );
        let inclusion = record_of(&recorder, Stage::Inclusion);
        assert_eq!(inclusion.outcome, StageOutcome::Failed);
        assert_eq!(inclusion.started_ns, Some(5_000 * MS));
        assert_eq!(
            record_of(&recorder, Stage::Settlement).outcome,
            StageOutcome::Skipped
        );
    }

    /// §34's T7: the route settled and the profit question never got its final
    /// answer. Only the last stage fails; everything behind it is timed.
    #[test]
    fn a_settled_run_without_a_verdict_fails_only_profit_verification() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
            (ExecutionStatus::Included, 6_000),
            (ExecutionStatus::Settled, 7_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "the profit check is inconclusive",
        );

        assert_eq!(
            record_of(&recorder, Stage::Settlement).outcome,
            StageOutcome::Completed
        );
        let profit = record_of(&recorder, Stage::ProfitVerification);
        assert_eq!(profit.outcome, StageOutcome::Failed);
        assert_eq!(profit.started_ns, Some(7_000 * MS));
    }

    /// A revert is an inclusion the chain did grant, so it is timed — and labelled,
    /// because a latency table that silently files a failed route under "fast" is the
    /// one thing §2.1's precedence rule cannot tolerate.
    #[test]
    fn a_reverted_inclusion_is_still_measured_and_says_it_reverted() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
            (ExecutionStatus::Reverted, 6_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "the receipt status was failure",
        );

        let inclusion = record_of(&recorder, Stage::Inclusion);
        assert_eq!(inclusion.outcome, StageOutcome::Completed);
        assert_eq!(inclusion.duration_ns, Some(1_000 * MS));
        assert_eq!(inclusion.note.as_deref(), Some(REVERTED_INCLUSION));
    }

    /// §23's two hops, one observable instant: the receipt is never a separate
    /// measurement in this build, and the trace says so rather than splitting the
    /// span in half to look complete.
    #[test]
    fn the_receipt_stage_carries_the_reason_it_has_no_number() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
            (ExecutionStatus::Included, 6_000),
            (ExecutionStatus::Settled, 7_000),
            (ExecutionStatus::ProfitVerified, 8_000),
        ]);
        let mut recorder = recorder();
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "the route is accounted for",
        );

        let receipt = record_of(&recorder, Stage::Receipt);
        assert_eq!(receipt.outcome, StageOutcome::Skipped);
        assert_eq!(receipt.note.as_deref(), Some(RECEIPT_NOT_SEPARABLE));
        assert_eq!(receipt.duration_ns, None);
        // The rest of the ladder is six completed spans.
        let completed = [
            Stage::Build,
            Stage::Sign,
            Stage::Submit,
            Stage::Inclusion,
            Stage::Receipt,
            Stage::Settlement,
            Stage::ProfitVerification,
        ]
        .into_iter()
        .filter(|stage| record_of(&recorder, *stage).outcome == StageOutcome::Completed)
        .count();
        assert_eq!(completed, 6, "every rung but the receipt has both instants");
    }

    /// §40's backward-compatibility claim, checked at the seam: a recorder that was
    /// never given a trace takes the same no-ops whatever the ladder says, and reports
    /// no refusals for having been asked.
    #[test]
    fn a_recorder_that_was_told_to_measure_nothing_records_nothing() {
        let record = ladder(&[(ExecutionStatus::Preflighted, 2_000)]);
        let mut recorder = TraceRecorder::off(Clock::new());
        assert!(!recorder.is_on());
        record_ladder(
            &mut recorder,
            Some(&record),
            ExecutionMode::Submit,
            "detail",
        );
        recorder.begin(Stage::Observation);
        recorder.end(Stage::Observation);
        let recorded = recorder.finish();
        assert!(recorded.trace.is_none());
        assert!(recorded.refusals.is_empty(), "{:?}", recorded.refusals);
    }

    /// A timing record in the order the pipeline takes its eight stamps, so a test can
    /// describe a run that stopped at graph update by leaving the rest `None` — which is
    /// exactly what [`crate::engine`] writes for such a run.
    fn timing(stamps: [Option<u64>; 8]) -> PipelineTiming {
        let mut timing = PipelineTiming::new(
            1_700_000_000,
            1_700_000_000_000,
            stamps[0].unwrap_or_default(),
        );
        timing.decoded_at = stamps[1];
        timing.state_updated_at = stamps[2];
        timing.graph_updated_at = stamps[3];
        timing.opportunity_detected_at = stamps[4];
        timing.simulation_started_at = stamps[5];
        timing.simulation_finished_at = stamps[6];
        timing.risk_decided_at = stamps[7];
        timing
    }

    /// A run that went source → state → graph → opportunity → simulation → risk, with a
    /// millisecond stamp per boundary. Round numbers again: these are inputs to a test of
    /// *reading*, and §46 forbids them being mistaken for a figure from a real run.
    const WHOLE_DISCOVERY: [Option<u64>; 8] = [
        Some(1_000),
        Some(1_002),
        Some(1_002),
        Some(1_005),
        Some(1_008),
        Some(1_010),
        Some(1_040),
        Some(1_041),
    ];

    /// §15's claim, checked at the seam: the discovery half is a *re-description* of the
    /// stamps M5 already writes. No stage is invented, each row's own granularity says
    /// millisecond, and nothing below the decision is touched — that is the caller's
    /// `skip_after` or the ladder's to answer for.
    #[test]
    fn the_discovery_half_is_read_off_stamps_this_run_already_wrote() {
        let mut recorder = recorder();
        assert_eq!(
            record_discovery(&mut recorder, &timing(WHOLE_DISCOVERY)),
            Some(Stage::Risk)
        );
        let spans = [
            (Stage::Observation, 2),
            (Stage::StateUpdate, 0),
            (Stage::GraphUpdate, 3),
            (Stage::OpportunityDetection, 3),
            (Stage::Simulation, 30),
            (Stage::Risk, 1),
        ];
        for (stage, milliseconds) in spans {
            let record = record_of(&recorder, stage);
            assert_eq!(record.outcome, StageOutcome::Completed, "{stage}");
            assert_eq!(record.duration_ns, Some(milliseconds * MS), "{stage}");
            assert_eq!(
                record.granularity,
                Some(Granularity::Millisecond),
                "{stage} must not read as a nanosecond measurement"
            );
            let started = record
                .started_ns
                .unwrap_or_else(|| panic!("{stage} has a start stamp"));
            assert_eq!(started % MS, 0, "{stage} is stamped in whole milliseconds");
            assert_eq!(
                started.saturating_add(record.duration_ns.unwrap()),
                record.ended_ns.unwrap(),
                "{stage}'s span is the difference of its two instants, never a third number"
            );
        }
        let trace = recorder.trace().expect("this recorder is on");
        for stage in Stage::ALL {
            if stage > Stage::Risk {
                assert!(
                    trace.stage(stage).is_none(),
                    "{stage} is not this function's to write"
                );
            }
        }
        assert!(
            recorder.finish().refusals.is_empty(),
            "reading existing stamps cannot be refused"
        );
    }

    /// §2.2/§46: one instant cannot be a span. The pair the pipeline stamps together
    /// keeps its zero and carries the reason beside it, while the pairs that are real
    /// spans stay unannotated — a note here is a claim, and a claim must be read.
    #[test]
    fn the_pair_that_is_one_instant_says_so_instead_of_pretending_to_a_span() {
        let mut recorder = recorder();
        record_discovery(&mut recorder, &timing(WHOLE_DISCOVERY));
        let state = record_of(&recorder, Stage::StateUpdate);
        assert_eq!(state.duration_ns, Some(0));
        assert_eq!(state.note.as_deref(), Some(DECODE_AND_APPLY_STAMP));
        assert_eq!(record_of(&recorder, Stage::Observation).note, None);
    }

    /// §9's rule read the other way round. It forbids a skipped stage from reporting 0 ms;
    /// this is the case it cannot reach — a stage that *did* run and *did* answer, inside one
    /// millisecond. The 0 is then a true figure about a clock, and a reader who sums it into
    /// a budget would learn that observing a block costs nothing. So the row names the clock
    /// it hit, on either half of a lifecycle, and backs off where a caveat already explains
    /// the pair in its own words.
    #[test]
    fn a_span_that_lands_inside_one_millisecond_names_the_clock_it_hit() {
        let stuck = [
            Some(1_000),
            Some(1_002),
            Some(1_002),
            Some(1_002),
            Some(1_002),
            Some(1_005),
            Some(1_005),
            Some(1_040),
        ];
        let mut discovery = recorder();
        record_discovery(&mut discovery, &timing(stuck));

        for stage in [
            Stage::GraphUpdate,
            Stage::OpportunityDetection,
            Stage::Simulation,
        ] {
            let record = record_of(&discovery, stage);
            assert_eq!(record.outcome, StageOutcome::Completed, "{stage}");
            assert_eq!(record.duration_ns, Some(0), "{stage}");
            assert_eq!(record.note.as_deref(), Some(SAME_MILLISECOND), "{stage}");
        }
        // A pair that is a real span gains nothing from this rule: a note is a claim.
        assert_eq!(record_of(&discovery, Stage::Risk).note, None);
        // And a pair with its own explanation keeps it instead of doubling up.
        assert_eq!(
            record_of(&discovery, Stage::StateUpdate).note.as_deref(),
            Some(DECODE_AND_APPLY_STAMP)
        );

        // The execution ladder obeys the same reading, which is how M7's own
        // `settled` → `profit_verified` pair arrives: the account was closed inside the
        // millisecond the receipt landed.
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
            (ExecutionStatus::Included, 6_000),
            (ExecutionStatus::Settled, 7_000),
            (ExecutionStatus::ProfitVerified, 7_000),
        ]);
        let mut ladder_run = recorder();
        record_ladder(
            &mut ladder_run,
            Some(&record),
            ExecutionMode::Submit,
            "the route is accounted for",
        );
        let settled = record_of(&ladder_run, Stage::ProfitVerification);
        assert_eq!(settled.outcome, StageOutcome::Completed);
        assert_eq!(settled.duration_ns, Some(0));
        assert_eq!(settled.note.as_deref(), Some(SAME_MILLISECOND));
        assert_eq!(record_of(&ladder_run, Stage::Settlement).note, None);
    }

    /// §11 on the one stage it can be wrong about. A simulation whose state came from a
    /// dump is this process working; a simulation whose state came from `rpc:chain-91342`
    /// spent most of its span waiting on a node, and filing that under local processing
    /// would blame this binary for a node's latency — or, worse, credit it with time it
    /// never computed. The class comes from the run's own `state_source` string, and the
    /// duration is left exactly as it was measured.
    #[test]
    fn a_simulation_span_that_read_from_a_node_is_not_counted_as_local_processing() {
        let mut on_node = recorder();
        on_node.begin_at(Stage::Simulation, 1_000);
        on_node.end_at(Stage::Simulation, 24_000_000);
        on_node.classify_reads(Stage::Simulation, "rpc:chain-91342");
        let span = record_of(&on_node, Stage::Simulation);
        assert_eq!(span.domain(), Domain::Mixed);
        assert_eq!(span.duration_ns, Some(23_999_000), "the span is unchanged");
        let note = span.note.clone().expect("the row names its class");
        assert!(
            note.contains("rpc:chain-91342"),
            "the sentence has to name the source it read: {note}"
        );

        let dump = {
            let mut on_dump = recorder();
            on_dump.begin_at(Stage::Simulation, 1_000);
            on_dump.end_at(Stage::Simulation, 21_000);
            on_dump.classify_reads(
                Stage::Simulation,
                "data/evidence/m5/replay-91342/dump-37191169.json@37191169",
            );
            record_of(&on_dump, Stage::Simulation)
        };
        assert_eq!(
            dump.domain(),
            Domain::Local,
            "a run that read a file is this process working"
        );
        assert_eq!(dump.note, None, "and nothing is added to its row");
    }

    /// The same call on a stage that never ran: there is no span to class, and writing a
    /// refusal into `instrumentation_refusals` for a lifecycle that stopped early would
    /// turn the run's own outcome into an apparent fault of this module (§35).
    #[test]
    fn classing_a_stage_with_no_span_says_nothing_and_refuses_nothing() {
        let mut stopped = recorder();
        stopped.skip(
            Stage::Simulation,
            "the finding went stale before its run started",
        );
        stopped.classify_reads(Stage::Simulation, "rpc:chain-91342");
        let row = record_of(&stopped, Stage::Simulation);
        assert_eq!(row.outcome, StageOutcome::Skipped);
        assert_eq!(row.duration_ns, None);
        assert_eq!(
            row.note.as_deref(),
            Some("the finding went stale before its run started"),
            "the skip's own reason stands"
        );
        assert!(
            stopped.finish().refusals.is_empty(),
            "a stage that was never measured is not a refused write"
        );
    }

    /// The answer of [`record_discovery`] is where this lifecycle's story ends, so a run
    /// that never got a finding out of its graph is timed as far as it went and no
    /// further: §34's T2 shape one level up from the route path.
    #[test]
    fn a_run_that_stopped_above_the_finding_is_timed_only_as_far_as_it_went() {
        let half = [
            Some(1_000),
            Some(1_002),
            Some(1_002),
            None,
            None,
            None,
            None,
            None,
        ];
        let mut partial = recorder();
        assert_eq!(
            record_discovery(&mut partial, &timing(half)),
            Some(Stage::StateUpdate)
        );

        let mut recorder = recorder();
        let detail = "this run's graph had no position to read a route from";
        record_lifecycle(
            &mut recorder,
            &timing(half),
            None,
            ExecutionMode::Submit,
            detail,
        );
        assert_eq!(
            record_of(&recorder, Stage::StateUpdate).outcome,
            StageOutcome::Completed
        );
        for stage in Stage::ALL {
            if stage > Stage::StateUpdate {
                let record = record_of(&recorder, stage);
                assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
                assert_eq!(record.duration_ns, None, "{stage} never ran");
                assert!(
                    record.note.as_deref().unwrap_or_default().contains(detail),
                    "{stage} must say why in this run's own words: {:?}",
                    record.note
                );
            }
        }
    }

    /// A timing with only its first stamp holds no span at all. Nothing is measured, and
    /// the trace says so for all fourteen stages rather than opening a stage it cannot
    /// close — §9's `Skipped` with a reason, never a `Started` left hanging.
    #[test]
    fn a_block_that_never_reached_its_own_decoding_leaves_no_span_to_read() {
        let received_only = [Some(1_000), None, None, None, None, None, None, None];
        let mut without_spans = recorder();
        assert_eq!(
            record_discovery(&mut without_spans, &timing(received_only)),
            None
        );

        let mut recorder = recorder();
        let detail = "the block never made it through this run's decoding";
        record_lifecycle(
            &mut recorder,
            &timing(received_only),
            None,
            ExecutionMode::Submit,
            detail,
        );
        for stage in Stage::ALL {
            let record = record_of(&recorder, stage);
            assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
            assert_eq!(record.duration_ns, None, "{stage}");
        }
        let recorded = recorder.finish();
        assert!(recorded.refusals.is_empty(), "{:?}", recorded.refusals);
    }

    /// §34's T1 at the seam that joins the two halves: a lifecycle that ran the whole way
    /// gets twelve measured spans, and the two stages without a number are the two this
    /// build cannot have — §20's gate on the route path only, and a receipt the M6 ladder
    /// never stamps separately.
    #[test]
    fn a_lifecycle_that_ran_the_whole_way_answers_for_the_two_stages_it_cannot_have() {
        let record = ladder(&[
            (ExecutionStatus::Preflighted, 2_000),
            (ExecutionStatus::Built, 3_000),
            (ExecutionStatus::Signed, 4_000),
            (ExecutionStatus::Submitted, 5_000),
            (ExecutionStatus::Included, 6_000),
            (ExecutionStatus::Settled, 7_000),
            (ExecutionStatus::ProfitVerified, 8_000),
        ]);
        let mut recorder = recorder();
        record_lifecycle(
            &mut recorder,
            &timing(WHOLE_DISCOVERY),
            Some(&record),
            ExecutionMode::Submit,
            "the lane took this finding",
        );
        for stage in Stage::ALL {
            assert!(
                recorder
                    .trace()
                    .and_then(|trace| trace.stage(stage))
                    .is_some(),
                "{stage} must have a row"
            );
        }
        let completed = Stage::ALL
            .into_iter()
            .filter(|stage| record_of(&recorder, *stage).outcome == StageOutcome::Completed)
            .count();
        assert_eq!(
            completed, 12,
            "fourteen stages minus the gate and the receipt"
        );
        assert_eq!(
            record_of(&recorder, Stage::Preflight).note.as_deref(),
            Some(NO_GATE_ON_LIVE)
        );
        assert_eq!(
            record_of(&recorder, Stage::Receipt).note.as_deref(),
            Some(RECEIPT_NOT_SEPARABLE)
        );
        let end_to_end = recorder.trace().expect("on").end_to_end();
        // §12's detection latency is a milestone, not a stage span: from the first
        // instant of observation to the instant the finding was accepted.
        assert_eq!(end_to_end.detection_ns, Some(8 * MS));
        assert_eq!(end_to_end.inclusion_ns, Some(2_000 * MS));
        assert!(
            end_to_end.end_to_end_ns.is_some(),
            "a run that reached profit verification has a whole lifecycle to report"
        );
    }

    /// §14 inside one trace: the finding's own latencies stay measured whether or not a
    /// transaction was ever built from them, and every stage under the decision answers
    /// for bytes this opportunity did not have. §13's execution figures read as absent,
    /// not as zero.
    #[test]
    fn a_declined_finding_keeps_its_latencies_and_has_no_transaction_half() {
        let mut recorder = recorder();
        let detail = "the risk layer declined this finding (impact over the cap)";
        record_lifecycle(
            &mut recorder,
            &timing(WHOLE_DISCOVERY),
            None,
            ExecutionMode::Submit,
            detail,
        );
        for stage in [
            Stage::Observation,
            Stage::StateUpdate,
            Stage::GraphUpdate,
            Stage::OpportunityDetection,
            Stage::Simulation,
            Stage::Risk,
        ] {
            assert_eq!(
                record_of(&recorder, stage).outcome,
                StageOutcome::Completed,
                "{stage}"
            );
        }
        assert_eq!(
            record_of(&recorder, Stage::Preflight).note.as_deref(),
            Some(NO_GATE_ON_LIVE),
            "the absence of the gate is a fact about this build, not about this finding"
        );
        for stage in Stage::ALL {
            if stage > Stage::Preflight {
                let record = record_of(&recorder, stage);
                assert_eq!(record.outcome, StageOutcome::Skipped, "{stage}");
                assert_eq!(record.duration_ns, None, "{stage} must not average as 0");
            }
        }
        let trace = recorder.trace().expect("on");
        let end_to_end = trace.end_to_end();
        assert_eq!(end_to_end.detection_ns, Some(8 * MS));
        assert_eq!(end_to_end.execution_preparation_ns, None);
        assert_eq!(end_to_end.inclusion_ns, None);
        assert_eq!(end_to_end.settlement_ns, None);
        assert_eq!(end_to_end.end_to_end_ns, None);
        assert_eq!(trace.stage_duration_sum_ns(), Some(39 * MS));
        let missing_hops = trace
            .hops()
            .into_iter()
            .filter(|(_, duration)| duration.is_none())
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        for hop in [
            "opportunity_to_submit",
            "submit_to_inclusion",
            "settlement_to_profit_verified",
        ] {
            assert!(missing_hops.contains(&hop), "{hop} must read as absent");
        }
    }
}
