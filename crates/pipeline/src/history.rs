//! M8.1 §30: the baseline that is already on disk.
//!
//! M7 ran six real transactions through the whole lifecycle and wrote down, for its own
//! reasons, the instant each rung of the execution ladder answered at and the spans of
//! detection, simulation and §26's gate — `latency_ms`, in the `route-run.json` each route
//! run leaves behind. §30 asks M8.1 to use that history rather than re-run it, and
//! forbids the tempting shortcut: where the files hold no instant, the answer is N/A,
//! never a `reconstructed_time` (§46's interpolation ban applied to M7's own runs).
//!
//! So this module reads one file per run and shapes what is in it:
//!
//! - **A duration with no instants is recorded as one.** M7's stamps count milliseconds
//!   from *its* process's monotonic clock, and that clock's zero was never written to
//!   disk. The difference of two of them is therefore a real measured span, while either
//!   one alone is a number no other run can compare against.
//!   [`evm_metrics::StageRecord::duration_only`] is the shape that says exactly that:
//!   a span, and no claim about when.
//! - **A stage with no figure is skipped**, with the field that would have carried it
//!   named in the note. The three discovery stages go this way: a route run is *handed*
//!   its candidate, so the observation that produced it is not this evidence's to report.
//! - **Every note names the file and key it read**, so a reader of `traces.jsonl` can
//!   open the M7 file and check the number. That is §44's reproducibility for a baseline
//!   built from someone else's run, and it is why nothing here averages, rounds,
//!   rescales, or subtracts a figure it did not find in the file.
//!
//! The traces land under [`evm_metrics::TraceSource::Replay`]: the latencies describe real
//! network and chain time, but *this* process did not measure them, and what §45 regulates
//! is which samples get averaged together. A live observation run's traces go in a
//! different directory and a different table.

use std::path::{Path, PathBuf};

use evm_metrics::{Clock, LatencyTrace, Stage, TraceSource};
use serde_json::Value;

use crate::error::{PipelineError, Result};
use crate::latency::{
    git_revision, LatencyEvidence, RecordedTrace, TraceRecorder, RECEIPT_NOT_SEPARABLE, TRACES_FILE,
};

/// The one file M7 wrote that holds a route run's timings (§30's source). Nothing else
/// in the run's directory is read here.
pub const RUN_FILE: &str = "route-run.json";

/// §30's N/A, for the three discovery stages this file holds no figure for.
pub const NO_DISCOVERY_IN_M7: &str = "M7's route run is handed its pair on the command line: \
     this evidence holds no observation, state-update or graph-update figure, so §30's N/A \
     applies and none of these three rows carries a number; the head read the run did perform \
     is inside its detection figure, which says so beside itself";

/// §30's N/A for the one discovery stage M7 reached but did not time.
pub const NO_RISK_SPAN_IN_M7: &str = "M7's route run recorded the risk decision's content and \
     not its span: no figure for the time between the simulation answering and the decision \
     exists in this file, so nothing is inferred from the stages around it";

/// §42.3's caveat on the one M7 figure whose name covers more than §12's stage.
///
/// M7 starts this clock *before* it asks the node for the head — `run_once` in
/// [`crate::arbitrage`] takes `detected_at`, then reads the head, the header, and
/// `price_legs` — so the number its file holds is §13's observation→opportunity
/// milestone, and the live trace of the same code path puts that same value in
/// `hops_ns.observation_to_opportunity` while its `opportunity_detection` row holds only
/// the span after the header was read. A history has no instants, so the milestone can
/// travel nowhere but this row, and the row says what it is.
pub const M7_DETECTION_COVERS_OBSERVATION: &str = "M7's `opportunity_detection_latency_ms` \
     starts before its own head read and stops once the route is priced, so this is §13's \
     observation→opportunity milestone rather than §12's detection-stage span, and the \
     detection stage alone is smaller than this figure by the observation inside it";

/// §46's honest zero, generically: two stamps landing on one millisecond is a fact about
/// the clock's resolution, not a measurement that the stage between them cost nothing.
/// Phrased this way rather than per-pair because a zero can appear on any rung (M7's
/// settlement and its profit verdict climb back to back, and a fast build can stamp twice
/// in the same millisecond) and the note must not claim a pairing the run did not make.
pub const SAME_INSTANT: &str = "both stamps are the same millisecond, so this span is the \
     resolution of M7's clock rather than a measurement that this stage costs no time";

