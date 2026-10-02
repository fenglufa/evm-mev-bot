//! M8.1 §3–§14: one trace per opportunity lifecycle.
//!
//! A trace is a *bypass* record (§2.1). It holds timestamps that some other part
//! of the run already produced, and it decides nothing: no stage in this file can
//! change whether an opportunity is simulated, gated, built, signed or sent, and
//! nothing here reads a node, a socket or a clock other than the monotonic one
//! this run is already using (§15's rule — a latency point that would need a new
//! RPC is not allowed to exist).
//!
//! Two halves, deliberately kept apart rather than merged into one
//! `execution_latency` (§4, §14):
//!
//! ```text
//! discovery    Observation → StateUpdate → GraphUpdate → OpportunityDetection
//! execution    Simulation → Risk → Preflight → Build → Sign → Submit
//!              → Inclusion → Receipt → Settlement → ProfitVerification
//! ```
//!
//! The units are nanoseconds on [`Clock`](crate::Clock)'s single monotonic origin
//! (§8, §23). `SystemTime` never appears in this module: a duration computed from
//! wall time can be moved by an NTP correction mid-run, which would make the
//! baseline a fact about the network time service rather than about this binary.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use serde_json::json;
use thiserror::Error;

/// The version stamped on every trace this build writes, so a later schema
/// change is visible in the file it did not touch (§44).
pub const TRACE_SCHEMA: u32 = 1;

/// The one hop list §12 asks for, in the order the report prints it.
pub const HOP_NAMES: &[&str] = &[
    "observation_to_opportunity",
    "opportunity_to_simulation_start",
    "simulation_duration",
    "simulation_to_risk",
    "risk_duration",
    "risk_to_preflight",
    "preflight_duration",
    "build_duration",
    "sign_duration",
    "submit_duration",
    "submit_to_inclusion",
    "inclusion_to_receipt",
    "receipt_to_settlement",
    "settlement_to_profit_verified",
    "opportunity_to_submit",
    "opportunity_to_inclusion",
    "opportunity_to_profit_verified",
];

/// The §4 stage model: every point in the lifecycle that has a defensible
/// "started" and "ended", and no others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    // Discovery half.
    Observation,
    StateUpdate,
    GraphUpdate,
    OpportunityDetection,
    // Execution half (§14's ExecutionTrace, and the half a run that never sends
    // spends entirely in `Skipped`).
    Simulation,
    Risk,
    Preflight,
    Build,
    Sign,
    Submit,
    Inclusion,
    Receipt,
    Settlement,
    ProfitVerification,
}

impl Stage {
    /// Every stage, in lifecycle order. The evidence file writes this order, so a
    /// reader comparing two runs' traces is comparing the same fourteen rows.
    pub const ALL: [Stage; 14] = [
        Stage::Observation,
        Stage::StateUpdate,
        Stage::GraphUpdate,
        Stage::OpportunityDetection,
        Stage::Simulation,
        Stage::Risk,
        Stage::Preflight,
        Stage::Build,
        Stage::Sign,
        Stage::Submit,
        Stage::Inclusion,
        Stage::Receipt,
        Stage::Settlement,
        Stage::ProfitVerification,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::StateUpdate => "state_update",
            Self::GraphUpdate => "graph_update",
            Self::OpportunityDetection => "opportunity_detection",
            Self::Simulation => "simulation",
            Self::Risk => "risk",
            Self::Preflight => "preflight",
            Self::Build => "build",
            Self::Sign => "sign",
            Self::Submit => "submit",
            Self::Inclusion => "inclusion",
            Self::Receipt => "receipt",
            Self::Settlement => "settlement",
            Self::ProfitVerification => "profit_verification",
        }
    }

    /// §14: which half of the lifecycle a stage belongs to. A trace for a finding
    /// that was never executed has a discovery half with data and an execution
    /// half with skips, and the two must stay tellable apart.
    pub const fn half(self) -> Half {
        match self {
            Self::Observation
            | Self::StateUpdate
            | Self::GraphUpdate
            | Self::OpportunityDetection => Half::Discovery,
            Self::Simulation
            | Self::Risk
            | Self::Preflight
            | Self::Build
            | Self::Sign
            | Self::Submit
            | Self::Inclusion
            | Self::Receipt
            | Self::Settlement
            | Self::ProfitVerification => Half::Execution,
        }
    }

    /// §11's *nominal* class: where a stage's time belongs judging by what the stage
    /// is. Summing across a mixed category is how a run gets blamed for a node it does
    /// not control. A span whose contents disagree with its name carries the override
    /// on its record instead — see [`StageRecord::cost_domain`].
    pub const fn domain(self) -> Domain {
        match self {
            Self::StateUpdate
            | Self::GraphUpdate
            | Self::OpportunityDetection
            // Nominal only: on the route path this stage's state arrives from a node
            // while the span is open, and that run says so on the record.
            | Self::Simulation
            | Self::Risk
            | Self::Build
            | Self::Sign => Domain::Local,
            Self::Observation
            | Self::Submit
            | Self::Inclusion
            | Self::Receipt
            | Self::Settlement
            | Self::ProfitVerification => Domain::Network,
            // M7's preflight reads balances and a gas price from the node, then
            // decides locally. Its span contains both, so it is counted in
            // neither of the two sums below and is listed on its own.
            Self::Preflight => Domain::Mixed,
        }
    }
}

impl Display for Stage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// §14's two halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    Discovery,
    Execution,
}

impl Half {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Execution => "execution",
        }
    }
}

/// §11's three cost classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// This binary computing.
    Local,
    /// A node, an RPC round trip, or the chain's own block time.
    Network,
    /// Reads from the node and decides locally, so its span belongs to neither.
    Mixed,
}

impl Domain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Network => "network",
            Self::Mixed => "mixed",
        }
    }
}

/// §9: how a stage ended.
///
/// `Skipped` is a state a trace can report, and a skipped stage has **no**
/// duration rather than a zero one — §9's point is that "the run never got there"
/// and "it got there in no time" are different facts, and collapsing them would
/// make a run that sent nothing look like the fastest run on record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageOutcome {
    Started,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl StageOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether this outcome ends the stage. `Started` is the one that does not,
    /// and §34's T9 turns on rejecting a second close after it already has.
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Started)
    }
}

