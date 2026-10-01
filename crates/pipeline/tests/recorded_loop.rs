//! §2 and §63's acceptance, run through the same binary path the CLI uses.
//!
//! The input is a recording of real GIWA Testnet blocks — the 50 around
//! 37,191,169, the one block in the last ~380k where both attested pools of the
//! WETH/TTAX pair restated their reserves in the same block. Nothing here is a
//! fixture in the sense of invented market data: every block, log and reserve
//! came off the chain, and the state a simulation reads is a recording of the
//! node's own answers at that height (`fixtures/simulation-m4/dump-37191169.json`,
//! written by M4 while reading the live RPC). §63 is explicit that this is the
//! legitimate way to accept the loop when a live session happens to find nothing,
//! and just as explicit that the run has to say `replay` while it does.
//!
//! What each test therefore pins is a *stage boundary*: a finding has to be
//! priced at a real block, carry that block's pin into the simulation, and come
//! back through the risk layer with a reason attached. Whether the answer is
//! money is not the point of this file — §76 says a Reject on a chain with no
//! arb is the correct output, and this chain's own answer was a Reject.

use std::path::{Path, PathBuf};

use alloy_primitives::{address, Address};
use serde_json::Value;

use evm_pipeline::{run, PipelineConfig, SessionReport};

const WRAPPED_NATIVE: Address = address!("0x4200000000000000000000000000000000000006");
/// The block the two attested pools of one pair moved together.
const ARB_BLOCK: u64 = 37_191_169;
/// The hash that block's own header carries, from the recording. A simulation
/// that claims to have run at this height has to name this hash (§19, §20).
const ARB_HASH: &str = "0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d670";

/// The same 32 bytes with the last one changed — a hash that is well-formed and
/// wrong. A refusal that turns on this difference is the §19/§20 check working; a
/// run that accepted it would be simulating a finding against a block that never
/// sealed it.
const TAMPERED_HASH: &str = "0x56c628d7102131d35d5bdf6a0ed2c6ec3d4d00edc4419a20641c7b396c48d671";

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
    // Both milestones' attestations, because the pair this acceptance runs on was
    // attested in M3 and the run has to be able to name every pool it saw.
    vec![
        workspace_root().join("data/protocols"),
        workspace_root().join("data/protocols-m3"),
    ]
}

/// One run per test, under a directory of its own.
///
/// Each session writes inside its own subdirectory (the runner names it after the
/// session), so two runs of one test never share a file. The parent is emptied
/// first so a subdirectory left by an earlier build cannot be read as if this run
/// had written it.
async fn replay(name: &str, with_state_recording: bool) -> SessionReport {
    replay_with_dump(name, with_state_recording.then(state_recording)).await
}

/// The same run, with the *state* recording chosen per test rather than per flag.
///
/// This exists so a test can hand the pipeline a dump that is not what the chain
/// says it is. The path is written before the run and lives outside the session's
/// own directory, which the call below wipes.
async fn replay_with_dump(name: &str, dump: Option<PathBuf>) -> SessionReport {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = PipelineConfig::replay(registries(), dir.clone(), corpus());
    config.wrapped_native = Some(WRAPPED_NATIVE);
    config.state_dump = dump;
    // The recording ends, and the run is expected to end with it; this is only a
    // guard so a regression that ignores the end cannot hang a test.
    config.duration = std::time::Duration::from_secs(60);
    run(&config)
        .await
        .unwrap_or_else(|error| panic!("{name}: the run failed: {error}"))
}

/// A copy of the real recording with one field changed: the block hash.
///
/// Nothing else moves — same accounts, same storage, same header numbers. That is
/// the point. A dump whose state is perfectly plausible but whose identity is not
/// the block the finding was priced at is exactly what §19/§20 forbid a run from
/// simulating against, and the only way to test that without inventing market data
/// is to tamper with a recording that came off the chain.
fn tampered_dump(name: &str) -> PathBuf {
    let path = workspace_root()
        .join("target/pipeline-tests")
        .join(format!("{name}-dump.json"));
    let text = std::fs::read_to_string(state_recording()).expect("the recording M4 wrote");
    let tampered = text.replace(ARB_HASH, TAMPERED_HASH);
    assert_ne!(
        tampered, text,
        "the recording does not carry the hash the test claims it does — \
         the tampering would have been a no-op"
    );
    std::fs::create_dir_all(path.parent().expect("a parent directory")).unwrap();
    std::fs::write(&path, tampered).expect("writes the tampered recording");
    path
}

fn lines(dir: &Path, file: &str) -> Vec<Value> {
    let path = dir.join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("an evidence line is JSON"))
        .collect()
}

