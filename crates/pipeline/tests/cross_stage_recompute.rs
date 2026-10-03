//! §29's evidence group, first half: every published figure is a fold over raw rows.
//!
//! M8.4.1's committed `pipeline-calls.json` is read here as the raw material it is — its
//! `rows` are the records the recorder wrote — and M8.4.2's four tables are computed from
//! them in this process. The figures asserted below were computed a second time, by an
//! independent script, from those same rows. Two derivations of one number are the point: a
//! table that agreed only with itself would prove nothing.
//!
//! The last test works on M8.4.1's other committed corpus, its three fixed-block arms, and
//! asks the §12 question of them: replay one block and the relations have to come out the
//! same each time. That corpus is where the honest answer is interesting rather than
//! flattering — it holds one sink, so it can only reproduce a zero — and the test therefore
//! carries the positive control beside it: the same fold, in the same process, finds 69 pairs
//! across the live rows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use evm_pipeline::canonicalization::{
    cross_stage_run_table, cross_stage_tables, DUPLICATE_EXACT, DUPLICATE_MATRIX_FILE,
    DUPLICATE_SUMMARY_FILE, PAIR_MEASURED, PAIR_NO_ASKS_OBSERVED, PAIR_NO_DUPLICATES,
    PAIR_STAGE_ABSENT, REUSE_CANDIDATES_FILE, SAME_TARGET_BLOCK_UNDETERMINED,
    SAME_TARGET_DIFFERENT_BLOCK, SCOPE_CROSS_STAGE, SCOPE_INTRA_STAGE, STAGE_PAIRS_FILE,
};
use serde_json::{json, Value};

const SOURCE_EVIDENCE: &str = "data/evidence/m8/storage-dependency/pipeline-calls.json";