/// §46's caveat where one number could otherwise be read as covering more than it does.
///
/// M7's ledger holds **one record for the whole six-step sequence**, and its ladder is
/// climbed once — so `submitted` and `included` are the first bound transaction's, while
/// the other five steps leave one instant each in `submissions.jsonl` with no pair to
/// subtract. Saying so on the row is what keeps a baseline of "inclusion latency" from
/// silently meaning "the first of six".
pub fn sequence_caveat(steps: u64) -> String {
    format!(
        "this record is the whole sequence and its ladder is climbed once: this span covers \
         the first bound transaction, not all {steps} steps"
    )
}

/// One M7 route run's recorded timings, as far as its own file states them.
pub struct M7Run {
    pub dir: PathBuf,
    pub session_id: String,
    pub chain_id: u64,
    pub mode: String,
    pub pinned_block: Option<u64>,
    /// The opportunity's stable id, as M7 wrote it — §14 asks a trace to reuse an
    /// identity that already exists rather than invent a second one to disagree with.
    pub opportunity_id: Option<String>,
    /// `REAL_MARKET` or `CONTROLLED_FIXTURE`: what market these latencies were paid for.
    pub market: Option<String>,
    steps_planned: u64,
    /// Where the run's ladder stopped, in its own record's words.
    status: Option<String>,
    /// What M7's run answered from when the EVM asked for state — `simulation.state_source`
    /// in this same file. §11 reads it to decide whether the simulation span it is timing
    /// contains a node: `rpc:chain-91342` says it does. Absent means the row is left with
    /// the class its stage name gives it, which is the honest default and not a claim.
    state_source: Option<String>,
    /// The `latency_ms` object, kept raw: every figure below names the key it was read
    /// out of, so a note in the trace file is checkable against the evidence.
    latency: Value,
}