/// Strip what a second run of the same input cannot reproduce: which session wrote
/// the line, how long each hop took, and which worker thread answered. Everything
/// else has to be byte-identical — that is the difference between "the numbers
/// matched" and "the run is reproducible".
///
/// `worker` is the one field here whose value the run does not decide: two workers
/// read from one queue, and which of them wakes is the OS's. The fields that carry
/// the answer — `fingerprint`, `gas_used`, `status`, `pin`, `state_version`, the
/// risk decision — are all compared, so dropping the thread id costs the test
/// nothing it was actually claiming.
fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| {
                key != "session_id" && key != "ms" && key != "worker" && !key.ends_with("_ms")
            });
            for (_, value) in map.iter_mut() {
                normalize(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(normalize),
        _ => {}
    }
}

fn normalized_lines(dir: &Path, file: &str) -> Vec<String> {
    lines(dir, file)
        .into_iter()
        .map(|mut line| {
            normalize(&mut line);
            // This workspace's `serde_json::Map` is a BTreeMap (the `preserve_order`
            // feature is off), so one line has exactly one string form.
            line.to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_recorded_market_move_walks_state_graph_opportunity_simulation_and_risk() {
    let report = replay("full-loop", true).await;
    let dir = &report.evidence_dir;

    assert_eq!(report.blocks, 50, "the whole recording was ingested");
    assert_eq!(
        report.ended_by, "every source ended",
        "a recording has an end, and the run stopped at it rather than at a timeout"
    );

    // §48: the session record says which of the two state sources this was, so a
    // reader never has to guess whether a number came off the chain or off a file.
    let session: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("live-session.json")).unwrap())
            .unwrap();
    assert_eq!(session["source"], "replay");
    assert_eq!(session["chain_id"], 91342);
    assert_eq!(session["end_block"], ARB_BLOCK + 30);
    let state_source = session["state_source"].as_str().expect("a string");
    assert!(
        state_source.starts_with("recorded:") && state_source.ends_with("@37191169"),
        "the record names the recording state came from, got {state_source}"
    );

    // Stage 1→3: the market moved, and the move was priced into exactly one pair
    // of findings on the two attested pools.
    let opportunities = normalized_lines(dir, "opportunities.jsonl");
    assert_eq!(
        opportunities.len(),
        2,
        "both directions of the one pair that moved together"
    );
    for line in &opportunities {
        assert!(
            line.contains(&format!("\"observed_block\":{ARB_BLOCK}")),
            "{line}"
        );
        assert!(line.contains("0x5bef6275"), "{line}");
        assert!(line.contains("0xf487d533"), "{line}");
    }

    // Stage 3→4: the finding that this run may fund was simulated, against the
    // block it was priced at and no other.
    let simulations = lines(dir, "simulation-results.jsonl");
    assert_eq!(simulations.len(), 1);
    assert_eq!(report.simulations, 1);
    let run = &simulations[0];
    assert_eq!(run["observed_block"], ARB_BLOCK);
    assert_eq!(run["state_version"]["block_number"], ARB_BLOCK);
    assert_eq!(
        run["pin"],
        format!("{ARB_BLOCK}:{ARB_HASH}"),
        "the simulation read the state of the finding's own block, by hash"
    );
    assert_eq!(
        run["state_source"],
        format!("{}@{ARB_BLOCK}", state_recording().display(),),
        "the record repeats the path this run was given, so a reader can open the \
         same file the numbers came from"
    );
    // The chain's own answer: the route that spends wrapped native executes up to
    // the second hop and reverts there. That is an answer, not a failure (§34's
    // rule from M4), and it is what the recording of this block supports.
    assert_eq!(run["gas_used"], 340_703);
    assert!(
        run["status"]
            .as_str()
            .is_some_and(|s| s.starts_with("Reverted")),
        "status was {:?}",
        run["status"]
    );

    // Stage 4→5: the risk layer judged that answer, and said why.
    let decisions = lines(dir, "risk-decisions.jsonl");
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0]["decision"]["Reject"]["rule"],
        "SimulationSuccess"
    );
    assert_eq!(decisions[0]["observed_block"], ARB_BLOCK);
    assert!(decisions[0]["no_broadcast"]
        .as_str()
        .expect("every decision says what it did not do")
        .contains("no broadcast"));
    assert_eq!(report.accepts, 0);
    assert_eq!(report.rejects, 1);

    // And the finding this run could not fund is a counted decline with a reason,
    // not an absence (§51, §57).
    let declines = lines(dir, "declines.jsonl");
    assert_eq!(declines.len(), 1);
    assert_eq!(declines[0]["rule"], "funding_unavailable");
    assert_eq!(declines[0]["observed_block"], ARB_BLOCK);
}

