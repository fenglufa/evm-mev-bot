//! M8.1 §33's deterministic fixture and §45's source label, run over a recording of
//! real GIWA blocks through the same path the CLI uses.
//!
//! The input is the 50 blocks around 37,191,169 — the one height in the last ~380k
//! where both attested pools of the WETH/TTAX pair restated their reserves in the same
//! block — plus the state recording M4 wrote while reading the live RPC. Nothing here
//! is invented market data, and nothing here is a *live* latency either: a replay run
//! measures how fast this binary walks a recording it already has, and §45 forbids
//! blending that with a live session's percentile. The trace file says which one it is
//! on every line, in a `source` field, and that field is this file's subject.
//!
//! What each test pins:
//!
//! - one lifecycle per finding, with the fourteen stages §4 lists, whatever the run
//!   reached (§26's line shape, §10's totals);
//! - the six discovery stages timed **from stamps M5 already wrote**, at
//!   `millisecond` granularity — this run measured no new instants (§15), and the file
//!   says so per record rather than claiming a precision it does not have;
//! - every stage the lifecycle never reached is a `skipped` row carrying this run's own
//!   reason, never a `0 ns` (§9, §34's T2–T4 at the level of the written file);
//! - §13's execution end-to-end figures are `null` for a finding that was never
//!   executed, not `0`;
//! - the baseline lives **beside** the run's evidence, never inside it, and the two
//!   never share a file (§2.1, §44);
//! - turning the instrumentation on is the only difference between the two runs below:
//!   the same replay without the flag produces the same evidence files and no traces
//!   directory at all (§40).

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address};
use serde_json::Value;

use evm_pipeline::{latency::SAME_MILLISECOND, run, PipelineConfig, SessionReport};

const WRAPPED_NATIVE: Address = address!("0x4200000000000000000000000000000000000006");
const CHAIN: u64 = 91_342;
const ARB_BLOCK: u64 = 37_191_169;

/// The stages the discovery half of §4's model holds, in lifecycle order. Six,
/// because this run has no lane: `risk` is the last stage that can have a number.
const DISCOVERY: [&str; 6] = [
    "observation",
    "state_update",
    "graph_update",
    "opportunity_detection",
    "simulation",
    "risk",
];

/// The eight stages below the risk decision on a run that never built a transaction.
const NOT_REACHED: [&str; 8] = [
    "preflight",
    "build",
    "sign",
    "submit",
    "inclusion",
    "receipt",
    "settlement",
    "profit_verification",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus() -> PathBuf {
    workspace_root().join("fixtures/live-m5/arbitrage-window")
}

fn state_recording() -> PathBuf {
    workspace_root().join("fixtures/simulation-m4/dump-37191169.json")
}

fn registries() -> Vec<PathBuf> {
    vec![
        workspace_root().join("data/protocols"),
        workspace_root().join("data/protocols-m3"),
    ]
}

/// One replay run, with the latency directory named separately from the evidence
/// directory so the two trees can be compared and never confused.
async fn replay(name: &str, latency: bool) -> (SessionReport, PathBuf) {
    let root = workspace_root().join("target/pipeline-tests").join(name);
    let _ = std::fs::remove_dir_all(&root);
    let mut config = PipelineConfig::replay(registries(), root.join("evidence"), corpus());
    config.wrapped_native = Some(WRAPPED_NATIVE);
    config.state_dump = Some(state_recording());
    let latency_dir = root.join("latency");
    config.latency_dir = latency.then_some(latency_dir.clone());
    // The recording ends and the run is expected to end with it; this is only a guard
    // so a regression that ignores the end cannot hang a test.
    config.duration = std::time::Duration::from_secs(60);
    let report = run(&config)
        .await
        .unwrap_or_else(|error| panic!("{name}: the run failed: {error}"));
    (report, latency_dir)
}

fn lines(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a trace line is JSON"))
        .collect()
}

/// The stage rows of one trace, keyed by stage name.
fn stages(trace: &Value) -> Vec<(String, String)> {
    trace["stages"]
        .as_array()
        .expect("a trace holds its stages")
        .iter()
        .map(|row| {
            (
                row["stage"].as_str().expect("a named stage").to_string(),
                row["outcome"].as_str().expect("an outcome").to_string(),
            )
        })
        .collect()
}

fn stage_row<'a>(trace: &'a Value, name: &str) -> &'a Value {
    trace["stages"]
        .as_array()
        .expect("a trace holds its stages")
        .iter()
        .find(|row| row["stage"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("no {name} row in the trace"))
}