impl Display for StageOutcome {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How finely a stamp in one record was actually taken.
///
/// This exists because of §15 and §30 at once. Measuring a stage at nanosecond
/// resolution would mean putting a timestamp *inside* the thing being measured —
/// an extra read, in some cases an extra RPC — so a stage whose instants already
/// exist as milliseconds on the run's clock is recorded as milliseconds, honestly
/// labelled, rather than padded with three invented zeros. A reader can then see
/// which end of the baseline is a measurement and which is a reading of a stamp
/// M7 wrote for its own reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Nanosecond,
    Millisecond,
}

impl Granularity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nanosecond => "nanosecond",
            Self::Millisecond => "millisecond",
        }
    }

    const fn factor(self) -> u64 {
        match self {
            Self::Nanosecond => 1,
            Self::Millisecond => 1_000_000,
        }
    }
}

impl Display for Granularity {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of run produced a trace (§44's `source`).
///
/// The three are never aggregated together: §45 forbids blending a replay's
/// latency (a directory read from disk) with a live run's (a node answering over
/// the network), and a fixture must never be presented as market latency (§33).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TraceSource {
    Live,
    Replay,
    Fixture,
}

impl TraceSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Replay => "replay",
            Self::Fixture => "fixture",
        }
    }
}

impl Display for TraceSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a write to a trace was refused.
///
/// §35 allows no `unwrap`, `expect` or `panic!` in this code, so every rule about
/// double-closing a stage has to be a value the caller can see and count instead
/// of a crash. A refusal never damages the record that is already there.
#[derive(Clone, Debug, Error)]
pub enum TraceError {
    #[error("stage {stage} is already {outcome}; a second close would overwrite a measurement")]
    AlreadyClosed { stage: Stage, outcome: StageOutcome },
    #[error("stage {stage} was never started, so it cannot be closed")]
    NotStarted { stage: Stage },
    #[error("stage {stage} already has a record; this trace does not silently replace one")]
    AlreadyRecorded { stage: Stage },
    #[error("stage {stage} has no measured span, so there is nothing to class")]
    Unmeasured { stage: Stage },
    #[error("the trace is closed; a stage cannot be written after the lifecycle ended")]
    TraceClosed,
}

/// One stage's measurement.
///
/// `started_ns` and `ended_ns` are readings of the run's monotonic clock, so they
/// mean "milliseconds-nanoseconds since this process began", not "when the world
/// was" (§2.2). They are `Option`s because a historical record read out of M7's
/// evidence has a duration and no instants: inventing the pair to fill the shape
/// would be §30's forbidden `reconstructed_time`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageRecord {
    pub stage: Stage,
    pub outcome: StageOutcome,
    pub started_ns: Option<u64>,
    pub ended_ns: Option<u64>,
    /// `None` for a skipped or cancelled stage, and for one that is still open.
    pub duration_ns: Option<u64>,
    pub granularity: Option<Granularity>,
    pub note: Option<String>,
    /// §11's class as this span actually earned it, when that disagrees with the
    /// class the stage's name suggests. `None` means the nominal [`Stage::domain`]
    /// stands.
    ///
    /// One stage needs this. `Simulation` is a local computation, and on a
    /// dump-backed run every microsecond of its span is this process working; on the
    /// route path its state arrives from a node *inside* the same span
    /// (`crates/simulation/src/state.rs` reads accounts, code and storage slots
    /// through the chain adapter), so labelling that span local would file node
    /// round trips under CPU. The run knows which of the two it used — the provider
    /// names its own source — so the class is handed over rather than guessed from
    /// the stage name, the same way §9's Skipped-versus-Failed is read off
    /// `ExecutionMode`'s predicates instead of inferred.
    pub cost_domain: Option<Domain>,
}

impl StageRecord {
    /// The cost class to sum this span into: the one the run stated, or the stage's
    /// nominal class when the run had no reason to disagree.
    pub fn domain(&self) -> Domain {
        self.cost_domain.unwrap_or_else(|| self.stage.domain())
    }

    /// A span measured directly on the run's clock.
    pub fn measured(stage: Stage, started_ns: u64, ended_ns: u64) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Completed,
            started_ns: Some(started_ns),
            ended_ns: Some(ended_ns),
            // §8: never negative. Two readings of one monotonic clock cannot
            // invert, but the floor is written down rather than assumed.
            duration_ns: Some(ended_ns.saturating_sub(started_ns)),
            granularity: Some(Granularity::Nanosecond),
            note: None,
            cost_domain: None,
        }
    }

    /// A span read off stamps that already exist in milliseconds.
    pub fn measured_ms(stage: Stage, started_ms: u64, ended_ms: u64) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Completed,
            started_ns: Some(started_ms.saturating_mul(Granularity::Millisecond.factor())),
            ended_ns: Some(ended_ms.saturating_mul(Granularity::Millisecond.factor())),
            duration_ns: Some(
                ended_ms
                    .saturating_sub(started_ms)
                    .saturating_mul(Granularity::Millisecond.factor()),
            ),
            granularity: Some(Granularity::Millisecond),
            note: None,
            cost_domain: None,
        }
    }

    /// A duration with no instants behind it: what M7's evidence holds for the
    /// discovery half, where a latency was computed and the readings that produced
    /// it were not written down (§30: no guessing).
    pub fn duration_only(
        stage: Stage,
        duration_ms: u64,
        granularity: Granularity,
        note: &str,
    ) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Completed,
            started_ns: None,
            ended_ns: None,
            duration_ns: Some(duration_ms.saturating_mul(granularity.factor())),
            granularity: Some(granularity),
            note: Some(note.to_string()),
            cost_domain: None,
        }
    }

    /// Opened, not yet closed. A trace that is closed with one of these still in
    /// it turns it into `Cancelled` (§9's fifth state).
    pub fn open(stage: Stage, started_ns: u64, granularity: Granularity) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Started,
            started_ns: Some(started_ns),
            ended_ns: None,
            duration_ns: None,
            granularity: Some(granularity),
            note: None,
            cost_domain: None,
        }
    }

    /// §9: the lifecycle reached this stage and the stage did not finish.
    pub fn failed(stage: Stage, started_ns: u64, ended_ns: u64, note: &str) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Failed,
            started_ns: Some(started_ns),
            ended_ns: Some(ended_ns),
            duration_ns: Some(ended_ns.saturating_sub(started_ns)),
            granularity: Some(Granularity::Nanosecond),
            note: Some(note.to_string()),
            cost_domain: None,
        }
    }

    /// §9's `Failed`, read off a stamp that exists in milliseconds: the lifecycle
    /// entered this stage — its entering rung was written — and never wrote the next
    /// one. There is a start, no end, and deliberately no duration: the instant the
    /// run gave up was never recorded, and inventing it to fill the field is §46's
    /// forbidden synthetic timestamp.
    pub fn failed_ms(stage: Stage, started_ms: u64, note: &str) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Failed,
            started_ns: Some(started_ms.saturating_mul(Granularity::Millisecond.factor())),
            ended_ns: None,
            duration_ns: None,
            granularity: Some(Granularity::Millisecond),
            note: Some(note.to_string()),
            cost_domain: None,
        }
    }

    /// §9: the lifecycle stopped short of this stage. No duration, by design.
    pub fn skipped(stage: Stage, note: &str) -> Self {
        Self {
            stage,
            outcome: StageOutcome::Skipped,
            started_ns: None,
            ended_ns: None,
            duration_ns: None,
            granularity: None,
            note: Some(note.to_string()),
            cost_domain: None,
        }
    }

    /// The record's own value for a hop that needs this stage's `ended`, or `None`
    /// when it has no instant — which is what keeps an unmeasured hop out of the
    /// percentile table instead of pulling it to zero (§13, §29).
    fn ended(&self) -> Option<u64> {
        self.ended_ns
    }

    fn started(&self) -> Option<u64> {
        self.started_ns
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "stage": self.stage.as_str(),
            "half": self.stage.half().as_str(),
            "domain": self.domain().as_str(),
            "nominal_domain": self.stage.domain().as_str(),
            "outcome": self.outcome.as_str(),
            "started_ns": self.started_ns,
            "ended_ns": self.ended_ns,
            "duration_ns": self.duration_ns,
            "granularity": self.granularity.map(Granularity::as_str),
            "note": self.note,
        })
    }
}