/// One run record: M8.4.1's own provenance fields plus the rows that carry its run name.
fn run_records(aggregate: &Value) -> Vec<Value> {
    let mut by_run: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for row in aggregate["rows"].as_array().expect("rows") {
        let run = row["run"].as_str().expect("row run").to_string();
        by_run.entry(run).or_default().push(row.clone());
    }
    aggregate["assembled_from"]
        .as_array()
        .expect("assembled_from")
        .iter()
        .map(|meta| {
            let run = meta["run"].as_str().expect("meta run").to_string();
            let calls = by_run.remove(&run).unwrap_or_default();
            json!({
                "run": run,
                "source": meta["source"],
                "chain_id": meta["chain_id"],
                "block_number": meta["block_number"],
                "endpoint_id": meta["endpoint_id"],
                "execution_mode": meta["execution_mode"],
                "git_revision": meta["git_revision"],
                "generated_at_unix_ms": meta["generated_at_unix_ms"],
                "calls": calls,
            })
        })
        .collect()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_aggregate() -> Value {
    let path = workspace_root().join(SOURCE_EVIDENCE);
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn cell(table: &Value, key: &str) -> usize {
    table[key]
        .as_u64()
        .unwrap_or_else(|| panic!("no figure {key} in {table}")) as usize
}

/// The control's GRAND tally and §6's three levels, over the three runs M8.4.1 committed.
#[test]
fn cross_stage_tables_recompute_from_m8_4_1_rows() {
    let aggregate = read_aggregate();
    let runs = run_records(&aggregate);
    assert_eq!(runs.len(), 3, "M8.4.1 committed three runs");
    assert_eq!(
        runs.iter()
            .map(|run| run["calls"].as_array().map_or(0, Vec::len))
            .sum::<usize>(),
        235,
        "every raw row reaches exactly one run record"
    );

    let tables = cross_stage_tables(&runs);
    let summary = &tables[DUPLICATE_SUMMARY_FILE];

    // §4's classes: 34 exact, 29 same-target-different-block, 6 undetermined, none semantic.
    assert_eq!(cell(summary, "exact_duplicate_pairs"), 34);
    assert_eq!(cell(summary, "same_target_different_block_pairs"), 29);
    assert_eq!(cell(summary, "same_target_block_undetermined_pairs"), 6);
    assert_eq!(
        cell(summary, "same_method_different_semantics_pairs"),
        0,
        "no pair in this corpus is one method asked two different questions"
    );
    assert_eq!(cell(summary, "semantic_duplicate_pairs"), 0);

    // §17's separation, and §25's: a retry is not a pair, so no pair is intra-request.
    assert_eq!(cell(summary, "duplicate_pairs"), 69);
    assert_eq!(cell(summary, "cross_stage_pairs"), 63);
    assert_eq!(cell(summary, "intra_stage_pairs"), 6);
    assert_eq!(cell(summary, "same_logical_request_pairs"), 0);

    // §6's three levels: 34 candidates, none of them safe, 29 ended by the block rule.
    let candidates = &tables[REUSE_CANDIDATES_FILE];
    assert_eq!(cell(candidates, "pairs"), 69);
    assert_eq!(cell(candidates, "candidates"), 34);
    assert_eq!(cell(candidates, "safe_to_reuse"), 0);
    assert_eq!(cell(candidates, "refused_by_block_identity"), 29);
    assert_eq!(cell(summary, "reuse_candidates"), 34);
    assert_eq!(cell(summary, "unknown_reuse_candidates"), 34);
    assert_eq!(cell(summary, "safe_reuse_candidates"), 0);
    assert_eq!(cell(summary, "refused_by_block_identity"), 29);

    // §10's per-run figures, so a pooled number can be traced to the run that holds it.
    let per_run = summary["per_run"].as_array().expect("per_run");
    assert_eq!(per_run.len(), 3);
    let asks = per_run
        .iter()
        .map(|run| run["asks"].as_u64().expect("asks"))
        .collect::<Vec<u64>>();
    assert_eq!(asks, vec![71, 82, 82], "the runs' own row counts");

    // One (run, class, scope) cell per the control's per-run split, read back by name.
    let mut cells: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for run in per_run {
        let name = run["run"].as_str().expect("run name").to_string();
        for pair_cell in run["pairs_by_class_and_scope"]
            .as_array()
            .expect("pairs_by_class_and_scope")
        {
            cells.insert(
                (
                    name.clone(),
                    pair_cell["duplicate_type"]
                        .as_str()
                        .expect("class")
                        .to_string(),
                    pair_cell["scope"].as_str().expect("scope").to_string(),
                ),
                pair_cell["pairs"].as_u64().expect("pairs") as usize,
            );
        }
    }
    let named_runs: Vec<String> = per_run
        .iter()
        .map(|run| run["run"].as_str().expect("run name").to_string())
        .collect();
    let expected = [
        (DUPLICATE_EXACT, SCOPE_CROSS_STAGE, vec![4, 12, 12]),
        (DUPLICATE_EXACT, SCOPE_INTRA_STAGE, vec![2, 2, 2]),
        (
            SAME_TARGET_DIFFERENT_BLOCK,
            SCOPE_CROSS_STAGE,
            vec![9, 10, 10],
        ),
        (
            SAME_TARGET_BLOCK_UNDETERMINED,
            SCOPE_CROSS_STAGE,
            vec![2, 2, 2],
        ),
    ];
    for (class, scope, counts) in expected {
        let seen: Vec<usize> = named_runs
            .iter()
            .map(|run| {
                *cells
                    .get(&(run.clone(), class.to_string(), scope.to_string()))
                    .unwrap_or(&0)
            })
            .collect();
        assert_eq!(seen, counts, "{class} | {scope} per run");
    }
}

/// The matrix and the §13 rows fold the same pairs the summary counts, so their totals meet.
#[test]
fn every_table_agrees_with_the_others() {
    let aggregate = read_aggregate();
    let tables = cross_stage_tables(&run_records(&aggregate));

    let matrix = &tables[DUPLICATE_MATRIX_FILE];
    assert_eq!(matrix["totals"]["pairs"].as_u64().expect("pairs"), 69);
    let matrix_rows: usize = matrix["rows"]
        .as_array()
        .expect("matrix rows")
        .iter()
        .map(|row| row["pairs"].as_u64().expect("cell pairs") as usize)
        .sum();
    assert_eq!(matrix_rows, 69, "the matrix cells sum to the pair count");

    let candidates = tables[REUSE_CANDIDATES_FILE]["rows"]
        .as_array()
        .expect("candidate rows")
        .len();
    assert_eq!(candidates, 69);

    let pairs_file = &tables[STAGE_PAIRS_FILE];
    let named = pairs_file["rows"].as_array().expect("named pair rows");
    assert_eq!(
        named.len(),
        11,
        "§13 names eleven pairs and all eleven are rows"
    );
}

/// The fold adds nothing: every pair side names a row that was actually asked, by run and
/// `rpc_id`, and no consumer appears as a pair twice.
#[test]
fn tables_hold_no_row_that_was_not_asked() {
    let aggregate = read_aggregate();
    let asked: BTreeSet<(String, u64)> = aggregate["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            (
                row["run"].as_str().unwrap_or_default().to_string(),
                row["rpc_id"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    let tables = cross_stage_tables(&run_records(&aggregate));
    let rows = tables[REUSE_CANDIDATES_FILE]["rows"]
        .as_array()
        .expect("candidate rows");
    let mut seen: BTreeSet<(String, u64)> = BTreeSet::new();
    for row in rows {
        let run = row["run"].as_str().unwrap_or_default().to_string();
        for side in ["producer", "consumer"] {
            let id = (
                run.clone(),
                row[side]["rpc_id"].as_u64().unwrap_or_default(),
            );
            assert!(
                asked.contains(&id),
                "{side} names a row that was not asked: {id:?}"
            );
        }
        let consumer = (run, row["consumer"]["rpc_id"].as_u64().unwrap_or_default());
        assert!(
            seen.insert(consumer.clone()),
            "one consumer cannot be a pair twice: {consumer:?}"
        );
    }
}

/// M8.4.1's §12 arm: one block, replayed, and classified again from scratch on each replay.
///
/// The input is the committed `fixed-block/` corpus — three arms of one route against one state
/// snapshot at block 37,191,169, served by the stub that holds the M4 dump. Nothing here is
/// re-run against a node: §12 asks whether the relations found at a block reproduce, and the
/// answer has to be a fact about the corpus, so this test reads raw rows and folds them.
///
/// What the corpus says is reported as it is, including the shape fact that makes the
/// interesting number zero: every arm has one sink, so a cross-stage pair cannot exist in it
/// (§13's rule is `not_applicable`, not a fabricated second stage).
#[test]
fn the_fixed_block_arms_classify_the_same_way_run_after_run() {
    const BLOCK: &str = "37191169";
    let arms: Vec<Value> = ["run-01", "run-02", "run-03"]
        .iter()
        .map(|name| fixed_block_arm(name))
        .collect();

    // §12's four controls, checked before any number: chain id, block, route, and the state
    // snapshot. The first two are literal. The third is read as the sequence of asks, the only
    // spelling of "the same route" this corpus carries. The fourth is the one control this
    // corpus evidences by source rather than by value, and the test says so rather than
    // looking the other way: each arm is served by its own stub, bound to an ephemeral port,
    // so its three endpoint digests differ while every answer behind them comes from the one
    // recording file.
    for arm in &arms {
        assert_eq!(arm["chain_id"], 91342, "§12: chain_id identical");
        assert_eq!(arm["block_number"], 37_191_169, "§12: block identical");
        assert_eq!(
            arm["source"], "fixture",
            "§12: the state snapshot is a recording, so no wall clock can move it between arms"
        );
        assert!(
            arm["endpoint_id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("rpc-"),
            "the digest of the endpoint this arm read from"
        );
        assert_eq!(arm["execution_mode"], "build-only");
    }
    let endpoints: BTreeSet<String> = arms
        .iter()
        .map(|arm| arm["endpoint_id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        endpoints.len(),
        3,
        "one stub per arm: three ports, three digests, one recording behind them"
    );
    assert!(
        !endpoints.contains("rpc-faa716cada04a9ef"),
        "the live endpoint's digest appeared in a fixed-block arm: {endpoints:?}"
    );
    assert!(
        arms.windows(2)
            .all(|pair| pair[0]["git_revision"] == pair[1]["git_revision"]),
        "one build replayed three times, not three builds"
    );
    let sequences: Vec<Vec<String>> = arms
        .iter()
        .map(|arm| {
            arm["calls"]
                .as_array()
                .expect("calls")
                .iter()
                .map(|row| {
                    // A call either names a height or names no block at all (`eth_chainId`
                    // is the one in this corpus); a third answer would not be a fixed block.
                    let tag = row["block_tag"].as_str().unwrap_or("");
                    assert!(
                        tag.is_empty() || tag == BLOCK,
                        "a fixed-block arm read {tag}: {row}"
                    );
                    format!(
                        "{}|{}|{}|{}",
                        row["method"].as_str().unwrap_or_default(),
                        row["target"].as_str().unwrap_or_default(),
                        row["slot"].as_str().unwrap_or_default(),
                        tag,
                    )
                })
                .collect()
        })
        .collect();
    assert!(
        sequences.windows(2).all(|pair| pair[0] == pair[1]),
        "§12: route identical — the three arms asked a different sequence of questions"
    );
    let asks = sequences[0].len();
    assert_eq!(asks, 41, "M8.4.1 committed 41 asks per fixed-block arm");

    // The relations themselves: same tables, arm after arm. `UNREPRODUCIBLE` below lists what a
    // replay cannot reproduce and why each is a fact about the process rather than about the
    // reads, along with the run's own name, which appears in every `logical_request_id`.
    // Everything else is compared, including the identity strings, both sides' `rpc_id`s, the
    // class, scope, block relation and verdict of every pair.
    let digests: Vec<String> = arms
        .iter()
        .zip(["run-01", "run-02", "run-03"])
        .map(|(arm, name)| {
            let tables = cross_stage_tables(std::slice::from_ref(arm));
            let mut text = String::new();
            for file in [
                DUPLICATE_MATRIX_FILE,
                DUPLICATE_SUMMARY_FILE,
                REUSE_CANDIDATES_FILE,
                STAGE_PAIRS_FILE,
            ] {
                text.push_str(
                    &serde_json::to_string(&stripped(tables[file].clone())).expect("a table"),
                );
            }
            text.push_str(
                &serde_json::to_string(&stripped(cross_stage_run_table(arm))).expect("a run table"),
            );
            text.replace(name, "arm")
        })
        .collect();
    let first_difference = || -> String {
        let (a, b) = (&digests[0], &digests[1]);
        let offset = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
        let window = |text: &str| {
            let tail: String = text.chars().skip(offset).take(160).collect();
            format!("…{tail}")
        };
        format!(
            "the two arms' tables first differ at byte {offset}:\n  run-01 {}\n  run-02 {}",
            window(a),
            window(b)
        )
    };
    assert!(
        digests.windows(2).all(|pair| pair[0] == pair[1]),
        "§12: the duplicate/reuse relations are not reproducible across the three replays.\n{}",
        first_difference()
    );
    assert!(
        digests[0].contains(r#""asks":41"#),
        "the four tables that just matched do not even carry the arm's own ask count, so the \
         compare above was against something smaller than a table"
    );

    // The figures one replay yields, read out of its own table.
    let arm = &arms[0];
    let tables = cross_stage_tables(std::slice::from_ref(arm));
    let per_run = cross_stage_run_table(arm);
    assert_eq!(per_run["asks"].as_u64().expect("asks"), asks as u64);
    assert_eq!(per_run["pairs"].as_u64().expect("pairs"), 0);
    assert_eq!(
        tables[REUSE_CANDIDATES_FILE]["pairs"]
            .as_u64()
            .expect("pairs"),
        0
    );
    assert_eq!(
        tables[REUSE_CANDIDATES_FILE]["candidates"]
            .as_u64()
            .expect("candidates"),
        0
    );
    assert_eq!(
        tables[REUSE_CANDIDATES_FILE]["safe_to_reuse"]
            .as_u64()
            .expect("safe_to_reuse"),
        0
    );

    // Zero is a claim about this corpus twice over, and both halves are checked here rather
    // than assumed: the single sink, which rules out a cross-stage pair by shape, and
    // M8.4.1's own duplicate count, which rules out an intra-stage one by measurement.
    let sinks: BTreeSet<String> = arm["calls"]
        .as_array()
        .expect("calls")
        .iter()
        .map(|row| row["sink"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        sinks,
        BTreeSet::from(["simulation".to_string()]),
        "the fold found cross-stage pairs in a one-sink corpus, which is the bug §13 forbids"
    );
    for name in ["run-01", "run-02", "run-03"] {
        let path = fixed_block_dir(name).join("duplicate-reads.json");
        let duplicates = read_json_at(&path);
        for source in duplicates["per_source"].as_array().expect("per_source") {
            assert_eq!(
                source["duplicate_state_reads"].as_u64(),
                Some(0),
                "{name}: M8.4.1 measured a duplicate this fold did not see"
            );
        }
    }

    // §13's rule on a corpus with no path: every named pair is a row, and none of them is
    // `measured`. An absent stage and a stage that asked nothing are different answers, so
    // this checks both spellings appear.
    let rows = tables[STAGE_PAIRS_FILE]["rows"]
        .as_array()
        .expect("named pair rows");
    assert_eq!(rows.len(), 11);
    let statuses: BTreeSet<String> = rows
        .iter()
        .map(|row| row["status"].as_str().unwrap_or_default().to_string())
        .collect();
    for row in rows {
        assert_ne!(
            row["status"],
            json!(PAIR_MEASURED),
            "{}: a fixed-block arm with one sink measured a cross-stage pair",
            row["pair"]
        );
        assert_ne!(
            row["status"],
            json!(PAIR_NO_DUPLICATES),
            "{}: `measured_no_duplicates` claims both sides asked something, and one side of \
             every named pair here asked nothing",
            row["pair"]
        );
    }
    assert!(
        statuses.contains(PAIR_NO_ASKS_OBSERVED),
        "statuses were {statuses:?}"
    );
    assert!(statuses.contains(PAIR_STAGE_ABSENT));

    // The control that keeps the whole test honest: the same fold over M8.4.1's live rows, in
    // the same process, is not empty. A fold that returned zero tables everywhere would pass
    // every assertion above.
    let live = cross_stage_tables(&run_records(&read_aggregate()));
    let live_summary = &live[DUPLICATE_SUMMARY_FILE];
    assert_eq!(cell(live_summary, "duplicate_pairs"), 69);
    assert!(
        cell(live_summary, "cross_stage_pairs") > 0,
        "the live corpus has no cross-stage pair either, so the block above proves nothing"
    );
}

/// One fixed-block arm as a run record: the raw rows, with the provenance its own summary
/// carries, in the shape `cross_stage_tables` reads.
fn fixed_block_arm(name: &str) -> Value {
    let dir = fixed_block_dir(name);
    let calls = read_json_at(&dir.join("pipeline-calls.json"));
    let summary = read_json_at(&dir.join("pipeline-summary.json"));
    let assembled = summary["assembled_from"]
        .as_array()
        .expect("assembled_from");
    assert_eq!(assembled.len(), 1, "one arm writes one run record");
    let meta = &assembled[0];
    json!({
        "run": name,
        "source": meta["source"],
        "chain_id": meta["chain_id"],
        "block_number": meta["block_number"],
        "endpoint_id": meta["endpoint_id"],
        "execution_mode": meta["execution_mode"],
        "git_revision": meta["git_revision"],
        "generated_at_unix_ms": meta["generated_at_unix_ms"],
        "calls": calls["rows"],
    })
}

fn fixed_block_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("data/evidence/m8/storage-dependency/fixed-block")
        .join(name)
}

/// The fields of these tables a replay of the same input cannot reproduce, because each is a
/// fact about the process that ran rather than about the reads: the three monotonic timings,
/// measured from an origin that restarts with the run (§7 of M8.4.1's rules, inherited here);
/// the wall-clock stamp of the assembly that wrote the rows; and the endpoint digest, which
/// for a fixed-block arm is the ephemeral port its own stub bound.
///
/// None of the four carries a relation. The §6 condition that reads the endpoint — same
/// endpoint, so one state semantics — survives as its outcome, and the digests themselves are
/// asserted above rather than here, where a string compare across arms would only measure
/// which port the OS handed out.
const UNREPRODUCIBLE: [&str; 5] = [
    "started_ns",
    "finished_ns",
    "duration_ns",
    "generated_at_unix_ms",
    "endpoint_id",
];

/// A table with those fields removed at every depth, so two arms' tables can be compared as
/// strings without the clock setting the answer.
fn stripped(value: Value) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.retain(|key, _| !UNREPRODUCIBLE.contains(&key.as_str()));
                for (_, child) in map.iter_mut() {
                    strip(child);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value = value;
    strip(&mut value);
    value
}

fn read_json_at(path: &std::path::Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}