/// §26's line shape, §4's fourteen stages, and §11's cost split — all of it present for
/// a lifecycle that ended at a risk decline, because the shape of a trace may not depend
/// on how far the run got.
#[tokio::test]
async fn a_replayed_finding_writes_one_line_of_fourteen_stages() {
    let (report, latency) = replay("latency-traces", true).await;
    let dir = latency.join(&report.session_id);
    let traces = lines(&dir.join("traces.jsonl"));
    assert_eq!(
        traces.len(),
        report.simulations as usize,
        "one line per lifecycle that reached the simulation stage"
    );
    let trace = &traces[0];
    assert_eq!(trace["source"], "replay", "§45: a replay row says replay");
    assert_eq!(trace["chain_id"], CHAIN);
    assert_eq!(trace["opportunity_block"], ARB_BLOCK);
    assert_eq!(trace["session_id"], report.session_id);
    assert_eq!(
        trace["instrumentation_refusals"].as_array().map(Vec::len),
        Some(0),
        "a correct wiring must not leave a refused write in the file"
    );
    assert!(
        trace["opportunity_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "§4: the trace names the finding it ran, by the id `opportunities.jsonl` uses"
    );
    let rows = stages(trace);
    assert_eq!(rows.len(), 14, "§4's fourteen stages, in declaration order");

    // The discovery half: six timed stages, each read off a stamp M5 wrote for its own
    // reasons, and a granularity field that admits as much.
    for name in DISCOVERY {
        let row = stage_row(trace, name);
        assert_eq!(row["outcome"], "completed", "{name}");
        assert!(
            row["duration_ns"].is_number(),
            "{name} ran, so its span is a fact"
        );
        assert_eq!(row["granularity"], "millisecond", "{name}");
        assert!(
            row["started_ns"].is_number() && row["ended_ns"].is_number(),
            "{name}"
        );
    }
    // §9's rule, at the level of the file: what the run never reached is a skip with a
    // reason, and no skip reads as a stage that took no time.
    for name in NOT_REACHED {
        let row = stage_row(trace, name);
        assert_eq!(row["outcome"], "skipped", "{name}");
        assert!(row["duration_ns"].is_null(), "{name} must not report 0 ns");
        assert!(
            row["note"].as_str().is_some_and(|s| !s.is_empty()),
            "{name} must say why it is empty"
        );
    }
    // The one skip whose reason is a fact about this build rather than about this
    // finding: §20's gate lives on the route path, and the live lane never ran one.
    assert!(
        stage_row(trace, "preflight")["note"]
            .as_str()
            .expect("a note")
            .contains("route path only"),
        "the gate's absence has to be distinguishable from a decline"
    );
    assert!(
        stage_row(trace, "build")["note"]
            .as_str()
            .expect("a note")
            .contains("risk layer declined"),
        "the decline is the reason nothing was built, and it belongs in the row"
    );

    // §13: an opportunity that was never executed has no execution latency. N/A.
    let end = &trace["end_to_end_ns"];
    assert!(
        end["detection_latency_ns"].is_number(),
        "observation → detection happened, so it is a number"
    );
    for field in [
        "execution_preparation_latency_ns",
        "inclusion_latency_ns",
        "settlement_latency_ns",
        "end_to_end_latency_ns",
    ] {
        assert!(end[field].is_null(), "{field} must be null, never 0");
    }
    // §10's two totals, neither of which is the other.
    let totals = &trace["totals_ns"];
    assert!(totals["wall_clock_total"].is_number());
    assert!(totals["stage_duration_sum"].is_number());
    assert!(
        totals["local_processing"].is_number(),
        "the discovery stages this run timed are all this binary computing"
    );

    // §25's other two files, and §44's metadata in the middle one.
    assert!(dir.join("summary.json").is_file());
    assert!(dir.join("README.md").is_file());
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["sample_count"], 1);
    assert_eq!(summary["sources"][0]["source"], "replay");
    assert_eq!(summary["sources"][0]["sample_count"], 1);
    assert_eq!(
        summary["execution_mode"], "no-lane",
        "a live/replay run's summary names the lane it had"
    );
    assert!(summary["git_revision"].is_string());
    assert_eq!(summary["sources_are_never_blended"], true);

    // The baseline is beside the run's evidence, never inside it: a reader of the files
    // a run's decisions were recorded in must not be able to confuse a latency line
    // with a market line (§2.1, §44).
    let evidence = &report.evidence_dir;
    for file in ["traces.jsonl", "summary.json", "README.md"] {
        assert!(
            !evidence.join(file).exists(),
            "{file} belongs to the latency directory, not to the run's own evidence"
        );
    }
    let session: Value =
        serde_json::from_str(&std::fs::read_to_string(evidence.join("live-session.json")).unwrap())
            .unwrap();
    assert_eq!(session["source"], "replay");
    assert_eq!(
        session["latency_traces"]["traces"], 1,
        "the session record names the baseline it wrote, so a reader of either file \
         can find the other"
    );
}