/// The five end-to-end latencies §13 asks a report to state.
///
/// Every field is an `Option`: a run that never executed an opportunity has no
/// `EndToEndExecutionLatency`, and §13 requires that to be reported as N/A rather
/// than as the zero a struct with `u64` fields would be forced to hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndToEnd {
    pub detection_ns: Option<u64>,
    pub execution_preparation_ns: Option<u64>,
    pub inclusion_ns: Option<u64>,
    pub settlement_ns: Option<u64>,
    pub end_to_end_ns: Option<u64>,
}

impl EndToEnd {
    fn to_json(self) -> serde_json::Value {
        json!({
            "detection_latency_ns": self.detection_ns,
            "execution_preparation_latency_ns": self.execution_preparation_ns,
            "inclusion_latency_ns": self.inclusion_ns,
            "settlement_latency_ns": self.settlement_ns,
            "end_to_end_latency_ns": self.end_to_end_ns,
        })
    }
}

/// One opportunity's lifecycle, from "this block exists" to "the profit was
/// verified" — or to wherever the run stopped, which is the more common case and
/// is a result rather than a gap (§13, §32).
///
/// Owned by the thing running the lifecycle and handed to it as `&mut` (§39: no
/// global). A trace is not a registry, not a queue, and not shared: two
/// opportunities are two traces, and the only cross-trace structure is the
/// baseline that aggregates them after the run.
#[derive(Clone, Debug)]
pub struct LatencyTrace {
    trace_id: String,
    source: TraceSource,
    chain_id: u64,
    opportunity_block: Option<u64>,
    opportunity_tx_index: Option<u64>,
    /// The finding's own stable identity, reused rather than duplicated (§4). The
    /// pipeline already names a finding `OpportunityId`; this field holds that
    /// same string so a reader can join a trace to `opportunities.jsonl`.
    opportunity_id: Option<String>,
    started_ns: Option<u64>,
    completed_ns: Option<u64>,
    closed: bool,
    stages: BTreeMap<Stage, StageRecord>,
}

impl LatencyTrace {
    /// A trace for one named opportunity, with an id derived from its identity.
    pub fn new(
        source: TraceSource,
        chain_id: u64,
        opportunity_block: Option<u64>,
        opportunity_id: Option<&str>,
    ) -> Self {
        let trace_id = derive_trace_id(source, chain_id, opportunity_block, opportunity_id);
        Self {
            trace_id,
            source,
            chain_id,
            opportunity_block,
            opportunity_tx_index: None,
            opportunity_id: opportunity_id.map(str::to_string),
            started_ns: None,
            completed_ns: None,
            closed: false,
            stages: BTreeMap::new(),
        }
    }

    /// A trace for a block that was observed but produced nothing to run.
    ///
    /// §32 lets a live session find zero opportunities, and a run that found none
    /// still has a discovery-side latency worth baselining; the identity it has is
    /// the block, so that is what the id is derived from.
    pub fn for_block(source: TraceSource, chain_id: u64, block_number: u64) -> Self {
        Self::new(source, chain_id, Some(block_number), None)
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn source(&self) -> TraceSource {
        self.source
    }

    /// The chain the lifecycle ran against, in the baseline's metadata (§44). A
    /// plain number rather than a core type: this crate holds the evidence shapes
    /// and depends on no other crate in the workspace.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn set_tx_index(&mut self, index: Option<u64>) {
        self.opportunity_tx_index = index;
    }

    /// Name the opportunity this lifecycle is running, once it has a name.
    ///
    /// A route run cannot know the finding's stable id before it has read the two
    /// venues, but §16's Observation stage belongs to the same lifecycle and starts
    /// before that read. So the trace opens on the block, and this call adopts the id
    /// and re-derives `trace_id` from it rather than keeping a weaker key: §4 asks the
    /// trace to carry the opportunity's identity, and a second run of the same route
    /// must still produce the same id.
    pub fn set_opportunity(&mut self, opportunity_id: &str) {
        let already_named = self
            .opportunity_id
            .as_deref()
            .is_some_and(|previous| previous == opportunity_id);
        if already_named {
            return;
        }
        self.opportunity_id = Some(opportunity_id.to_string());
        self.trace_id = derive_trace_id(
            self.source,
            self.chain_id,
            self.opportunity_block,
            self.opportunity_id.as_deref(),
        );
    }

    pub fn opportunity_id(&self) -> Option<&str> {
        self.opportunity_id.as_deref()
    }

    pub fn set_started(&mut self, started_ns: u64) {
        if self.started_ns.is_none() {
            self.started_ns = Some(started_ns);
        }
    }

    pub fn started_ns(&self) -> Option<u64> {
        self.started_ns
    }

    /// The stage's record, if this lifecycle produced one.
    pub fn stage(&self, stage: Stage) -> Option<&StageRecord> {
        self.stages.get(&stage)
    }