#[tokio::test]
async fn a_replay_with_no_state_recording_declines_instead_of_stopping() {
    // §53's separation, tested at the boundary a recorded corpus sits on: the
    // blocks are real and the market path runs on them, but a recording of block
    // bodies is not a recording of state. The finding has to end up as a decline
    // naming that fact, and the session still has to complete.
    let report = replay("no-state-recording", false).await;
    assert_eq!(report.blocks, 50);

    let declines = lines(&report.evidence_dir, "declines.jsonl");
    assert_eq!(
        declines.len(),
        2,
        "both directions of the pair asked for state this run does not have"
    );
    for decline in &declines {
        assert_eq!(decline["rule"], "state_unavailable");
        assert_eq!(decline["observed_block"], ARB_BLOCK);
        let reason = decline["reason"].as_str().expect("a reason");
        assert!(
            reason.contains(&format!("block {ARB_BLOCK}")),
            "the decline names the height it could not serve: {reason}"
        );
        assert!(
            reason.contains("no execution header"),
            "and the provider's own words for why: {reason}"
        );
    }
    assert_eq!(
        report.simulations, 0,
        "a decline is not a simulation that went missing"
    );

    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(report.evidence_dir.join("live-session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        session["state_source"], "chain: the source this run reads its blocks from",
        "with no recording configured the run says it would have asked the same chain it \
         read blocks from — which, for a replay, is the directory"
    );
}

#[tokio::test]
async fn the_same_recording_walked_twice_produces_the_same_artifacts() {
    // §33/§68's determinism claim, checked at the artifact layer rather than in
    // memory: two runs of one input have to write the same bytes, minus the
    // session id and the timings no two runs can share.
    let first = replay("determinism-a", true).await;
    let second = replay("determinism-b", true).await;
    for file in [
        "blocks.jsonl",
        "state-updates.jsonl",
        "opportunities.jsonl",
        "simulation-results.jsonl",
        "risk-decisions.jsonl",
        "declines.jsonl",
    ] {
        let a = normalized_lines(&first.evidence_dir, file);
        let b = normalized_lines(&second.evidence_dir, file);
        assert_eq!(a.len(), b.len(), "{file}: a different number of lines");
        for (index, (a, b)) in a.iter().zip(&b).enumerate() {
            assert_eq!(a, b, "{file} line {index} differs between two runs");
        }
        assert!(!a.is_empty() || file == "blocks.jsonl", "{file} was empty");
    }

    // The counters, which is the part a reader compares first.
    let metrics_a: Value = serde_json::from_str(
        &std::fs::read_to_string(first.evidence_dir.join("metrics.json")).unwrap(),
    )
    .unwrap();
    let metrics_b: Value = serde_json::from_str(
        &std::fs::read_to_string(second.evidence_dir.join("metrics.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(metrics_a["counters"], metrics_b["counters"]);
}

#[tokio::test]
async fn a_state_recording_that_is_not_the_block_a_finding_was_priced_at_is_refused() {
    // §19 and §20 are a claim about identity, not about availability: reading the
    // finding's own height is worthless if what answers is a different block. So
    // the input here is the real recording with exactly one byte of its block hash
    // changed — every account, every storage slot, the base fee and the gas limit
    // all still the chain's own numbers. A pipeline that simulated this would
    // produce a plausible-looking gas figure about a block that never existed,
    // which is the failure mode §20's ban on "latest" is really guarding against.
    let dump = tampered_dump("pin-mismatch");
    let report = replay_with_dump("pin-mismatch", Some(dump.clone())).await;
    let dir = &report.evidence_dir;

    // The market path is untouched: the tampering is at the state boundary, and
    // pretending the blocks never arrived would hide the very finding this test is
    // about. Detection and refusal are two different stages, and both are asserted.
    assert_eq!(report.blocks, 50);
    assert_eq!(
        normalized_lines(dir, "opportunities.jsonl").len(),
        2,
        "both directions of the pair are still recorded as findings (§51)"
    );

    assert_eq!(
        report.simulations, 0,
        "not one job ran against state it could not identify"
    );
    assert!(
        lines(dir, "simulation-results.jsonl").is_empty(),
        "and none is missing from the record — there was nothing to record"
    );
    assert_eq!(report.accepts, 0);
    assert_eq!(report.rejects, 0);
    assert!(
        lines(dir, "risk-decisions.jsonl").is_empty(),
        "a finding that never got its own state never reaches the risk layer (§25)"
    );

    let declines = lines(dir, "declines.jsonl");
    assert_eq!(
        declines.len(),
        2,
        "the refusal is a counted decline per finding, not a silence (§51)"
    );
    for decline in &declines {
        assert_eq!(decline["rule"], "state_unavailable");
        assert_eq!(decline["observed_block"], ARB_BLOCK);
        let reason = decline["reason"].as_str().expect("a reason");
        assert!(
            reason.contains(ARB_HASH),
            "the decline names the hash the finding was sealed with: {reason}"
        );
        assert!(
            reason.contains(TAMPERED_HASH),
            "and the one the state source answered with, so a reader can see which side \
             of the comparison was wrong: {reason}"
        );
    }

    // The session record still names the file state came from — the tampered copy,
    // not the pristine fixture — because §48's purpose is that a reader can open
    // exactly what the numbers came from.
    assert!(
        report.session["state_source"]
            .as_str()
            .is_some_and(|s| s.contains(&dump.display().to_string())),
        "state_source was {:?}",
        report.session["state_source"]
    );
}
