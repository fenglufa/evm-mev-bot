//! M8.1 §30's entry point, tested the way a person uses it: the real binary, over M7's
//! **real** run records, with the exit code and the files as the assertions.
//!
//! Three reasons this file drives a process rather than a struct. `baseline` has no config
//! to inspect — it reads files and writes files, so the observable contract *is* the exit
//! code plus what landed on disk. §44's "do not overwrite a run that is already evidence"
//! and §45's "one population per table" are rules about a *second* invocation, which only a
//! real process can make. And the sample this command measures is M7's own work: the runs
//! named below are the directories this repository holds real receipts for, so a test that
//! passed over a fabricated record would be testing nothing.
//!
//! Nothing here signs, submits, or opens an endpoint. `baseline` needs no `--rpc-url` and no
//! key variable, which the refusal tests below also establish: an incomplete command line
//! exits `2` for the flags and `1` for the evidence, and neither run writes a directory.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

/// The three runs §30 says to use: the one that settled across six real transactions, the
/// one §26's gate refused, and the one that stopped after building.
const SETTLED: &str = "data/evidence/m7/route-submit/route-91342-37563264-1790908382146";
const GATE_REFUSED: &str = "data/evidence/m7/route-submit/route-91342-37563031-1790908149030";
const BUILD_ONLY: &str = "data/evidence/m7/route-build-only/route-91342-37562423-1790907541327";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_dir(name: &str) -> String {
    workspace_root().join(name).display().to_string()
}

/// An output directory for one test, cleared first: §44 forbids a baseline from appending to
/// one already on disk, so two tests sharing a path would fail for the wrong reason.
fn fresh(name: &str) -> PathBuf {
    let dir = workspace_root().join("target/cli-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn lines(path: &Path) -> Vec<Value> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a trace line is JSON"))
        .collect()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// `baseline` with these arguments, in a process that inherits no endpoint and no key.
fn baseline(args: &[&str]) -> Output {
    let mut argv = vec!["baseline"];
    argv.extend_from_slice(args);
    std::process::Command::new(
        std::env::var("CARGO_BIN_EXE_evm-mev-bot")
            .expect("cargo gives an integration test its package's binary"),
    )
    .args(&argv)
    .env_remove("GIWA_RPC_URL")
    .env_remove("GIWA_WS_URL")
    .env_remove("GIWA_FLASHBLOCKS_URL")
    .env_remove("GIWA_EXECUTION_PRIVATE_KEY")
    .output()
    .expect("the binary runs")
}

/// §30 as a command: two of M7's `submit` runs in, one baseline out, and the figures are the
/// ones in the files.
#[test]
fn two_real_runs_become_one_baseline_and_the_summary_says_so() {
    let out = fresh("baseline-two-runs");
    let output = baseline(&[
        "--evidence-dir",
        &run_dir(SETTLED),
        "--evidence-dir",
        &run_dir(GATE_REFUSED),
        "--output",
        &out.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(printed.contains("2 trace(s)"), "{printed}");
    assert!(printed.contains("submit"), "{printed}");

    let traces = lines(&out.join("traces.jsonl"));
    assert_eq!(traces.len(), 2, "one line per run read");
    for trace in &traces {
        assert_eq!(
            trace["source"], "replay",
            "§45: these latencies were paid for by another process"
        );
        assert_eq!(trace["stages"].as_array().map(Vec::len), Some(14));
        assert_eq!(
            trace["started_ns"],
            Value::Null,
            "§46: no instant is invented"
        );
    }
    // The two runs are distinguishable by the identity M7 gave each finding, which the
    // trace reuses rather than renumbering (§14).
    let ids: Vec<&str> = traces
        .iter()
        .map(|trace| trace["opportunity_id"].as_str().unwrap_or_default())
        .collect();
    assert_ne!(ids[0], ids[1], "{ids:?}");
    assert_ne!(traces[0]["trace_id"], traces[1]["trace_id"]);

    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).expect("written"))
            .expect("the summary is JSON");
    assert_eq!(summary["sample_count"], 2);
    assert_eq!(summary["execution_mode"], "submit");
    assert_eq!(summary["sources"].as_array().map(Vec::len), Some(1));

    // §25's three files, and nothing that belongs to M7.
    assert!(out.join("README.md").is_file(), "§25 asks for a README");
    assert!(
        std::fs::read_dir(&out)
            .expect("a readable directory")
            .filter_map(|entry| entry.ok())
            .all(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name == "traces.jsonl" || name == "summary.json" || name == "README.md"
            }),
        "the baseline directory holds only the three files §25 names"
    );
}