    /// Every record this lifecycle wrote, in stage order.
    ///
    /// [`Self::stage`] answers a question about one named stage; M8.3.2 §14 asks the
    /// opposite one — a call is already stamped, and which stage held it is what has to be
    /// found out. That needs the set at once, and the map is keyed by [`Stage`], which
    /// derives `Ord` in the order the lifecycle enters them, so this yields the stages in
    /// the order a reader expects rather than the order they happened to be written in.
    /// Read-only: a span a caller could edit from here would not be a measurement.
    pub fn stage_records(&self) -> impl Iterator<Item = &StageRecord> {
        self.stages.values()
    }

    /// Open a stage at a nanosecond reading of the run's clock.
    ///
    /// The first writing call on most traces: the caller stamps from the clock it
    /// already holds, so the cost here is one map insert (§38, §15's O(1)).
    pub fn begin(&mut self, stage: Stage, started_ns: u64) -> Result<(), TraceError> {
        self.guard_open()?;
        if self.stages.contains_key(&stage) {
            // The record that stands is the first one, which is the measurement to
            // keep; opening a stage twice is a wiring bug and says so.
            return Err(TraceError::AlreadyRecorded { stage });
        }
        self.stages.insert(
            stage,
            StageRecord::open(stage, started_ns, Granularity::Nanosecond),
        );
        self.set_started(started_ns);
        Ok(())
    }

    /// Open a stage from a millisecond stamp that already exists in the evidence
    /// path, so a hop read off M5's timing table lands in the same record shape as
    /// one measured directly.
    pub fn begin_ms(&mut self, stage: Stage, started_ms: u64) -> Result<(), TraceError> {
        self.guard_open()?;
        if self.stages.contains_key(&stage) {
            return Err(TraceError::AlreadyRecorded { stage });
        }
        let started_ns = started_ms.saturating_mul(Granularity::Millisecond.factor());
        self.stages.insert(
            stage,
            StageRecord::open(stage, started_ns, Granularity::Millisecond),
        );
        self.set_started(started_ns);
        Ok(())
    }

    /// Close an open stage. An error here leaves the open record untouched: §34's
    /// T9 is exactly this — a double finish must not corrupt the first duration.
    pub fn complete(&mut self, stage: Stage, ended_ns: u64) -> Result<(), TraceError> {
        self.guard_open()?;
        let record = self
            .stages
            .get(&stage)
            .ok_or(TraceError::NotStarted { stage })?;
        let granularity = record.granularity.unwrap_or(Granularity::Nanosecond);
        let (started_ns, outcome, cost_domain) =
            (record.started_ns, record.outcome, record.cost_domain);
        if outcome.is_terminal() {
            return Err(TraceError::AlreadyClosed { stage, outcome });
        }
        let started_ns = started_ns.unwrap_or(ended_ns);
        self.stages.insert(
            stage,
            StageRecord {
                stage,
                outcome: StageOutcome::Completed,
                started_ns: Some(started_ns),
                ended_ns: Some(ended_ns),
                duration_ns: Some(ended_ns.saturating_sub(started_ns)),
                granularity: Some(granularity),
                note: None,
                cost_domain,
            },
        );
        Ok(())
    }

    /// Close an open stage with the stage having run and not produced an answer.
    /// §16's requirement that a revert, a timeout and an error each terminate the
    /// trace, with their own state, is what this is for.
    pub fn fail(&mut self, stage: Stage, ended_ns: u64, note: &str) -> Result<(), TraceError> {
        self.guard_open()?;
        let record = self
            .stages
            .get(&stage)
            .ok_or(TraceError::NotStarted { stage })?;
        if record.outcome.is_terminal() {
            return Err(TraceError::AlreadyClosed {
                stage,
                outcome: record.outcome,
            });
        }
        let started_ns = record.started_ns.unwrap_or(ended_ns);
        self.stages.insert(
            stage,
            StageRecord {
                stage,
                outcome: StageOutcome::Failed,
                started_ns: Some(started_ns),
                ended_ns: Some(ended_ns),
                duration_ns: Some(ended_ns.saturating_sub(started_ns)),
                granularity: record.granularity,
                note: Some(note.to_string()),
                cost_domain: record.cost_domain,
            },
        );
        Ok(())
    }

    /// Record that the lifecycle did not reach a stage — with no duration (§9).
    ///
    /// This is the write that makes §34's T2–T5 expressible: a run declined at
    /// Risk has Build, Sign and Submit present as skips, which is a different
    /// statement from those stages being absent and a different one again from
    /// them being measured at zero.
    pub fn skip(&mut self, stage: Stage, note: &str) -> Result<(), TraceError> {
        self.guard_open()?;
        if self.stages.contains_key(&stage) {
            return Err(TraceError::AlreadyRecorded { stage });
        }
        self.stages.insert(stage, StageRecord::skipped(stage, note));
        Ok(())
    }

    /// Place a record that was produced elsewhere — a duration read from M7's
    /// evidence, or a span derived from the execution ledger. Refused, not
    /// overwritten, if this trace already holds a record for the stage.
    pub fn record(&mut self, record: StageRecord) -> Result<(), TraceError> {
        self.guard_open()?;
        let stage = record.stage;
        if self.stages.contains_key(&stage) {
            return Err(TraceError::AlreadyRecorded { stage });
        }
        self.set_started_from(&record);
        self.stages.insert(stage, record);
        Ok(())
    }

    /// Say, after the fact, which cost class one recorded stage's span belongs to,
    /// with the run's own words beside it.
    ///
    /// This is the §11 correction for a span whose contents disagree with its stage
    /// name, and it is deliberately a statement the caller makes rather than a class
    /// this crate infers: only the run knows whether the state a simulation computed
    /// on came from a node or from a file. The record must already exist — this
    /// relabels a measurement, it never opens one — and the duration is untouched, so
    /// a relabelled stage moves between the two sums of §11 without changing any
    /// number in §12.
    pub fn reclassify(
        &mut self,
        stage: Stage,
        domain: Domain,
        note: &str,
    ) -> Result<(), TraceError> {
        self.guard_open()?;
        let record = self
            .stages
            .get_mut(&stage)
            .ok_or(TraceError::NotStarted { stage })?;
        if record.duration_ns.is_none() {
            // A skip, a cancel and an open record have no span to put in a sum, so a
            // class would be a label on nothing — and rewriting such a record's reason
            // is not what this call is for.
            return Err(TraceError::Unmeasured { stage });
        }
        record.cost_domain = Some(domain);
        record.note = Some(match record.note.take() {
            Some(previous) => format!("{previous} | {note}"),
            None => note.to_string(),
        });
        Ok(())
    }