impl M7Run {
    /// Read one run's directory. A missing or unparsable file is a failure of this read,
    /// not a zero: §30's baseline is only as honest as the file it came from, and a trace
    /// with no chain id or no session id would be a §44 metadata field invented.
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(RUN_FILE);
        let text = std::fs::read_to_string(&path).map_err(|error| PipelineError::Evidence {
            path: path.clone(),
            detail: format!("M7's run record could not be read: {error}"),
        })?;
        let record: Value =
            serde_json::from_str(&text).map_err(|error| PipelineError::Evidence {
                path: path.clone(),
                detail: format!("M7's run record is not JSON: {error}"),
            })?;
        let missing = |field: &str| PipelineError::Evidence {
            path: path.clone(),
            detail: format!(
                "M7's run record has no `{field}`: a history trace cannot be opened without \
                 the §44 metadata the file is expected to carry"
            ),
        };
        let string = |field: &str| {
            record
                .get(field)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| missing(field))
        };
        let execution = record
            .get("execution")
            .filter(|value| !value.is_null())
            .unwrap_or(&Value::Null);
        Ok(Self {
            dir: dir.to_path_buf(),
            session_id: string("session_id")?,
            chain_id: record
                .get("chain_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| missing("chain_id"))?,
            mode: string("mode")?,
            pinned_block: record.get("pinned_block").and_then(Value::as_u64),
            opportunity_id: execution
                .get("opportunity_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            market: record
                .get("market")
                .and_then(|market| market.get("market_kind"))
                .and_then(Value::as_str)
                .map(str::to_string),
            steps_planned: execution
                .get("steps_planned")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            status: execution
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
            state_source: record
                .get("simulation")
                .and_then(|simulation| simulation.get("state_source"))
                .and_then(Value::as_str)
                .filter(|source| !source.is_empty())
                .map(str::to_string),
            latency: record
                .get("latency_ms")
                .cloned()
                .ok_or_else(|| missing("latency_ms"))?,
        })
    }

    /// The file and key a note names, written so the path in `traces.jsonl` opens exactly
    /// where the number came from.
    fn source(&self, key: &str) -> String {
        format!("{}#latency_ms.{key}", self.dir.join(RUN_FILE).display())
    }

    /// A span M7 computed and wrote, with no instants behind it in this file.
    fn figure(&self, key: &str) -> Option<u64> {
        self.latency.get(key).and_then(Value::as_u64)
    }

    /// One of the ladder's stamps, in M7's own run-relative milliseconds.
    fn stamp(&self, rung: &str) -> Option<u64> {
        self.latency.get("rung_stamps")?.get(rung)?.as_u64()
    }

    /// The difference of two recorded stamps — the only arithmetic this module does, and
    /// the same pairing M7's own metrics perform (`meter` in
    /// `crates/execution/src/lifecycle.rs`).
    ///
    /// A pair out of order reads as no figure at all. §34's T8 says a negative duration
    /// may not exist, and clamping one to zero would be an estimate wearing a
    /// measurement's name (§46).
    fn span(&self, from: &str, to: &str) -> Option<u64> {
        let (first, second) = (self.stamp(from)?, self.stamp(to)?);
        (second >= first).then_some(second - first)
    }

    /// Why this span is not in the file: the run's own stopping point as its record
    /// states it, rather than a guess about which step failed. The execution mode rides
    /// along because it is in the same file and it is what stopped a `build-only` run —
    /// §9's difference between a stage that did not answer and one that was never
    /// attempted cannot be drawn without it.
    fn stopped(&self, from: &str, to: &str) -> String {
        let stopped = self.status.as_deref().unwrap_or("unrecorded");
        format!(
            "M7's ladder holds no `{from}`→`{to}` pair at {}: the run stopped with status \
             `{stopped}` under execution mode `{}`",
            self.source("rung_stamps"),
            self.mode,
        )
    }

    /// One stage's row from a figure M7 wrote, or §30's N/A for the key that is absent.
    fn timed(&self, recorder: &mut TraceRecorder, stage: Stage, key: &str) {
        self.timed_noting(recorder, stage, key, None);
    }

    /// The same row with one more thing a reader needs in order to believe it — used
    /// where M7's field name is not the same measurement as §12's stage name.
    fn timed_noting(
        &self,
        recorder: &mut TraceRecorder,
        stage: Stage,
        key: &str,
        beyond: Option<&str>,
    ) {
        match self.figure(key) {
            Some(ms) => {
                let base = format!("read from {}", self.source(key));
                let base = match beyond {
                    Some(extra) => format!("{base}; {extra}"),
                    None => base,
                };
                recorder.duration_ms(stage, Some(ms), &self.caveat(stage, &base));
            }
            None => recorder.skip(stage, &format!("no figure at {}", self.source(key))),
        }
    }

    /// One stage's row from the difference of two stamps, or the reason there is none.
    fn spanned(&self, recorder: &mut TraceRecorder, stage: Stage, from: &str, to: &str) {
        match self.span(from, to) {
            Some(ms) => {
                let base = format!(
                    "the difference of the `{from}` and `{to}` stamps at {}",
                    self.source("rung_stamps")
                );
                let note = if ms == 0 {
                    format!("{base}; {SAME_INSTANT}")
                } else {
                    self.caveat(stage, &base)
                };
                recorder.duration_ms(stage, Some(ms), &note);
            }
            None => recorder.skip(stage, &self.stopped(from, to)),
        }
    }

    /// The one place a span needs more than its own two stamps to be read honestly.
    fn caveat(&self, stage: Stage, note: &str) -> String {
        if self.steps_planned > 1 && matches!(stage, Stage::Submit | Stage::Inclusion) {
            format!("{note}; {}", sequence_caveat(self.steps_planned))
        } else {
            note.to_string()
        }
    }

    /// The trace this run's evidence supports: `Replay`, because these are recorded
    /// figures rather than a lifecycle this process walked, and keyed on the opportunity
    /// M7 named.
    pub fn trace(&self) -> LatencyTrace {
        LatencyTrace::new(
            TraceSource::Replay,
            self.chain_id,
            self.pinned_block,
            self.opportunity_id.as_deref(),
        )
    }

    /// All fourteen stages, each either a span the file states or the absence it states.
    ///
    /// The pairing is M7's own: build is `created`→`built` because the ledger stamps no
    /// rung for §26's gate, which is why `preflight_latency_ms` stands here as a figure of
    /// its own *and* sits inside the build span. That is a fact about M7's evidence and it
    /// travels in the trace rather than being fixed in this build — §15: a latency point
    /// that would need a new stamp is a point this milestone does not take. It is written
    /// down as an optimization candidate instead (§42.9).
    pub fn record(&self, recorder: &mut TraceRecorder) {
        for stage in [Stage::Observation, Stage::StateUpdate, Stage::GraphUpdate] {
            recorder.skip(stage, NO_DISCOVERY_IN_M7);
        }
        self.timed_noting(
            recorder,
            Stage::OpportunityDetection,
            "opportunity_detection_latency_ms",
            Some(M7_DETECTION_COVERS_OBSERVATION),
        );
        self.timed(recorder, Stage::Simulation, "simulation_latency_ms");
        recorder.skip(Stage::Risk, NO_RISK_SPAN_IN_M7);
        self.timed(recorder, Stage::Preflight, "preflight_latency_ms");
        self.spanned(recorder, Stage::Build, "created", "built");
        self.spanned(recorder, Stage::Sign, "built", "signed");
        self.spanned(recorder, Stage::Submit, "signed", "submitted");
        self.spanned(recorder, Stage::Inclusion, "submitted", "included");
        recorder.skip(Stage::Receipt, RECEIPT_NOT_SEPARABLE);
        self.spanned(recorder, Stage::Settlement, "included", "settled");
        self.spanned(
            recorder,
            Stage::ProfitVerification,
            "settled",
            "profit_verified",
        );
    }

    /// This run as one line of a history baseline.
    ///
    /// The recorder is `on` a clock it never reads: every row here comes from a figure or
    /// a difference, and [`TraceRecorder::finish`]'s closing stamp is suppressed for a
    /// trace that has no first instant of its own — otherwise the loading process's clock
    /// would appear in the file as the end of a lifecycle it did not run (§46).
    pub fn recorded(&self) -> RecordedTrace {
        let mut recorder = TraceRecorder::on(Clock::new(), self.trace());
        self.record(&mut recorder);
        // §11: M7's simulation ran on the node's state, and its own record says so, so
        // the 21 s span this loader timed is reads-and-compute rather than CPU time. A run
        // whose record names no state source keeps the nominal class: there is nothing
        // here to override it with.
        if let Some(source) = self.state_source.as_deref() {
            recorder.classify_reads(Stage::Simulation, source);
        }
        recorder.finish().with_session(self.session_id.clone())
    }
}