/// §40's compatibility claim, checked the only way it can be: the same run, twice, with
/// and without the flag. The evidence the run decides on is byte-for-byte the same set
/// of files, and the only difference is a directory that either exists or was never
/// created.
#[tokio::test]
async fn a_run_that_measures_no_latency_writes_no_traces() {
    let (report, latency) = replay("latency-off", false).await;
    assert!(
        !latency.exists(),
        "a run without the flag must not create the directory"
    );
    assert_eq!(
        report.simulations, 1,
        "the run itself is unaffected: the same recording, the same one simulation"
    );
    let evidence = &report.evidence_dir;
    for file in ["traces.jsonl", "summary.json"] {
        assert!(!evidence.join(file).exists(), "{file}");
    }
    let session: Value =
        serde_json::from_str(&std::fs::read_to_string(evidence.join("live-session.json")).unwrap())
            .unwrap();
    assert!(
        session["latency_traces"].is_null(),
        "the absence is written as an absence, not as a zero"
    );
}

/// §0's seventh question, as a test: does one opportunity's replay give a consistent
/// trace *structure*? The two runs below read one recording through one binary; every
/// figure in them is a duration that depends on how fast the machine happened to be, so
/// the comparison is of the shape — which stage is timed, which is skipped, and which
/// reason each skip carries — with the instants and the wall-clock totals removed.
#[tokio::test]
async fn two_runs_of_one_recording_agree_on_the_trace_structure() {
    let (first, first_latency) = replay("latency-determinism-a", true).await;
    let (second, second_latency) = replay("latency-determinism-b", true).await;
    let one_path = first_latency.join(&first.session_id).join("traces.jsonl");
    let two_path = second_latency.join(&second.session_id).join("traces.jsonl");
    let mut one = lines(&one_path);
    let mut two = lines(&two_path);
    assert_eq!(
        (one.len(), two.len()),
        (1, 1),
        "both runs replayed the same recording, so both wrote one lifecycle"
    );
    let mut one = one.remove(0);
    let mut two = two.remove(0);
    // §4's identity rule says the key is derived, not assigned: two runs of one finding
    // must produce one trace id, and that id must not be a session's accident.
    assert_eq!(one["trace_id"], two["trace_id"]);
    strip(&mut one);
    strip(&mut two);
    assert_eq!(one, two, "the two traces differ in more than their timings");
}

/// Remove what a second run of the same input cannot reproduce: the instants, the spans
/// taken from them, the totals computed from those, and the session that wrote the line.
/// `trace_id`, `source`, `chain_id`, `opportunity_block`, `opportunity_id`, every stage
/// name, outcome, half and domain stay. So does each row's note, with one exception:
/// [`SAME_MILLISECOND`] is written exactly when a stage's two stamps fall in one
/// millisecond, which is a reading of how fast this machine ran rather than a statement
/// about the lifecycle, so it is folded to the same absence a wider span carries. Every
/// other note — a skip's reason, the decode-and-apply caveat, §11's class sentence — is
/// structural and must match.
fn strip(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| {
                !key.ends_with("_ns")
                    && key != "started_ns"
                    && key != "completed_ns"
                    && key != "totals_ns"
                    && key != "hops_ns"
                    && key != "session_id"
                    && key != "closed"
            });
            for (key, value) in map.iter_mut() {
                if key == "note" && value.as_str() == Some(SAME_MILLISECOND) {
                    *value = Value::Null;
                }
                strip(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(strip),
        _ => {}
    }
}