    /// End the lifecycle. Every stage still open becomes `Cancelled` (§9) and the
    /// completion instant is recorded so `wall_clock_total` has both ends (§10).
    ///
    /// The far end is recorded **only when the trace has a near end**, which is the case
    /// for a lifecycle this process walked and not for one assembled from another run's
    /// recorded durations (`crates/pipeline/src/history.rs`). A lone completion instant
    /// from the loading process's own clock would be a number beside a lifecycle it never
    /// measured — §46's invented timestamp, and the reason §10's two totals are both
    /// `null` there rather than one of them being a real figure.
    pub fn close(&mut self, completed_ns: u64) {
        for record in self.stages.values_mut() {
            if record.outcome == StageOutcome::Started {
                record.outcome = StageOutcome::Cancelled;
                record.note = Some(
                    "the lifecycle ended while this stage was open; no duration is claimed"
                        .to_string(),
                );
            }
        }
        if self.started_ns.is_some() {
            self.completed_ns = Some(completed_ns);
        }
        self.closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn guard_open(&self) -> Result<(), TraceError> {
        if self.closed {
            return Err(TraceError::TraceClosed);
        }
        Ok(())
    }

    fn set_started_from(&mut self, record: &StageRecord) {
        if let (None, Some(started)) = (self.started_ns, record.started()) {
            self.started_ns = Some(started);
        }
    }

    /// How a stage's own work went: its span. `None` for a skip, a cancel, an
    /// open stage, or a stage that was never in this lifecycle.
    pub fn span_ns(&self, stage: Stage) -> Option<u64> {
        self.stages.get(&stage)?.duration_ns
    }

    /// The wait between two consecutive stages: from the first one's answer to
    /// the second one starting (§16–§24's "gap" hops). `None` whenever either end
    /// lacks an instant, which is the honest answer for a duration-only record.
    pub fn gap_ns(&self, from: Stage, to: Stage) -> Option<u64> {
        let first = self.stages.get(&from)?.ended()?;
        let second = self.stages.get(&to)?.started()?;
        Some(second.saturating_sub(first))
    }

    /// From the start of one stage to the end of another: the §13 end-to-end
    /// shape, which is a milestone difference rather than a sum, so a slow stage
    /// in the middle is counted once.
    pub fn milestone_ns(&self, from: Stage, to: Stage) -> Option<u64> {
        let first = self.stages.get(&from)?.started()?;
        let second = self.stages.get(&to)?.ended()?;
        Some(second.saturating_sub(first))
    }

    /// §12's named hops, in order, with the §12 names.
    ///
    /// A `None` here serializes as JSON `null`, which is what §13 and §30 ask of a
    /// latency that was never measured: it stays a question rather than becoming a
    /// fast-looking answer.
    pub fn hops(&self) -> Vec<(&'static str, Option<u64>)> {
        use Stage::*;
        vec![
            (
                "observation_to_opportunity",
                self.milestone_ns(Observation, OpportunityDetection),
            ),
            (
                "opportunity_to_simulation_start",
                self.gap_ns(OpportunityDetection, Simulation),
            ),
            ("simulation_duration", self.span_ns(Simulation)),
            ("simulation_to_risk", self.gap_ns(Simulation, Risk)),
            ("risk_duration", self.span_ns(Risk)),
            ("risk_to_preflight", self.gap_ns(Risk, Preflight)),
            ("preflight_duration", self.span_ns(Preflight)),
            ("build_duration", self.span_ns(Build)),
            ("sign_duration", self.span_ns(Sign)),
            ("submit_duration", self.span_ns(Submit)),
            // §22 and §23: the submission's own RPC answer, then the wait for the
            // transaction to be in a block, then the wait for the receipt. Each is
            // one stage's span, so none of the three swallows another.
            ("submit_to_inclusion", self.span_ns(Inclusion)),
            ("inclusion_to_receipt", self.span_ns(Receipt)),
            ("receipt_to_settlement", self.span_ns(Settlement)),
            (
                "settlement_to_profit_verified",
                self.span_ns(ProfitVerification),
            ),
            (
                "opportunity_to_submit",
                self.milestone_ns(OpportunityDetection, Submit),
            ),
            (
                "opportunity_to_inclusion",
                self.milestone_ns(OpportunityDetection, Inclusion),
            ),
            (
                "opportunity_to_profit_verified",
                self.milestone_ns(OpportunityDetection, ProfitVerification),
            ),
        ]
    }

    /// §13's five, assembled from the same records rather than from a second set
    /// of stamps.
    pub fn end_to_end(&self) -> EndToEnd {
        use Stage::*;
        EndToEnd {
            detection_ns: self.milestone_ns(Observation, OpportunityDetection),
            execution_preparation_ns: self.milestone_ns(OpportunityDetection, Submit),
            inclusion_ns: self.milestone_ns(Submit, Inclusion),
            settlement_ns: self.milestone_ns(Inclusion, Settlement),
            end_to_end_ns: self.milestone_ns(Observation, ProfitVerification),
        }
    }

    /// §10: how long the lifecycle took, end to end on the wall of this run — the
    /// span from the trace's first stamp to its last, including the time between
    /// stages.
    pub fn wall_clock_total_ns(&self) -> Option<u64> {
        let first = self.stages.get(&Stage::Observation)?.started()?;
        let last = self.completed_ns?;
        Some(last.saturating_sub(first))
    }

    /// §10: the sum of the stages that were measured. Kept a separate number from
    /// [`LatencyTrace::wall_clock_total_ns`] on purpose: the two are equal only
    /// when every stage ran back to back with no queueing and no wait for the
    /// chain, and the difference between them is the queueing and the chain. A
    /// later milestone that runs stages in parallel will make them differ
    /// outright, which is why neither is derived from the other.
    pub fn stage_duration_sum_ns(&self) -> Option<u64> {
        let mut sum: Option<u64> = None;
        for record in self.stages.values() {
            if let Some(duration) = record.duration_ns {
                sum = Some(sum.unwrap_or(0).saturating_add(duration));
            }
        }
        sum
    }

    /// §11's split, over the class each span actually earned rather than the class its
    /// stage name suggests (see [`StageRecord::cost_domain`]). `Mixed` is counted in
    /// neither of the other two sums and shows up on its own, so a reader can add it to
    /// whichever side they argue it belongs to rather than having this file have
    /// decided for them.
    pub fn cost_split_ns(&self) -> CostSplit {
        let mut split = CostSplit::default();
        for record in self.stages.values() {
            let Some(duration) = record.duration_ns else {
                continue;
            };
            match record.domain() {
                Domain::Local => split.local_ns = Some(split.local_ns.unwrap_or(0) + duration),
                Domain::Network => {
                    split.network_ns = Some(split.network_ns.unwrap_or(0) + duration)
                }
                Domain::Mixed => split.mixed_ns = Some(split.mixed_ns.unwrap_or(0) + duration),
            }
        }
        split
    }

    /// The trace as one evidence line (§25's `traces.jsonl`, §26's stable schema).
    ///
    /// `stages` always holds fourteen entries in lifecycle order, with `null` for
    /// a stage this lifecycle never reached. The shape does not depend on which
    /// stages ran, so a reader parsing line 3 of a reverted run and line 3 of a
    /// settled one is reading the same columns — and a missing stage cannot make
    /// this fail (§34's T10).
    pub fn to_json(&self) -> serde_json::Value {
        let stages: Vec<serde_json::Value> = Stage::ALL
            .iter()
            .map(|stage| match self.stages.get(stage) {
                Some(record) => record.to_json(),
                None => json!({
                    "stage": stage.as_str(),
                    "half": stage.half().as_str(),
                    "domain": stage.domain().as_str(),
                    "nominal_domain": stage.domain().as_str(),
                    "outcome": "absent",
                    "started_ns": null,
                    "ended_ns": null,
                    "duration_ns": null,
                    "granularity": null,
                    "note": "this lifecycle produced no record for the stage",
                }),
            })
            .collect();
        let hops: serde_json::Map<String, serde_json::Value> = self
            .hops()
            .into_iter()
            .map(|(name, value)| (name.to_string(), json!(value)))
            .collect();
        let split = self.cost_split_ns();
        json!({
            "trace_schema": TRACE_SCHEMA,
            "trace_id": self.trace_id,
            "source": self.source.as_str(),
            "chain_id": self.chain_id,
            "opportunity_block": self.opportunity_block,
            "opportunity_tx_index": self.opportunity_tx_index,
            "opportunity_id": self.opportunity_id,
            "started_ns": self.started_ns,
            "completed_ns": self.completed_ns,
            "closed": self.closed,
            "stages": stages,
            "hops_ns": hops,
            "end_to_end_ns": self.end_to_end().to_json(),
            "totals_ns": {
                "wall_clock_total": self.wall_clock_total_ns(),
                "stage_duration_sum": self.stage_duration_sum_ns(),
                "local_processing": split.local_ns,
                "network_or_chain": split.network_ns,
                "mixed_reads_and_compute": split.mixed_ns,
            },
        })
    }
}

/// §11's three cost classes, summed over one trace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CostSplit {
    pub local_ns: Option<u64>,
    pub network_ns: Option<u64>,
    pub mixed_ns: Option<u64>,
}