/// The §30 baseline over a set of M7 run directories.
///
/// One rule binds here that binds nowhere else in M8.1: every run in one baseline must
/// have been executed under the **same mode**. §45 forbids blending a replay's latencies
/// with a live network's, and the same reasoning says a run that stopped at the gate and a
/// run that paid for six real inclusions are not one sample population either —
/// `execution_mode` is one of §44's metadata fields and a directory whose traces disagree
/// about it cannot fill it in. Refusing costs one command line; blending would cost the
/// baseline its meaning.
pub fn baseline(dirs: &[PathBuf], output: &Path) -> Result<BaselineRun> {
    if dirs.is_empty() {
        return Err(PipelineError::Config(
            "a history baseline needs at least one M7 run directory".to_string(),
        ));
    }
    let traces = output.join(TRACES_FILE);
    if traces.exists() {
        return Err(PipelineError::Evidence {
            path: traces,
            detail: "this directory already holds a trace file: §44 forbids appending a second \
                     history onto one already on disk, so name a new output directory for this \
                     baseline"
                .to_string(),
        });
    }
    let runs = dirs
        .iter()
        .map(|dir| M7Run::read(dir))
        .collect::<Result<Vec<M7Run>>>()?;
    let mode = runs[0].mode.clone();
    if let Some(other) = runs.iter().find(|run| run.mode != mode) {
        return Err(PipelineError::Config(format!(
            "these runs are not one sample population: {} was executed under `{}` and {} under \
             `{}`. §45's separation applies to execution modes as it does to sources, so build \
             one baseline per mode",
            runs[0].session_id, mode, other.session_id, other.mode
        )));
    }

    let mut evidence = LatencyEvidence::open(output, git_revision(), &mode)?;
    for run in &runs {
        evidence.record(run.recorded())?;
    }
    let dir = evidence.finish()?;
    Ok(BaselineRun {
        dir,
        samples: runs.len(),
        mode,
        sessions: runs.iter().map(|run| run.session_id.clone()).collect(),
    })
}

/// What the loader wrote, so the caller can say it out loud and a test can check it.
#[derive(Clone, Debug)]
pub struct BaselineRun {
    pub dir: PathBuf,
    pub samples: usize,
    pub mode: String,
    pub sessions: Vec<String>,
}