/// `--json` prints the table the command just wrote, so a script can read the baseline
/// without knowing the directory layout — and reads the *same* figures, not a second
/// rendering of them.
#[test]
fn json_mode_prints_the_file_that_is_on_disk() {
    let out = fresh("baseline-json");
    let output = baseline(&[
        "--evidence-dir",
        &run_dir(SETTLED),
        "--output",
        &out.display().to_string(),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: Value = serde_json::from_str(&stdout(&output)).expect("--json is JSON");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).expect("written"))
            .expect("the summary is JSON");
    assert_eq!(printed, on_disk, "one rendering, two readers");
    assert_eq!(printed["sample_count"], 1);
    assert_eq!(printed["sources"][0]["source"], "replay");
    assert_eq!(printed["sources"][0]["chain_id"], 91_342);
}

/// A command line that names no runs cannot produce a baseline: exit `2` is "the flags were
/// wrong", and no directory appears where one was not asked to be created.
#[test]
fn a_baseline_over_no_runs_is_refused_and_leaves_no_directory() {
    let out = fresh("baseline-no-runs");
    let output = baseline(&["--output", &out.display().to_string()]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(text.contains("--evidence-dir"), "{text}");
    assert!(!out.exists(), "a refusal writes nothing: {}", out.display());
}

/// A path that holds no `route-run.json` is a fact about the evidence, not a row of nulls:
/// exit `1`, and the message names the file it could not open.
#[test]
fn a_directory_with_no_record_fails_loudly_instead_of_becoming_a_zero() {
    let out = fresh("baseline-missing-record");
    let stranded = workspace_root()
        .join("target/cli-tests/not-an-m7-run")
        .display()
        .to_string();
    let output = baseline(&[
        "--evidence-dir",
        &stranded,
        "--output",
        &out.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(text.contains("route-run.json"), "{text}");
    assert!(
        !out.exists(),
        "a read failure leaves no baseline: {}",
        out.display()
    );
}

/// §45 one level down, as a CLI contract: a `submit` run and a `build-only` run are refused
/// together, with both modes named so the caller can see what to split.
#[test]
fn two_execution_modes_are_two_baselines_and_the_refusal_writes_nothing() {
    let out = fresh("baseline-mixed-modes");
    let output = baseline(&[
        "--evidence-dir",
        &run_dir(SETTLED),
        "--evidence-dir",
        &run_dir(BUILD_ONLY),
        "--output",
        &out.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("submit") && text.contains("build-only"),
        "{text}"
    );
    assert!(!out.exists(), "the refusal came first: {}", out.display());

    // The build-only run on its own is fine, into its own directory.
    let alone = fresh("baseline-build-only-alone");
    let second = baseline(&[
        "--evidence-dir",
        &run_dir(BUILD_ONLY),
        "--output",
        &alone.display().to_string(),
    ]);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert!(
        stdout(&second).contains("build-only"),
        "{}",
        stdout(&second)
    );
}

/// §44's 「不覆盖历史运行」 at the command line: the second invocation that would add to a
/// baseline already on disk is refused, and the first baseline's bytes survive untouched.
#[test]
fn a_second_baseline_into_one_directory_is_refused_with_the_file_intact() {
    let out = fresh("baseline-append");
    let first = baseline(&[
        "--evidence-dir",
        &run_dir(SETTLED),
        "--output",
        &out.display().to_string(),
    ]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let before = std::fs::read(out.join("traces.jsonl")).expect("the first file");

    let second = baseline(&[
        "--evidence-dir",
        &run_dir(GATE_REFUSED),
        "--output",
        &out.display().to_string(),
    ]);
    assert_eq!(second.status.code(), Some(1), "{}", stdout(&second));
    let text = stderr(&second);
    assert!(text.contains("already holds"), "{text}");
    assert_eq!(
        before,
        std::fs::read(out.join("traces.jsonl")).expect("still there")
    );
    assert_eq!(lines(&out.join("traces.jsonl")).len(), 1);
}

/// The default output directory is §25's home, shown in the help text where a caller will
/// read it before typing. Checked through `--help` rather than by running the command, since
/// the real default is where the milestone's own evidence goes.
#[test]
fn the_help_names_the_default_directory_and_no_endpoint() {
    let help = stdout(&baseline(&["--help"]));
    assert!(
        help.contains("data/evidence/m8/latency/m7-history"),
        "the default has to be findable in the help: {help}"
    );
    assert!(
        !help.contains("--rpc-url") && !help.contains("GIWA"),
        "a command that reads files must not advertise an endpoint or a key: {help}"
    );
    assert!(
        !help.contains("execution-mode"),
        "the baseline never runs a lifecycle, so it has no mode to choose: {help}"
    );
}