/// §4's id: unique to one lifecycle, reproducible for one recorded opportunity,
/// and not confusable with a transaction hash, a block hash or a bare opportunity
/// id — it is a hash *of* those, prefixed, and 16 hex digits long so a reader
/// cannot mistake it for either of the 64-digit forms.
///
/// Deterministic rather than random, because §0's question 9 asks whether a replay
/// of one opportunity gives a consistent trace: an id that changed between replays
/// would make the comparison about the id instead of about the stages. It carries
/// no secret (§37) and no clock reading, so it is a fact about the opportunity, not
/// about the run that looked at it.
fn derive_trace_id(
    source: TraceSource,
    chain_id: u64,
    opportunity_block: Option<u64>,
    opportunity_id: Option<&str>,
) -> String {
    let key = format!(
        "{}|{}|{}|{}",
        source.as_str(),
        chain_id,
        opportunity_block.map_or("-".to_string(), |block| block.to_string()),
        opportunity_id.unwrap_or("-"),
    );
    let hash = alloy_primitives::keccak256(key.as_bytes());
    format!("lat-{}", &hash.to_string()[2..18])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The §34 T1 shape: a full lifecycle, every stage closed, no negative or
    /// zero-length duration anywhere.
    #[test]
    fn a_full_lifecycle_closes_every_stage_with_a_positive_duration() {
        let mut trace = LatencyTrace::new(TraceSource::Fixture, 91_342, Some(100), Some("opp-1"));
        for (index, stage) in Stage::ALL.iter().enumerate() {
            trace.begin(*stage, index as u64 * 1_000).expect("open");
            trace
                .complete(*stage, index as u64 * 1_000 + 500)
                .expect("close");
        }
        trace.close(15_000);
        for stage in Stage::ALL {
            let record = trace.stage(stage).expect("a record");
            assert_eq!(record.outcome, StageOutcome::Completed, "{stage}");
            assert!(
                record.duration_ns.unwrap_or(0) > 0,
                "{stage} has no duration"
            );
        }
        assert_eq!(trace.stage_duration_sum_ns(), Some(7_000));
        // Observation opened at 0 and the lifecycle was closed at 15_000, so the
        // wall total is the whole run, while the stage sum is only the time inside
        // the stages — the difference is the waiting between them (§10).
        assert_eq!(trace.wall_clock_total_ns(), Some(15_000));
        assert!(trace.wall_clock_total_ns() > trace.stage_duration_sum_ns());
        assert!(
            trace.end_to_end().end_to_end_ns.is_some(),
            "a settled lifecycle has an end-to-end latency"
        );
    }

    /// §34 T9: a second close is refused and the first measurement stands.
    #[test]
    fn finishing_a_stage_twice_does_not_corrupt_the_first_duration() {
        let mut trace = LatencyTrace::new(TraceSource::Fixture, 91_342, Some(1), Some("opp-2"));
        trace.begin(Stage::Simulation, 1_000).expect("open");
        trace.complete(Stage::Simulation, 3_000).expect("close");
        let error = trace
            .complete(Stage::Simulation, 9_000)
            .expect_err("a closed stage cannot be closed again");
        assert!(
            matches!(error, TraceError::AlreadyClosed { .. }),
            "wrong refusal: {error}"
        );
        assert_eq!(trace.span_ns(Stage::Simulation), Some(2_000));
        // The same guard on the failure path, so a late error cannot rewrite it.
        assert!(matches!(
            trace.fail(Stage::Simulation, 9_000, "late"),
            Err(TraceError::AlreadyClosed { .. })
        ));
        assert_eq!(trace.span_ns(Stage::Simulation), Some(2_000));
    }

    /// §34 T10: serialization does not depend on which stages are present.
    #[test]
    fn a_trace_missing_most_stages_still_serializes() {
        let mut trace = LatencyTrace::new(TraceSource::Live, 91_342, Some(7), Some("opp-3"));
        trace.begin(Stage::Observation, 10).expect("open");
        trace.complete(Stage::Observation, 20).expect("close");
        trace
            .skip(Stage::Build, "the run had no execution lane")
            .expect("skip");
        let line = trace.to_json();
        let stages = line["stages"].as_array().expect("an array");
        assert_eq!(stages.len(), 14, "the schema is fourteen rows whatever ran");
        assert_eq!(stages[0]["stage"], "observation");
        assert_eq!(stages[0]["duration_ns"], 10);
        assert_eq!(stages[7]["stage"], "build");
        assert_eq!(stages[7]["outcome"], "skipped");
        assert_eq!(
            stages[7]["duration_ns"],
            serde_json::Value::Null,
            "§9: a skip has no duration, so it can never be averaged as a fast stage"
        );
        assert_eq!(stages[5]["outcome"], "absent");
        assert_eq!(line["hops_ns"]["build_duration"], serde_json::Value::Null);
        assert_eq!(
            line["end_to_end_ns"]["end_to_end_latency_ns"],
            serde_json::Value::Null
        );
        assert!(serde_json::to_string(&line).is_ok());
    }

    /// §34 T8: no sequence of readings produces a negative duration.
    #[test]
    fn inverted_clock_readings_floor_at_zero_and_never_wrap() {
        let mut trace = LatencyTrace::new(TraceSource::Fixture, 1, None, None);
        trace.begin(Stage::Risk, 5_000).expect("open");
        // A close that arrives with a smaller reading than the open. The wrong
        // answer is 2^64-something showing up as a stage length.
        trace.complete(Stage::Risk, 4_000).expect("close");
        assert_eq!(trace.span_ns(Stage::Risk), Some(0));
        let record = StageRecord::measured_ms(Stage::Submit, 9_000, 1_000);
        assert_eq!(record.duration_ns, Some(0));
        let gap = {
            let mut trace = LatencyTrace::new(TraceSource::Fixture, 1, None, None);
            trace.begin(Stage::Simulation, 8_000).expect("open");
            trace.complete(Stage::Simulation, 8_000).expect("close");
            trace.begin(Stage::Risk, 1_000).expect("open");
            trace.complete(Stage::Risk, 1_000).expect("close");
            trace.gap_ns(Stage::Simulation, Stage::Risk)
        };
        assert_eq!(gap, Some(0));
    }

    /// §9's fifth state, reached by closing a trace that left a stage open.
    #[test]
    fn a_stage_still_open_when_the_lifecycle_ends_is_cancelled_not_zero() {
        let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(3), Some("opp-4"));
        trace.begin(Stage::Observation, 0).expect("open");
        trace.complete(Stage::Observation, 100).expect("close");
        trace.begin(Stage::Inclusion, 200).expect("open");
        trace.close(5_000);
        let record = trace.stage(Stage::Inclusion).expect("a record");
        assert_eq!(record.outcome, StageOutcome::Cancelled);
        assert_eq!(
            record.duration_ns, None,
            "an unfinished wait is not a duration"
        );
        assert_eq!(
            record.started_ns,
            Some(200),
            "what was observed stays observed"
        );
        // Writing to a closed trace is refused rather than silently accepted.
        assert!(matches!(
            trace.begin(Stage::Receipt, 6_000),
            Err(TraceError::TraceClosed)
        ));
    }

    #[test]
    fn a_record_can_only_be_placed_once_per_stage() {
        let mut trace = LatencyTrace::new(TraceSource::Replay, 1, Some(3), Some("opp-5"));
        trace
            .record(StageRecord::duration_only(
                Stage::Simulation,
                21,
                Granularity::Millisecond,
                "read from M7 evidence",
            ))
            .expect("first write");
        let error = trace
            .record(StageRecord::measured(Stage::Simulation, 0, 5))
            .expect_err("a second write is a bug, not a re-measurement");
        assert!(matches!(error, TraceError::AlreadyRecorded { .. }));
        assert_eq!(trace.span_ns(Stage::Simulation), Some(21_000_000));
        assert!(matches!(
            trace.complete(Stage::Simulation, 60),
            Err(TraceError::AlreadyClosed { .. })
        ));
    }

    #[test]
    fn closing_a_stage_that_never_opened_is_refused() {
        let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(3), Some("opp-6"));
        assert!(matches!(
            trace.complete(Stage::Build, 10),
            Err(TraceError::NotStarted { .. })
        ));
        assert!(trace.stage(Stage::Build).is_none());
    }

    /// §4: the id names the lifecycle, is stable across replays of the same
    /// opportunity, and does not collide with the other identities it could be
    /// mistaken for.
    #[test]
    fn trace_ids_are_derived_from_the_opportunity_not_from_the_run() {
        let first = derive_trace_id(TraceSource::Replay, 91_342, Some(37_563_264), Some("opp-a"));
        let second = derive_trace_id(TraceSource::Replay, 91_342, Some(37_563_264), Some("opp-a"));
        assert_eq!(first, second, "a replay of one opportunity is one trace");
        assert!(first.starts_with("lat-"), "{first}");
        assert_eq!(first.len(), 20, "{first}");
        assert_ne!(
            first,
            derive_trace_id(TraceSource::Live, 91_342, Some(37_563_264), Some("opp-a"))
        );
        assert_ne!(
            first,
            derive_trace_id(TraceSource::Replay, 91_342, Some(37_563_265), Some("opp-a"))
        );
        assert_ne!(
            first,
            derive_trace_id(TraceSource::Replay, 91_342, Some(37_563_264), Some("opp-b"))
        );
        for forbidden in ["0x", "opportunity", "tx"] {
            assert!(!first.contains(forbidden), "{first} leaks {forbidden}");
        }
    }

    /// §11 and §14, as data rather than as prose.
    #[test]
    fn stages_are_classified_by_half_and_by_cost_domain() {
        assert_eq!(Stage::Observation.half(), Half::Discovery);
        assert_eq!(Stage::OpportunityDetection.half(), Half::Discovery);
        assert_eq!(Stage::Simulation.half(), Half::Execution);
        assert_eq!(Stage::ProfitVerification.half(), Half::Execution);
        assert_eq!(Stage::Sign.domain(), Domain::Local);
        assert_eq!(Stage::Inclusion.domain(), Domain::Network);
        assert_eq!(Stage::Preflight.domain(), Domain::Mixed);

        let mut trace = LatencyTrace::new(TraceSource::Fixture, 1, Some(1), Some("opp-7"));
        for (index, stage) in Stage::ALL.iter().enumerate() {
            trace.begin(*stage, index as u64 * 1_000).expect("open");
            trace
                .complete(*stage, index as u64 * 1_000 + 100)
                .expect("close");
        }
        let split = trace.cost_split_ns();
        // Seven local stages × 100 ns, six network stages × 100 ns, preflight alone.
        assert_eq!(split.local_ns, Some(700));
        assert_eq!(split.network_ns, Some(600));
        assert_eq!(split.mixed_ns, Some(100));
        assert_eq!(
            split.local_ns.unwrap() + split.network_ns.unwrap() + split.mixed_ns.unwrap(),
            trace.stage_duration_sum_ns().expect("a sum"),
            "the split covers the sum exactly, so nothing is double-counted or dropped"
        );
    }

    /// §11's second half: the class a stage's *name* suggests and the class a span
    /// earned are not always the same thing, and the sums have to follow the span. The
    /// reclassification moves 500 ns out of `local` and into `mixed`; no duration
    /// changes, and the row keeps both labels so a reader can see what was overridden.
    #[test]
    fn reclassifying_a_span_moves_the_time_between_the_two_sums() {
        let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(7), Some("opp-11"));
        for (index, stage) in [Stage::Simulation, Stage::Risk].iter().enumerate() {
            trace.begin(*stage, index as u64 * 1_000).expect("open");
            trace
                .complete(*stage, index as u64 * 1_000 + 500)
                .expect("close");
        }
        assert_eq!(
            trace.stage(Stage::Simulation).expect("row").domain(),
            Domain::Local,
            "the nominal class of a simulation is local work"
        );
        let before = trace.cost_split_ns();
        assert_eq!(before.local_ns, Some(1_000));
        assert_eq!(before.mixed_ns, None);

        trace
            .reclassify(
                Stage::Simulation,
                Domain::Mixed,
                "state read from rpc:chain-1",
            )
            .expect("the stage is measured, so it has a span to class");
        let after = trace.cost_split_ns();
        assert_eq!(after.local_ns, Some(500), "only Risk is left as CPU work");
        assert_eq!(after.mixed_ns, Some(500));
        assert_eq!(
            trace.stage(Stage::Simulation).expect("row").duration_ns,
            Some(500),
            "classing a span changes its label and not its length"
        );
        assert_eq!(trace.stage_duration_sum_ns(), Some(1_000));

        let rows = trace.to_json();
        let index = Stage::ALL
            .iter()
            .position(|stage| *stage == Stage::Simulation)
            .expect("in the list");
        let row = &rows["stages"][index];
        assert_eq!(row["domain"], json!("mixed"));
        assert_eq!(row["nominal_domain"], json!("local"));
    }

    /// A skip has no span to class, and rewriting the reason it carries would let a
    /// later label overwrite why the lifecycle stopped where it did.
    #[test]
    fn a_stage_with_no_measured_span_refuses_to_be_classified() {
        let mut trace = LatencyTrace::new(TraceSource::Live, 1, Some(8), Some("opp-12"));
        trace
            .skip(Stage::Simulation, "the run never reached the EVM")
            .expect("open");
        let error = trace
            .reclassify(Stage::Simulation, Domain::Mixed, "rpc:chain-1")
            .expect_err("a skip carries no duration to move");
        assert!(matches!(
            error,
            TraceError::Unmeasured {
                stage: Stage::Simulation
            }
        ));
        assert_eq!(
            trace.stage(Stage::Simulation).expect("row").note.as_deref(),
            Some("the run never reached the EVM"),
            "the skip's own reason stands"
        );
    }

    #[test]
    fn the_hop_table_is_the_names_the_spec_asks_for_and_in_order() {
        let trace = LatencyTrace::new(TraceSource::Fixture, 1, Some(1), Some("opp-8"));
        let hops = trace.hops();
        let names: Vec<&str> = hops.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, HOP_NAMES.to_vec());
        assert!(
            hops.iter().all(|(_, value)| value.is_none()),
            "an untouched trace measures no hop"
        );
    }

    /// §13: an unexecuted opportunity has N/A end-to-end figures, and the way a
    /// duration-only record behaves is what makes that true rather than a special
    /// case in the writer.
    #[test]
    fn a_duration_only_record_gives_a_span_but_no_milestone() {
        let mut trace = LatencyTrace::new(TraceSource::Replay, 1, Some(1), Some("opp-9"));
        trace
            .record(StageRecord::duration_only(
                Stage::Simulation,
                21_449,
                Granularity::Millisecond,
                "latency_ms.opportunity_detection_latency_ms in M7's route-run.json",
            ))
            .expect("historical write");
        assert_eq!(trace.span_ns(Stage::Simulation), Some(21_449_000_000));
        assert_eq!(
            trace.gap_ns(Stage::OpportunityDetection, Stage::Simulation),
            None
        );
        assert_eq!(trace.end_to_end().end_to_end_ns, None);
        let json = trace.to_json();
        assert_eq!(json["stages"][4]["granularity"], "millisecond");
        assert_eq!(json["stages"][4]["started_ns"], serde_json::Value::Null);
    }

    #[test]
    fn millisecond_stamps_and_nanosecond_stamps_share_one_timeline() {
        // The two granularities have to be addable and comparable, because a
        // trace reads its execution half off stamps M6/M7 already wrote in ms and
        // its discovery half off new ns readings.
        let record = StageRecord::measured_ms(Stage::Build, 52_971, 53_743);
        assert_eq!(record.duration_ns, Some(772_000_000));
        assert_eq!(record.granularity, Some(Granularity::Millisecond));
        let mut trace = LatencyTrace::new(TraceSource::Fixture, 1, Some(1), Some("opp-10"));
        trace.begin_ms(Stage::Sign, 53_743).expect("open at ms");
        trace.complete(Stage::Sign, 53_743_000_500).expect("close");
        assert_eq!(trace.span_ns(Stage::Sign), Some(500));
        assert_eq!(
            trace.stage(Stage::Sign).expect("a record").granularity,
            Some(Granularity::Millisecond)
        );
    }
}
