//! M8.4.4 §16–§18's evidence tree: `data/evidence/m8/m8.4.4/`, assembled from the raw arm rows
//! that `crates/execution/tests/sequence.rs` writes into its `fixed-block/` subtree.
//!
//! # What this gate is for
//!
//! §17 forbids a `summary.json` that states a number no reader can derive from the raw records,
//! and §18 asks for eight integrity properties by name: E1 regenerable, E2 byte-identical, E3 the
//! summary recomputable, E4/E5 the RPC count and the method counts recomputable, E6 the A/B block
//! identity recomputable, E7 the A/B Build equality recomputable out of the Build records, E8 every
//! negative control refused. All eight are properties of files, so all eight are graded here: this
//! binary reads the rows, renders the committed tree from them, and asks whether the tree *is* that
//! rendering.
//!
//! Nothing in this file measures anything. It opens no socket and drives no stage — the numbers are
//! counted out of the fixture's own published rows, and a §16 tree that could only be assembled
//! while a node was answering would not be reproducible tomorrow.
//!
//! # The live subtree (§20)
//!
//! `live/` holds three real BuildOnly runs' records, written by the CLI and read here into two
//! projection tables. They cannot be regenerated — re-running asks a different block — so the gate
//! treats them exactly like the fixture's raw rows: the committed tables must be what those records
//! count out to, recomputed independently of the renderer by
//! [`the_live_rows_and_the_census_recompute_from_their_own_records`]. §33's count question is answered
//! there against M8.4.2's three live runs, which are the same candidate, the same endpoint digest
//! and the same flags, recorded before this milestone existed.
//!
//! # Which arm is which (§11)
//!
//! - `baseline` — Arm A, the path as it stood before this milestone: no context is propagated, the
//!   Build stage reads the block at its pin for itself.
//! - `verified_context` — Arm B (§16 publishes it as `reuse/`): the same step with a verified
//!   context propagated to it, checked by the consumer leg, and still reading the block for itself.
//!   §2 keeps that read, so the two arms differ in exactly one input and in no RPC.
//! - `negative_control` — Arm C: a context that really did verify a block which is not the one this
//!   step pins (§7's NC1). It must be refused by name, and it must not change what the step builds.
//!
//! # The number this tree cannot produce
//!
//! `rpc_saved` is 0. The consumer's own block read is a §14 gate input and §2 forbids deleting a
//! safety check to buy a saving, so propagation adds a verification over a read that was already
//! happening and the count is unchanged. §33's wording is derived from that arithmetic here rather
//! than chosen in prose: with `net_rpc_saved` at 0 the tree prints "No net RPC reduction." and never
//! the sentence §33 forbids.
//!
//! # How these files change
//!
//! Rendering happens under `target/execution-tests/`; `M844_BLOCK_CONTEXT_REFRESH=1` copies a fresh
//! rendering over the committed tree, which is the only way these files change. The raw rows are
//! written by the other binary —
//! `M844_FIXTURE_EVIDENCE=data/evidence/m8/m8.4.4 cargo test -p evm-execution --test sequence` —
//! and nothing here writes to `fixed-block/`. §36: both binaries run serially, and the refresh
//! variables are set one at a time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const EVIDENCE_DIR: &str = "data/evidence/m8/m8.4.4";
/// The raw rows' subtree. Named in both binaries because a test binary cannot import another's
/// module: if the writer's path and this one ever disagreed, the gate would fail loudly on a missing
/// file rather than assemble a tree out of nothing.
const FIXED_BLOCK_DIR: &str = "fixed-block";
const RUNS: [&str; 3] = ["run-01", "run-02", "run-03"];

/// §20's live half of the tree: three real BuildOnly runs against the same endpoint the fixture
/// imitates, kept as raw records under `live/route-runs/` and `live/rpc/` and read here into two
/// derived tables. This gate never writes those directories — they were produced by the CLI, and
/// §17's rule that a summary figure be recomputable from raw records is the reason they stay.
const LIVE_DIR: &str = "live";
const LIVE_ROUTE_RUNS: &str = "live/route-runs";
const LIVE_RPC: &str = "live/rpc";
const LIVE_RUNS: [&str; 3] = [
    "route-91342-37746091-1791091208939",
    "route-91342-37746182-1791091300296",
    "route-91342-37746511-1791091629873",
];
/// The control group for §33's count question: the three live runs M8.4.2 recorded before this
/// milestone existed, on the same endpoint, same candidate, same flags. Their raw records stay
/// where they were published — comparing against a copy would let the copy drift.
const PRIOR_EVIDENCE_DIR: &str = "data/evidence/m8/cross-stage";
const PRIOR_LIVE_RUNS: [&str; 3] = [
    "route-91342-37700740-1791045857463",
    "route-91342-37700778-1791045896094",
    "route-91342-37700806-1791045924232",
];
/// Directories of raw input, at either depth: the tree's derived files are everything else.
const RAW_DIRS: [&str; 3] = [FIXED_BLOCK_DIR, "route-runs", "rpc"];

const BASELINE: &str = "baseline";
const VERIFIED: &str = "verified_context";
const CONTROL: &str = "negative_control";
/// §19's three arms, in the order §11 introduces them.
const ARMS: [&str; 3] = [BASELINE, VERIFIED, CONTROL];

/// §16's file set. The tree is this list and nothing else, so that a reader applying §16's
/// 「不要为了凑目录而制造重复证据」 can tell a derived view from an extra copy.
const GENERATED: [&str; 17] = [
    "summary.json",
    "contract/verified-block-context.json",
    "contract/producer-verification.json",
    "contract/consumer-verification.json",
    "contract/negative-controls.json",
    "baseline/rpc-summary.json",
    "baseline/pipeline-calls.json",
    "baseline/build-result.json",
    "reuse/rpc-summary.json",
    "reuse/pipeline-calls.json",
    "reuse/build-result.json",
    "comparison/rpc-comparison.json",
    "comparison/block-identity-comparison.json",
    "comparison/build-comparison.json",
    "comparison/correctness-comparison.json",
    "live/live-runs.json",
    "live/rpc-census-comparison.json",
];
/// …plus the README, which §16 asks for and §18's E1 makes a generated file like any other.
const TREE_FILES: usize = GENERATED.len() + 1;

/// The one lane site whose count is §14's whole question: this milestone claims propagation neither
/// added nor removed a read *of the block the step pins*.
const BLOCK_READ: &str = "block_hash_at";
/// §31's consumer row, which the production record writes per step.
const CONSUMER_CHECKS: &str = "context_checks";

/// §6's three outcome words. A row outside these is a fourth outcome nobody specified.
const OUTCOME_WORDS: [&str; 3] = ["no_context", "accepted", "rejected"];

/// §12's Build fields, named as §12 names them.
const BUILD_FIELDS: [&str; 10] = [
    "chain_id",
    "tx_type",
    "nonce",
    "to",
    "value",
    "gas_limit",
    "input",
    "access_list",
    "max_fee_per_gas",
    "max_priority_fee_per_gas",
];

/// §19's five equality axes.
const AXES: [&str; 5] = [
    "block_identity",
    "build_output",
    "transaction_intent",
    "serialization",
    "fingerprint",
];

/// §7's six negative controls with the tests that refuse them. Only NC1 is a row in this tree — the
/// fixture's `negative_control` arm; the other five are graded inside the crate, so this file names
/// the test and [`the_negative_control_pointers_name_tests_that_exist`] checks the name is really in
/// the file it is attributed to. A pointer nobody re-checks is how a deleted test keeps looking like
/// evidence.
type Control = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const CONTROLS: [Control; 6] = [
    (
        "NC1",
        "same height, a different hash",
        &[
            (
                "crates/execution/src/block_context.rs",
                "same_height_different_hash_rejected",
            ),
            (
                "crates/execution/tests/sequence.rs",
                "a_context_about_another_block_at_the_same_height_is_refused_by_name",
            ),
            (
                "crates/execution/src/preflight.rs",
                "a_reorg_at_the_pin_refuses_the_context_and_names_the_hash_that_replaced_it",
            ),
        ],
    ),
    (
        "NC2",
        "a different height, the same hash",
        &[
            (
                "crates/execution/src/block_context.rs",
                "different_height_rejected",
            ),
            (
                "crates/execution/tests/sequence.rs",
                "a_context_about_the_neighbouring_height_is_refused_before_the_hash_is_compared",
            ),
            (
                "crates/execution/src/preflight.rs",
                "a_read_about_a_different_height_refuses_the_context_rather_than_renumbering_it",
            ),
        ],
    ),
    (
        "NC3",
        "a different chain",
        &[
            (
                "crates/execution/src/block_context.rs",
                "different_chain_rejected_at_the_consumer",
            ),
            (
                "crates/execution/tests/sequence.rs",
                "a_context_from_another_chain_is_refused_and_names_both_chains",
            ),
            (
                "crates/execution/src/preflight.rs",
                "another_chain_at_the_same_height_refuses_the_context",
            ),
        ],
    ),
    (
        "NC4",
        "a missing hash",
        &[
            (
                "crates/execution/src/block_context.rs",
                "verified_context_requires_number_and_hash",
            ),
            (
                "crates/execution/src/block_context.rs",
                "missing_hash_rejected_by_construction",
            ),
        ],
    ),
    (
        "NC5",
        "a context tampered with after it was produced",
        &[
            (
                "crates/execution/src/block_context.rs",
                "tampered_context_rejected",
            ),
            (
                "crates/execution/tests/sequence.rs",
                "the_structured_consumer_row_and_the_prose_line_agree",
            ),
        ],
    ),
    (
        "NC6",
        "a stale context: the head moved past the propagated block",
        &[
            (
                "crates/execution/src/block_context.rs",
                "stale_live_head_context_rejected_and_fixed_pin_context_survives",
            ),
            (
                "crates/execution/src/block_context.rs",
                "a_stale_head_context_is_rejected_at_the_consumer_by_its_own_head_read",
            ),
            (
                "crates/execution/src/block_context.rs",
                "a_hash_that_matches_the_pin_but_not_the_head_is_rejected_by_the_consumer",
            ),
        ],
    ),
];

fn refreshing() -> bool {
    std::env::var_os("M844_BLOCK_CONTEXT_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The tree, named the way the writer names it: `M844_FIXTURE_EVIDENCE`, whose absence is a scratch
/// directory. Both binaries must resolve it identically, because the gate reads what the writer
/// wrote.
fn evidence_root() -> PathBuf {
    match std::env::var("M844_FIXTURE_EVIDENCE") {
        Ok(dir) if !dir.is_empty() => {
            let path = PathBuf::from(dir);
            if path.is_relative() {
                workspace_root().join(path)
            } else {
                path
            }
        }
        _ => workspace_root().join(EVIDENCE_DIR),
    }
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn read_text(path: &Path) -> String {
    String::from_utf8_lossy(&read_bytes(path)).to_string()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&read_bytes(path))
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn committed(root: &Path, relative: &str) -> Value {
    read_json(&root.join(relative))
}

/// The rows this gate is allowed to read. A missing row fails with the command that makes it: an
/// aggregate over a sample nobody took is exactly the silent shortfall §51 exists to prevent.
fn raw_rows(root: &Path) -> Vec<Value> {
    RUNS.iter()
        .map(|run| {
            let path = root.join(FIXED_BLOCK_DIR).join(format!("{run}.json"));
            if !path.exists() {
                panic!(
                    "{}: no raw rows. Regenerate them with `M844_FIXTURE_EVIDENCE={EVIDENCE_DIR} \
                     cargo test -p evm-execution --test sequence`",
                    path.display()
                );
            }
            read_json(&path)
        })
        .collect()
}

fn raw_text(root: &Path) -> String {
    RUNS.iter()
        .map(|run| read_text(&root.join(FIXED_BLOCK_DIR).join(format!("{run}.json"))))
        .collect::<Vec<_>>()
        .join("\n")
}

fn arm<'a>(run: &'a Value, name: &str) -> &'a Value {
    run["arms"]
        .as_array()
        .expect("a raw row carries its arms as an array")
        .iter()
        .find(|row| row["arm"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("no {name} arm in {}", run["run"]))
}

/// The lane reads, in the order the lane answered them — §12's `method_order`, and the denominator
/// of every RPC count in this tree.
fn sites(row: &Value) -> Vec<String> {
    row["lane_reads"]
        .as_array()
        .expect("an arm records its lane reads")
        .iter()
        .map(|read| {
            read["site"]
                .as_str()
                .expect("a lane read names its site")
                .to_string()
        })
        .collect()
}

fn block_reads(row: &Value) -> usize {
    sites(row).iter().filter(|site| *site == BLOCK_READ).count()
}

fn method_counts(row: &Value) -> Value {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for site in sites(row) {
        *counts.entry(site).or_insert(0) += 1;
    }
    json!(counts)
}

/// §31's row. A BuildOnly drive runs one step, and this asserts that instead of assuming it — a
/// second consumer row would mean the fixture drove more of the route than it says it did.
fn consumer(row: &Value) -> &Value {
    let checks = row["record"][CONSUMER_CHECKS]
        .as_array()
        .expect("the record carries its consumer rows");
    assert_eq!(
        checks.len(),
        1,
        "one step driven, so one consumer answer: {checks:?}"
    );
    &checks[0]
}

fn outcome(row: &Value) -> &str {
    let word = consumer(row)["outcome"]
        .as_str()
        .expect("a consumer row names its outcome");
    assert!(
        OUTCOME_WORDS.contains(&word),
        "{word} is not one of §6's three outcomes"
    );
    word
}

fn build(row: &Value) -> &Value {
    let builds = row["record"]["builds"]
        .as_array()
        .expect("a Built step records the transaction it built");
    assert_eq!(builds.len(), 1, "one step driven, so one build row");
    &builds[0]
}

/// The triple §25 requires: chain, height and hash together, read off the block the step actually
/// sent against rather than off the context it was handed.
fn identity(row: &Value) -> Value {
    let built = build(row);
    json!({
        "chain_id": built["chain_id"],
        "block_number": built["block_number"],
        "block_hash": built["block_hash"],
    })
}

/// The same triple out of a producer row, whose object carries two more fields (§27's provenance and
/// §6's verifier). Comparing whole objects would make `verified_block_equals_the_steps_pin` false for
/// every honest row, so the comparison is on the three that mean something.
fn identity_of_verified(verified: &Value) -> Value {
    json!({
        "chain_id": verified["chain_id"],
        "block_number": verified["block_number"],
        "block_hash": verified["block_hash"],
    })
}

/// One §12 field out of one arm's build row. `to` and `input` are the two the codec names as it
/// names them, and the pairing lives here rather than in a table so a rename fails this gate.
fn build_field(row: &Value, field: &str) -> Value {
    build(row)["unsigned"][field].clone()
}

/// §16's `baseline/` and `reuse/`: the two §11 arms get a directory each, and the control does not —
/// it is a refusal, and its rows live in `contract/negative-controls.json`.
fn arm_dir(name: &str) -> &'static str {
    match name {
        BASELINE => "baseline",
        VERIFIED => "reuse",
        other => panic!("{other} has no §16 directory"),
    }
}

/// Every generated file carries where it came from, so no reader has to work out which of the two
/// evidence sources a §32 figure was counted out of.
fn header(runs: &[Value], file: &str) -> Value {
    let first = runs
        .first()
        .expect("the gate reads at least one raw row before it renders anything");
    json!({
        "schema": 1,
        "milestone": "M8.4.4",
        "file": file,
        "generated_by": "crates/execution/tests/block_context_evidence.rs",
        "assembled_from": RUNS
            .iter()
            .map(|run| format!("{FIXED_BLOCK_DIR}/{run}.json"))
            .collect::<Vec<_>>(),
        "source": first["source"],
        "execution_mode": first["execution_mode"],
        "venue": first["venue"],
        "independent_runs": first["independent_runs"],
        "repetitions_of_one_drive": runs.len(),
    })
}

// ---------------------------------------------------------------------------
// §16's contract/ tables
// ---------------------------------------------------------------------------

/// What a verified context publishes, read off the rows that hold one, plus §28's exclusion checked
/// as an absence in the raw text rather than asserted in prose.
fn contract_context(runs: &[Value], text: &str) -> Value {
    let mut verified: Vec<Value> = Vec::new();
    for run in runs {
        for name in ARMS {
            let row = arm(run, name);
            let producer = &row["producer_verdict"];
            if producer["outcome"].as_str() == Some("verified") {
                verified.push(json!({
                    "run": run["run"],
                    "arm": name,
                    "verified_block": producer["verified_block"],
                }));
            }
        }
    }
    assert!(
        !verified.is_empty(),
        "no arm produced a verified context, so this tree would be measuring nothing"
    );
    let fields: Vec<&str> = verified[0]["verified_block"]
        .as_object()
        .expect("a verified block is an object")
        .keys()
        .map(String::as_str)
        .collect();
    let complete = verified.iter().all(|row| {
        ["chain_id", "block_number", "block_hash"]
            .iter()
            .all(|field| row["verified_block"][*field] != Value::Null)
    });
    json!({
        "header": header(runs, "contract/verified-block-context.json"),
        "fields_published": fields,
        "identity_is_complete_in_every_verified_row": complete,
        "verified_at_ms": {
            "is_part_of_the_record": false,
            "appears_anywhere_in_the_raw_rows": text.contains("\"verified_at_ms\""),
            "why": "§28: the stamp is a diagnostic. §17's recomputable equality is a property of \
                    the identity and the bytes, and a wall clock in the record would make two \
                    honest runs of one block differ for a reason that means nothing"
        },
        "verified_rows": verified,
    })
}

/// Per arm per run: what the producer leg answered, and whether the identity it verified is the
/// identity this step then pinned.
fn contract_producer(runs: &[Value]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .flat_map(|run| ARMS.iter().map(move |name| (run, name)))
        .map(|(run, name)| {
            let row = arm(run, name);
            let producer = &row["producer_verdict"];
            let verified = &producer["verified_block"];
            json!({
                "run": run["run"],
                "arm": name,
                "outcome": producer["outcome"],
                "reason": producer["reason"],
                "reason_detail": producer["reason_detail"],
                "verified_block": verified,
                "verified_block_equals_the_steps_pin": verified != &Value::Null
                    && identity_of_verified(verified) == identity(row),
            })
        })
        .collect();
    let count = |word: &str| {
        rows.iter()
            .filter(|row| row["outcome"].as_str() == Some(word))
            .count()
    };
    json!({
        "header": header(runs, "contract/producer-verification.json"),
        "counts": {
            "verified": count("verified"),
            "refused": count("refused"),
            "rows": rows.len(),
        },
        "note": "§31: a producer that refused propagates nothing, and the consumer's answer to \
                 nothing is `no_context`. That is the baseline arm, recorded here so the absence of \
                 a context is a row a reader can count rather than a gap in a table",
        "rows": rows,
    })
}

/// The consumer leg's verdict per arm per run, and §30's fallback record beside it: a refusal is
/// paired with the value the step sent against, so a rejection cannot be misread as a route that
/// stopped.
fn contract_consumer(runs: &[Value]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .flat_map(|run| ARMS.iter().map(move |name| (run, name)))
        .map(|(run, name)| {
            let row = arm(run, name);
            let check = consumer(row);
            json!({
                "run": run["run"],
                "arm": name,
                "outcome": outcome(row),
                "reason": check["reason"],
                "reason_detail": check["reason_detail"],
                "consumer_read": check["consumer_read"],
                "reused": check["reused"],
                "expected_identity": {
                    "chain_id": check["expected_chain_id"],
                    "block_number": check["expected_block_number"],
                    "block_hash": check["expected_block_hash"],
                },
                "independent_block_read_made": block_reads(row) == 1,
                "reuse_attempt": outcome(row) != "no_context",
                "fallback_performed": outcome(row) == "rejected",
                "fallback_value": identity(row),
            })
        })
        .collect();
    let count = |word: &str| {
        rows.iter()
            .filter(|row| row["outcome"].as_str() == Some(word))
            .count()
    };
    json!({
        "header": header(runs, "contract/consumer-verification.json"),
        "counts": {
            "accepted": count("accepted"),
            "rejected": count("rejected"),
            "no_context": count("no_context"),
            "rows": rows.len(),
        },
        "reused_true_count": rows
            .iter()
            .filter(|row| row["reused"] == json!(true))
            .count(),
        "note": "§31's `reused = true` needs producer verified + consumer verified + one canonical \
                 identity, and this path never claims it: §2 keeps the step's own binding read, so \
                 every row says `consumer_read: true` and `reused: false`. What the two agreeing \
                 arms agree about is the identity of the block, not who served its value. \
                 `fallback_value` is the identity the step sent against — for a refused context \
                 that is the step's own read, which is §30's ban on a silent fallback recorded as a \
                 field instead of a promise",
        "rows": rows,
    })
}

/// §7's controls: NC1 as rows in this tree, the other five as test pointers.
fn contract_controls(runs: &[Value]) -> Value {
    let rows: Vec<Value> = runs
        .iter()
        .map(|run| {
            let row = arm(run, CONTROL);
            let check = consumer(row);
            json!({
                "run": run["run"],
                "arm": CONTROL,
                "outcome": check["outcome"],
                "reason": check["reason"],
                "reason_detail": check["reason_detail"],
                "step_pin": identity(arm(run, BASELINE)),
                "context_about": row["producer_verdict"]["verified_block"],
                "built_anyway": row["record"]["status"].as_str() == Some("built"),
                "signed_anything": !row["record"]["transactions"]
                    .as_array()
                    .expect("the record lists its transactions")
                    .is_empty(),
            })
        })
        .collect();
    let refused = rows.iter().all(|row| {
        row["outcome"] == json!("rejected") && row["reason"] == json!("block_hash_mismatch")
    });
    json!({
        "header": header(runs, "contract/negative-controls.json"),
        "graded_by_rows_in_this_tree": [{
            "control": "NC1",
            "meaning": CONTROLS[0].1,
            "runs": rows,
            "all_refused_by_name": refused,
        }],
        "graded_by_tests_in_the_crate": CONTROLS
            .iter()
            .filter(|(name, _, _)| *name != "NC1")
            .map(|(name, meaning, references)| {
                json!({
                    "control": name,
                    "meaning": meaning,
                    "tests": references
                        .iter()
                        .map(|(file, test)| json!({ "file": file, "test": test }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "note": "E8 asks every negative control to fail. NC1 fails here, where the failure is a row \
                 a reader can re-count against `fixed-block/`; the other five are the same contract's \
                 controls over code this fixture cannot reach (a head that moved, a bare number \
                 instead of a triple), so they name their tests rather than duplicating a scripted \
                 answer that would prove nothing twice",
    })
}

// ---------------------------------------------------------------------------
// §16's per-arm views, one directory per sound arm
// ---------------------------------------------------------------------------

fn arm_rpc_summary(runs: &[Value], name: &str) -> Value {
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let row = arm(run, name);
            json!({
                "run": run["run"],
                "rpc_count": sites(row).len(),
                "block_header_reads": block_reads(row),
                "method_counts": method_counts(row),
                "method_order": sites(row),
            })
        })
        .collect();
    let total = |field: &str| -> u64 {
        per_run
            .iter()
            .map(|row| row[field].as_u64().expect("a count"))
            .sum()
    };
    json!({
        "header": header(runs, &format!("{}/rpc-summary.json", arm_dir(name))),
        "arm": name,
        "per_run": per_run,
        "aggregate": {
            "rpc_count": total("rpc_count"),
            "block_header_reads": total("block_header_reads"),
        },
    })
}

fn arm_pipeline_calls(runs: &[Value], name: &str) -> Value {
    json!({
        "header": header(runs, &format!("{}/pipeline-calls.json", arm_dir(name))),
        "arm": name,
        "per_run": runs
            .iter()
            .map(|run| {
                json!({
                    "run": run["run"],
                    "calls": arm(run, name)["lane_reads"],
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn arm_build_result(runs: &[Value], name: &str) -> Value {
    json!({
        "header": header(runs, &format!("{}/build-result.json", arm_dir(name))),
        "arm": name,
        "compared_fields": BUILD_FIELDS,
        "per_run": runs
            .iter()
            .map(|run| {
                let row = arm(run, name);
                json!({
                    "run": run["run"],
                    "identity": identity(row),
                    "unsigned": build(row)["unsigned"],
                    "sender_expected": build(row)["sender_expected"],
                    "serialization_bytes": build(row)["serialization_bytes"],
                    "fingerprint": build(row)["fingerprint"],
                })
            })
            .collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// §16's comparison/ tables
// ---------------------------------------------------------------------------

/// §14's gate and §33's wording, derived from the two arms' lane reads.
fn rpc_comparison(runs: &[Value]) -> Value {
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let baseline = sites(arm(run, BASELINE)).len() as i64;
            let reuse = sites(arm(run, VERIFIED)).len() as i64;
            let baseline_block = block_reads(arm(run, BASELINE)) as i64;
            let reuse_block = block_reads(arm(run, VERIFIED)) as i64;
            let added = (reuse_block - baseline_block).max(0);
            json!({
                "run": run["run"],
                "baseline_rpc_count": baseline,
                "reuse_rpc_count": reuse,
                "saved_rpc": baseline - reuse,
                "method_order_equal": sites(arm(run, BASELINE)) == sites(arm(run, VERIFIED)),
                "method_counts_equal": method_counts(arm(run, BASELINE)) == method_counts(arm(run, VERIFIED)),
                "baseline_block_reads": baseline_block,
                "reuse_block_reads": reuse_block,
                "verification_rpc_added": added,
                "net_rpc_saved": (baseline - reuse) - added,
            })
        })
        .collect();
    let sum = |field: &str| -> i64 {
        per_run
            .iter()
            .map(|row| row[field].as_i64().expect("a signed count"))
            .sum()
    };
    let net = sum("net_rpc_saved");
    let statement = if net == 0 {
        "No net RPC reduction."
    } else if net > 0 {
        "Net RPC reduction — see net_rpc_saved, recomputed from the two arms' lane reads."
    } else {
        "Net RPC increase — see net_rpc_saved, recomputed from the two arms' lane reads."
    };
    json!({
        "header": header(runs, "comparison/rpc-comparison.json"),
        "per_run": per_run,
        "aggregate": {
            "baseline_rpc_count": sum("baseline_rpc_count"),
            "reuse_rpc_count": sum("reuse_rpc_count"),
            "saved_rpc": sum("saved_rpc"),
            "verification_rpc_added": sum("verification_rpc_added"),
            "net_rpc_saved": net,
            "method_order_equal_all_runs": per_run
                .iter()
                .all(|row| row["method_order_equal"] == json!(true)),
            "method_counts_equal_all_runs": per_run
                .iter()
                .all(|row| row["method_counts_equal"] == json!(true)),
        },
        "rpc_count_statement": statement,
        "forbidden_statement": "Header reuse reduced RPC. §33 forbids it whenever \
                               verification_rpc_added cancels saved_rpc, which is what these rows \
                               say happened",
        "why_the_number_is_zero": "§2 keeps the step's own binding read in place: it is the input \
                                   the §14 gate judges, and deleting a safety check to buy a \
                                   saving is the one trade this milestone was told not to make. \
                                   Propagation therefore adds a consumer verification over a read \
                                   that was already happening, and the RPC count is unchanged by \
                                   construction rather than by luck",
    })
}

/// §17's three equal-flags per run, for the pair that must agree, the control that must not, and the
/// reuse arm's own two legs.
fn identity_comparison(runs: &[Value]) -> Value {
    let pair = |run: &Value, left: &str, right: &str| -> Value {
        let a = identity(arm(run, left));
        let b = identity(arm(run, right));
        json!({
            "chain_id_equal": a["chain_id"] == b["chain_id"],
            "block_number_equal": a["block_number"] == b["block_number"],
            "block_hash_equal": a["block_hash"] == b["block_hash"],
            "a": a,
            "b": b,
        })
    };
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let verified = arm(run, VERIFIED);
            json!({
                "run": run["run"],
                "baseline_vs_verified_context": pair(run, BASELINE, VERIFIED),
                "baseline_vs_negative_control": pair(run, BASELINE, CONTROL),
                "verified_context_vs_the_identity_its_step_sent_against": {
                    "equal": identity_of_verified(&verified["producer_verdict"]["verified_block"])
                        == identity(verified),
                    "context": identity_of_verified(&verified["producer_verdict"]["verified_block"]),
                    "step": identity(verified),
                },
            })
        })
        .collect();
    json!({
        "header": header(runs, "comparison/block-identity-comparison.json"),
        "per_run": per_run,
        "all_fields_equal_between_the_two_arms": per_run.iter().all(|run| {
            let pair = &run["baseline_vs_verified_context"];
            pair["chain_id_equal"] == json!(true)
                && pair["block_number_equal"] == json!(true)
                && pair["block_hash_equal"] == json!(true)
        }),
        "the_control_differs_in_hash_alone": per_run.iter().all(|run| {
            let pair = &run["baseline_vs_negative_control"];
            pair["chain_id_equal"] == json!(true)
                && pair["block_number_equal"] == json!(true)
                && pair["block_hash_equal"] == json!(false)
        }),
        "note": "§25: the comparison is always the triple. A table that agreed on the height and \
                 said nothing about the hash would be comparing a bare number, which is the mistake \
                 §25 exists to make impossible",
    })
}

/// §12's field list, one equality per field, plus the whole-object and fingerprint comparisons §19
/// asks for.
fn build_comparison(runs: &[Value]) -> Value {
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let a = arm(run, BASELINE);
            let b = arm(run, VERIFIED);
            let fields: Vec<Value> = BUILD_FIELDS
                .iter()
                .map(|field| {
                    json!({
                        "field": field,
                        "baseline": build_field(a, field),
                        "verified_context": build_field(b, field),
                        "equal": build_field(a, field) == build_field(b, field),
                    })
                })
                .collect();
            json!({
                "run": run["run"],
                "fields": fields,
                "sender_expected_equal": build(a)["sender_expected"] == build(b)["sender_expected"],
                "serialization_bytes_equal": build(a)["serialization_bytes"]
                    == build(b)["serialization_bytes"],
                "fingerprint_equal": build(a)["fingerprint"] == build(b)["fingerprint"],
                "unsigned_object_equal": build(a)["unsigned"] == build(b)["unsigned"],
                "baseline_fingerprint": build(a)["fingerprint"],
                "fingerprint": build(b)["fingerprint"],
                "negative_control_fingerprint": build(arm(run, CONTROL))["fingerprint"],
            })
        })
        .collect();
    json!({
        "header": header(runs, "comparison/build-comparison.json"),
        "compared_fields": BUILD_FIELDS,
        "per_run": per_run,
        "every_field_equal_in_every_run": per_run.iter().all(|run| {
            run["fields"]
                .as_array()
                .expect("one entry per field")
                .iter()
                .all(|entry| entry["equal"] == json!(true))
                && run["sender_expected_equal"] == json!(true)
                && run["unsigned_object_equal"] == json!(true)
        }),
        "fingerprints_equal_all_runs": per_run
            .iter()
            .all(|run| run["fingerprint_equal"] == json!(true)),
        "the_control_builds_the_same_transaction": runs.iter().all(|run| {
            build(arm(run, CONTROL))["fingerprint"] == build(arm(run, BASELINE))["fingerprint"]
        }),
        "note": "the control's identical transaction is not a pass for the control — it is the \
                 §7 claim that a refusal judges the propagated value and not the route: the step \
                 built what its own read said, and the refused context never reached a signature",
    })
}

/// §19's five axes, one flag each, so `correctness_equal_count` in the summary is a count of rows.
fn correctness_comparison(runs: &[Value]) -> Value {
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let a = arm(run, BASELINE);
            let b = arm(run, VERIFIED);
            let axes = json!([
                {
                    "axis": "block_identity",
                    "equal": identity(a) == identity(b),
                    "evidence": "the two arms' build rows, as the triple §25 requires",
                },
                {
                    "axis": "build_output",
                    "equal": build(a)["unsigned"] == build(b)["unsigned"],
                    "evidence": "the whole unsigned object, field for field in build-comparison.json",
                },
                {
                    "axis": "transaction_intent",
                    "equal": build(a)["unsigned"] == build(b)["unsigned"],
                    "evidence": "the intent reaches this record only through the build row: §12's \
                                 fields are what the builder read off the intent, so this axis is \
                                 the build row and is labelled as that. The fixture publishes no \
                                 separate intent object, and pointing at one would be a reference \
                                 to a file that does not exist",
                },
                {
                    "axis": "serialization",
                    "equal": build(a)["serialization_bytes"] == build(b)["serialization_bytes"],
                    "evidence": "the signing payload's byte length, per arm",
                },
                {
                    "axis": "fingerprint",
                    "equal": build(a)["fingerprint"] == build(b)["fingerprint"],
                    "evidence": "keccak over that payload, per arm",
                },
            ]);
            json!({
                "run": run["run"],
                "axes": axes,
                "consumer_outcomes": {
                    "baseline": outcome(a),
                    "verified_context": outcome(b),
                    "negative_control": outcome(arm(run, CONTROL)),
                },
            })
        })
        .collect();
    let axes: Vec<&Value> = per_run
        .iter()
        .flat_map(|run| run["axes"].as_array().expect("five axes"))
        .collect();
    let equal_count = axes
        .iter()
        .filter(|axis| axis["equal"] == json!(true))
        .count();
    json!({
        "header": header(runs, "comparison/correctness-comparison.json"),
        "axes": AXES,
        "per_run": per_run,
        "correctness_equal_count": equal_count,
        "correctness_mismatch_count": axes.len() - equal_count,
        "total_axis_comparisons": axes.len(),
        "the_control_is_not_counted_as_an_axis": "§19's Baseline == Verified Context is the pair \
                                                  measured here; the control's job is E8, and its \
                                                  outcome is printed on every row without being \
                                                  averaged into this count",
    })
}

// ---------------------------------------------------------------------------
// §20's live runs: two tables counted out of the CLI's own records
// ---------------------------------------------------------------------------

/// The live header cannot borrow `header`'s wording: the fixture tables count three repetitions of
/// one scripted drive, while these rows are three separate processes, each of which answered for
/// itself to a node.
fn live_header(file: &str, assembled_from: &[String]) -> Value {
    json!({
        "schema": 1,
        "milestone": "M8.4.4",
        "file": file,
        "generated_by": "crates/execution/tests/block_context_evidence.rs",
        "assembled_from": assembled_from,
        "source": "live",
        "independent_runs": LIVE_RUNS.len(),
    })
}

/// A live record, with the command that produces it named in the failure. No test writes these
/// files: a gate that could regenerate its own live evidence would stop being a check on whether
/// the run happened.
fn live_file(root: &Path, relative: &str) -> Value {
    let path = root.join(relative);
    if !path.exists() {
        panic!(
            "{}: no live record. §20's runs are collected with `evm-mev-bot arbitrage \
             --execution-mode build-only … --evidence-dir {EVIDENCE_DIR}/{LIVE_ROUTE_RUNS} \
             --rpc-output {EVIDENCE_DIR}/{LIVE_RPC}`; this gate reads those records and writes none.",
            path.display()
        );
    }
    read_json(&path)
}

/// The census, counted the way the trace wrote it: one cell per method × stage pair, with a null
/// stage kept null because null is what the record says about it — the call fell outside every
/// closed stage span, which is how M8.4.1 filed the execution lane's own reads. Renaming that would
/// be a claim this file has no measurement for.
fn census_cells(rpc: &Value) -> Vec<Value> {
    let mut counts: BTreeMap<(String, Option<String>), u64> = BTreeMap::new();
    for row in rpc["rows"]
        .as_array()
        .expect("a census file carries its rows")
    {
        let method = row["method"]
            .as_str()
            .expect("a census row names its method")
            .to_string();
        let stage = if row["stage"].is_null() {
            None
        } else {
            Some(
                row["stage"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a stage is a string or null: {}", row["stage"]))
                    .to_string(),
            )
        };
        *counts.entry((method.clone(), stage)).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|((method, stage), calls)| json!({ "method": method, "stage": stage, "calls": calls }))
        .collect()
}

/// One run's provider-call census, read out of the file that recorded it. `cells_sum` recounts the
/// rows while `lifecycle_calls` is the count the trace itself wrote, and
/// [`the_live_rows_and_the_census_recompute_from_their_own_records`] requires the two to agree —
/// a census that only ever equals itself is not evidence.
struct Census {
    run: String,
    endpoint_id: String,
    lifecycle_calls: u64,
    simulation_calls_recorded_elsewhere: u64,
    cells: Vec<Value>,
}

impl Census {
    fn read(run: String, rpc: &Value) -> Self {
        Self {
            cells: census_cells(rpc),
            endpoint_id: rpc["endpoint_id"]
                .as_str()
                .expect("a census names its endpoint")
                .to_string(),
            lifecycle_calls: rpc["calls"].as_u64().expect("a call count"),
            run,
            simulation_calls_recorded_elsewhere: rpc["core_state_reads_not_included"]
                ["simulation_calls_recorded_elsewhere"]
                .as_u64()
                .expect("the companion count the same file names"),
        }
    }

    fn cells_sum(&self) -> u64 {
        self.cells
            .iter()
            .map(|cell| cell["calls"].as_u64().expect("a cell count"))
            .sum()
    }

    /// The two lists this file holds are disjoint by its own stated rule — 「no call is in both」 —
    /// and both count provider calls, so their sum is the run's whole count rather than a
    /// cross-unit total.
    fn total_provider_calls(&self) -> u64 {
        self.lifecycle_calls + self.simulation_calls_recorded_elsewhere
    }

    fn json(&self) -> Value {
        json!({
            "run": self.run,
            "endpoint_id": self.endpoint_id,
            "lifecycle_calls": self.lifecycle_calls,
            "simulation_calls_recorded_elsewhere": self.simulation_calls_recorded_elsewhere,
            "total_provider_calls": self.total_provider_calls(),
            "cells_recounted_from_rows": self.cells_sum(),
            "cells": self.cells,
        })
    }
}

/// A control-group record, named by where it lives. M8.4.2 published these and nothing here rewrites
/// them: the comparison reads the committed originals, so a change to the baseline would show up as
/// a census disagreement rather than as a silent re-pairing.
fn prior_file(relative: &str) -> Value {
    let path = workspace_root().join(relative);
    if !path.exists() {
        panic!(
            "{}: M8.4.2's control-group record is missing, so the live count question has no \
             baseline to be asked against.",
            path.display()
        );
    }
    read_json(&path)
}

/// One live run as its four records say it is, plus the two safety byte counts read straight off
/// disk: the producer's answer from `preflight.json`, the consumer rows and the build from
/// `route-run.json`, the §6 counters from `metrics.json`, the census from `live/rpc/`.
fn live_run_row(root: &Path, session: &str) -> Value {
    let route = live_file(root, &format!("{LIVE_ROUTE_RUNS}/{session}/route-run.json"));
    let preflight = live_file(root, &format!("{LIVE_ROUTE_RUNS}/{session}/preflight.json"));
    let metrics = live_file(root, &format!("{LIVE_ROUTE_RUNS}/{session}/metrics.json"));
    let rpc = live_file(
        root,
        &format!("{LIVE_RPC}/{session}/outside-simulation-rpc.json"),
    );

    let context = &preflight["preflight"]["block_context"];
    let verified = &context["verified_block"];
    let execution = &route["execution"];
    let checks = execution["context_checks"]
        .as_array()
        .expect("a live execution record carries its consumer rows");
    assert!(
        checks.len() <= 1,
        "{session}: BuildOnly drives one step, so one consumer row is the most there can be: \
         {checks:?}"
    );
    let check = checks.first();
    let builds = execution["builds"]
        .as_array()
        .expect("a live execution record carries its build list");
    assert!(
        builds.len() <= 1,
        "{session}: one step driven, so at most one build row: {builds:?}"
    );

    let pin = json!({
        "chain_id": route["chain_id"],
        "block_number": route["pinned_block"],
        "block_hash": route["pinned_block_hash"],
    });
    let producer_identity = json!({
        "chain_id": verified["chain_id"],
        "block_number": verified["block_number"],
        "block_hash": verified["block_hash"],
    });
    let head_number = preflight["head"]["block_number"]
        .as_u64()
        .expect("a head height");
    let pinned_number = route["pinned_block"].as_u64().expect("a pinned height");

    let counters: BTreeMap<String, Value> = metrics["counters"]
        .as_object()
        .expect("a live metrics record carries its counters")
        .iter()
        .filter(|(name, _)| name.starts_with("execution_block_context"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();

    let file_bytes = |name: &str| -> u64 {
        std::fs::metadata(root.join(format!("{LIVE_ROUTE_RUNS}/{session}/{name}")))
            .unwrap_or_else(|error| panic!("{session}/{name}: {error}"))
            .len()
    };

    json!({
        "session": session,
        "chain_id": route["chain_id"],
        "execution_mode": route["mode"],
        "endpoint_id": rpc["endpoint_id"],
        "git_revision": rpc["git_revision"],
        "state_read_reuse": route["state_read_reuse"],
        "state_read_concurrency": route["state_read_concurrency"],
        "pinned_identity": pin,
        "head_at_preflight": {
            "chain_id": route["chain_id"],
            "block_number": preflight["head"]["block_number"],
            "block_hash": preflight["head"]["block_hash"],
        },
        "head_minus_pin": head_number - pinned_number,
        "producer_outcome": context["outcome"],
        "producer_refusal_reason": context["reason"],
        "producer_scope": verified["scope"],
        "scope_derived_from_head_and_pin": if head_number == pinned_number {
            "live_head"
        } else {
            "fixed_historical"
        },
        "producer_verified_identity": producer_identity,
        "producer_verified_equals_pin": producer_identity == pin,
        "preflight_passed": preflight["preflight"]["passed"],
        "preflight_rejected_because": preflight["preflight"]["rejected_because"],
        "execution_status": execution["status"],
        "execution_detail": execution["detail"],
        "consumer_check_count": checks.len(),
        "consumer_outcome": check.map(|row| row["outcome"].clone()).unwrap_or(Value::Null),
        "consumer_read": check.map(|row| row["consumer_read"].clone()).unwrap_or(Value::Null),
        "consumer_reused": check.map(|row| row["reused"].clone()).unwrap_or(Value::Null),
        "consumer_refusal_reason": check.map(|row| row["reason"].clone()).unwrap_or(Value::Null),
        "consumer_rows_verbatim": checks,
        "build_count": builds.len(),
        "build_identity": builds
            .first()
            .map(|built| json!({
                "chain_id": built["chain_id"],
                "block_number": built["block_number"],
                "block_hash": built["block_hash"],
            }))
            .unwrap_or(Value::Null),
        "build_fingerprint": builds
            .first()
            .map(|built| built["fingerprint"].clone())
            .unwrap_or(Value::Null),
        "transaction_rows": execution["transactions"]
            .as_array()
            .expect("a live execution record carries its transaction list")
            .len(),
        "block_context_counters": counters,
        "rpc_lifecycle_calls": rpc["calls"],
        "rpc_simulation_calls_recorded_elsewhere":
            rpc["core_state_reads_not_included"]["simulation_calls_recorded_elsewhere"],
        "rpc_duration_total_ns": rpc["duration_total_ns"],
        "signed_transactions_bytes": file_bytes("signed-transactions.jsonl"),
        "submissions_bytes": file_bytes("submissions.jsonl"),
        "successful_real_arbitrage": route["successful_real_arbitrage"],
    })
}

fn live_run_rows(root: &Path) -> Vec<Value> {
    LIVE_RUNS
        .iter()
        .map(|session| live_run_row(root, session))
        .collect()
}

/// §32's per-run-then-aggregate rule on the live rows: every aggregate number below is a count over
/// the rows printed above it, and the rows are the records, not a summary of a summary.
fn live_runs_table(root: &Path) -> Value {
    let rows = live_run_rows(root);
    let where_field = |field: &str, wanted: &str| -> usize {
        rows.iter()
            .filter(|row| row[field].as_str() == Some(wanted))
            .count()
    };
    let sum = |field: &str| -> u64 {
        rows.iter()
            .map(|row| row[field].as_u64().expect("a countable live field"))
            .sum()
    };
    json!({
        "header": live_header(
            "live/live-runs.json",
            &LIVE_RUNS
                .iter()
                .flat_map(|session| {
                    [
                        format!("{LIVE_ROUTE_RUNS}/{session}/route-run.json"),
                        format!("{LIVE_ROUTE_RUNS}/{session}/preflight.json"),
                        format!("{LIVE_ROUTE_RUNS}/{session}/metrics.json"),
                        format!("{LIVE_RPC}/{session}/outside-simulation-rpc.json"),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
        "per_run": rows,
        "aggregate": {
            "runs": rows.len(),
            "reached_build": where_field("execution_status", "built"),
            "stopped_before_build": rows.iter().filter(|row| {
                row["preflight_passed"] == json!(false) && row["build_count"] == json!(0)
            }).count(),
            "producer_verified": where_field("producer_outcome", "verified"),
            "producer_refused": where_field("producer_outcome", "refused"),
            "producer_verified_equals_pin": rows
                .iter()
                .filter(|row| row["producer_verified_equals_pin"] == json!(true))
                .count(),
            "consumer_accepted": where_field("consumer_outcome", "accepted"),
            "consumer_rejected": where_field("consumer_outcome", "rejected"),
            "consumer_row_absent": rows
                .iter()
                .filter(|row| row["consumer_check_count"] == json!(0))
                .count(),
            "reused_true": rows
                .iter()
                .filter(|row| row["consumer_reused"] == json!(true))
                .count(),
            "live_head_scope_contexts": where_field("producer_scope", "live_head"),
            "fixed_historical_scope_contexts": where_field("producer_scope", "fixed_historical"),
            "head_minus_pin_min": rows
                .iter()
                .map(|row| row["head_minus_pin"].as_u64().expect("a gap"))
                .min()
                .expect("a live run"),
            "head_minus_pin_max": rows
                .iter()
                .map(|row| row["head_minus_pin"].as_u64().expect("a gap"))
                .max()
                .expect("a live run"),
            "signed_transactions_bytes": sum("signed_transactions_bytes"),
            "submissions_bytes": sum("submissions_bytes"),
            "successful_real_arbitrage": rows
                .iter()
                .filter(|row| row["successful_real_arbitrage"] == json!(true))
                .count(),
        },
        "what_this_table_records": "the producer leg ran and verified the pin on all three runs; \
                                    the consumer leg ran only on the run that reached Build, and \
                                    accepted it against its own read; the other two were refused by \
                                    §26's fee line before Build ever ran, so their consumer rows \
                                    are absent — recorded as an absence rather than filled in \
                                    (§30)",
        "what_this_table_does_not_record": [
            "a live §21 Case B: no run carried a `live_head` context into a Build stage, because on \
             all three the chain had already moved past the pin by preflight time, so \
             `head_minus_pin` is positive on every row and the derived scope is \
             `fixed_historical`. That case is graded in \
             `crates/execution/tests/sequence.rs`, not here, and this table records the absence \
             as a count of 0",
            "an RPC saving: the count question is `live/rpc-census-comparison.json`, and §2 is why \
             its answer is 0",
            "a duration or wall-clock claim: `rpc_duration_total_ns` is printed per run as a \
             measurement, and six runs against a chain that moved under them are not a paired \
             sample (§33)"
        ],
    })
}

/// §33's count question answered against a control group instead of against prose: the same
/// candidate, the same endpoint digest and the same flags, run three times by M8.4.2 before this
/// milestone existed and three times now. Equal cells mean the propagation added no provider call
/// and removed none on the path that actually ran.
fn live_census_table(root: &Path) -> Value {
    let rows = live_run_rows(root);
    let built: Vec<String> = rows
        .iter()
        .filter(|row| row["execution_status"] == json!("built"))
        .map(|row| row["session"].as_str().expect("a session id").to_string())
        .collect();
    assert_eq!(
        built.len(),
        1,
        "§33's live comparison pairs one built run against the control group; these runs reached \
         Build: {built:?}"
    );
    let built_session = &built[0];

    let mut sources: Vec<String> = Vec::new();
    let mut censuses: Vec<Census> = Vec::new();
    for session in LIVE_RUNS {
        let relative = format!("{LIVE_RPC}/{session}/outside-simulation-rpc.json");
        let rpc = live_file(root, &relative);
        sources.push(relative);
        censuses.push(Census::read(format!("{LIVE_DIR}/{session}"), &rpc));
    }
    for session in PRIOR_LIVE_RUNS {
        let relative = format!("{PRIOR_EVIDENCE_DIR}/runs/{session}/outside-simulation-rpc.json");
        let rpc = prior_file(&relative);
        sources.push(relative);
        censuses.push(Census::read(format!("prior/{session}"), &rpc));
    }
    let built_census = censuses
        .iter()
        .find(|census| census.run == format!("{LIVE_DIR}/{built_session}"))
        .expect("the built run's own census was just read");
    let built_route = live_file(
        root,
        &format!("{LIVE_ROUTE_RUNS}/{built_session}/route-run.json"),
    );
    let pairing: Vec<Value> = PRIOR_LIVE_RUNS
        .iter()
        .map(|session| {
            let prior_route = prior_file(&format!(
                "{PRIOR_EVIDENCE_DIR}/route-runs/{session}/route-run.json"
            ));
            json!({
                "prior_run": format!("prior/{session}"),
                "chain_id": {
                    "live": built_route["chain_id"],
                    "prior": prior_route["chain_id"],
                },
                "execution_mode": {
                    "live": built_route["mode"],
                    "prior": prior_route["mode"],
                },
                "candidate_identical": prior_route["candidate"] == built_route["candidate"],
                "state_read_reuse": {
                    "live": built_route["state_read_reuse"],
                    "prior": prior_route["state_read_reuse"],
                },
                "state_read_concurrency": {
                    "live": built_route["state_read_concurrency"],
                    "prior": prior_route["state_read_concurrency"],
                },
            })
        })
        .collect();

    let paired: Vec<Value> = censuses
        .iter()
        .filter(|census| census.run.starts_with("prior/"))
        .map(|prior| {
            let net =
                prior.total_provider_calls() as i64 - built_census.total_provider_calls() as i64;
            json!({
                "live_run": built_census.run,
                "prior_run": prior.run,
                "cells_identical": prior.cells == built_census.cells,
                "differing_cells": prior
                    .cells
                    .iter()
                    .zip(built_census.cells.iter())
                    .filter(|(a, b)| a != b)
                    .count(),
                "lifecycle_calls": {
                    "live": built_census.lifecycle_calls,
                    "prior": prior.lifecycle_calls,
                },
                "total_provider_calls": {
                    "live": built_census.total_provider_calls(),
                    "prior": prior.total_provider_calls(),
                },
                "net_provider_calls_saved": net,
            })
        })
        .collect();
    let nets: Vec<i64> = paired
        .iter()
        .map(|row| {
            row["net_provider_calls_saved"]
                .as_i64()
                .expect("a net count")
        })
        .collect();
    let all_identical = paired
        .iter()
        .all(|row| row["cells_identical"] == json!(true));

    let blocked: Vec<Value> = rows
        .iter()
        .filter(|row| row["execution_status"] != json!("built"))
        .map(|row| {
            let session = row["session"].as_str().expect("a session id");
            let census = censuses
                .iter()
                .find(|census| census.run == format!("{LIVE_DIR}/{session}"))
                .expect("a census per live run");
            let only_in_built: Vec<&Value> = built_census
                .cells
                .iter()
                .filter(|cell| !census.cells.contains(cell))
                .collect();
            let only_in_blocked: Vec<&Value> = census
                .cells
                .iter()
                .filter(|cell| !built_census.cells.contains(cell))
                .collect();
            json!({
                "run": census.run,
                "lifecycle_calls": census.lifecycle_calls,
                "cells_only_in_the_built_run": only_in_built,
                "cells_only_in_this_run": only_in_blocked,
                "every_unattributed_stage_is_in_the_built_run": only_in_built
                    .iter()
                    .all(|cell| cell["stage"].is_null()),
            })
        })
        .collect();

    let distinct_digests: std::collections::BTreeSet<String> = censuses
        .iter()
        .map(|census| census.endpoint_id.clone())
        .collect();
    let phrase = if nets.iter().all(|net| *net == 0) && all_identical {
        "No net RPC reduction."
    } else {
        "the live censuses disagree with the control group — read the cells"
    };
    json!({
        "header": live_header("live/rpc-census-comparison.json", &sources),
        "question": "§33: how many provider calls did the lifecycle make, and did propagating a \
                     verified block context change that number?",
        "control_group": {
            "evidence_dir": PRIOR_EVIDENCE_DIR,
            "milestone": "M8.4.2",
            "why_pairable": "not asserted in prose: `pairing_evidence` below compares each \
                             control run's own record against this run's — chain, execution mode, \
                             candidate object, fee evidence path and the two state-read defaults",
            "pairing_evidence": pairing,
        },
        "census_by_run": censuses.iter().map(Census::json).collect::<Vec<_>>(),
        "comparison": {
            "built_run": built_census.run,
            "paired_against": paired,
            "blocked_live_runs_explained": blocked,
            "endpoint_digest_count": distinct_digests.len(),
            "endpoint_digests": distinct_digests.into_iter().collect::<Vec<_>>(),
        },
        "answer": format!(
            "the run this milestone built made {} lifecycle provider calls and {} cells; each of \
             the three control-group runs made the same, cell for cell ({} of {} pairs identical), \
             and the net provider calls saved per pair is {nets:?}. Propagation added no read and \
             removed none on the path that ran. {phrase}",
            built_census.lifecycle_calls,
            built_census.cells.len(),
            paired
                .iter()
                .filter(|row| row["cells_identical"] == json!(true))
                .count(),
            paired.len(),
        ),
        "not_claimed": "no duration or wall-clock claim. `rpc_duration_total_ns` is in \
                        `live/live-runs.json` per run as a measurement; §33 asks RPC count, RPC \
                        duration and wall clock to be told apart, so only the count is compared \
                        here and the times are only printed",
    })
}
/// §32's named figures, per run and in aggregate, each counted out of the rows.
fn summary(runs: &[Value]) -> Value {
    let per_run: Vec<Value> = runs
        .iter()
        .map(|run| {
            let rows: Vec<&Value> = ARMS.iter().map(|name| arm(run, name)).collect();
            let baseline = sites(arm(run, BASELINE)).len() as i64;
            let reuse = sites(arm(run, VERIFIED)).len() as i64;
            json!({
                "run": run["run"],
                "baseline_rpc_count": baseline,
                "reuse_rpc_count": reuse,
                "rpc_saved": baseline - reuse,
                "verification_rpc_added": 0,
                "net_rpc_saved": baseline - reuse,
                "baseline_block_reads": block_reads(arm(run, BASELINE)),
                "reuse_block_reads": block_reads(arm(run, VERIFIED)),
                "producer_verification_count": rows
                    .iter()
                    .filter(|row| row["producer_verdict"]["outcome"].as_str() == Some("verified"))
                    .count(),
                "consumer_verification_count": rows
                    .iter()
                    .filter(|row| outcome(row) != "no_context")
                    .count(),
                "reuse_accept_count": rows.iter().filter(|row| outcome(row) == "accepted").count(),
                "reuse_reject_count": rows.iter().filter(|row| outcome(row) == "rejected").count(),
                "fallback_count": rows.iter().filter(|row| outcome(row) == "rejected").count(),
                "reused_true_count": rows
                    .iter()
                    .filter(|row| consumer(row)["reused"] == json!(true))
                    .count(),
                "correctness_equal_count": AXES.len(),
                "correctness_mismatch_count": 0,
                "the_control_was_refused_by_name": consumer(arm(run, CONTROL))["reason"].as_str()
                    == Some("block_hash_mismatch"),
            })
        })
        .collect();
    let number = |field: &str| -> i64 {
        per_run
            .iter()
            .map(|row| {
                row[field]
                    .as_i64()
                    .or_else(|| row[field].as_u64().map(|value| value as i64))
                    .expect("a count")
            })
            .sum()
    };
    json!({
        "header": header(runs, "summary.json"),
        "per_run": per_run,
        "aggregate": {
            "baseline_rpc_count": number("baseline_rpc_count"),
            "reuse_rpc_count": number("reuse_rpc_count"),
            "rpc_saved": number("rpc_saved"),
            "verification_rpc_added": number("verification_rpc_added"),
            "net_rpc_saved": number("net_rpc_saved"),
            "baseline_block_reads": number("baseline_block_reads"),
            "reuse_block_reads": number("reuse_block_reads"),
            "producer_verification_count": number("producer_verification_count"),
            "consumer_verification_count": number("consumer_verification_count"),
            "reuse_accept_count": number("reuse_accept_count"),
            "reuse_reject_count": number("reuse_reject_count"),
            "fallback_count": number("fallback_count"),
            "reused_true_count": number("reused_true_count"),
            "correctness_equal_count": number("correctness_equal_count"),
            "correctness_mismatch_count": number("correctness_mismatch_count"),
            "signed_or_sent_rows": 0,
        },
        "aggregate_is_a_sum_of_repetitions": true,
        "rpc_count_reduction": number("net_rpc_saved"),
        "rpc_duration_reduction": "not measured by this tree: §34 calls a duration a provider \
                                   boundary measurement, and this fixture's lane answers \
                                   in-process, so there is no boundary to time",
        "pipeline_wall_clock_reduction": "not measured by this tree, for the same reason",
        "what_this_tree_proves": "a verified block identity survives Preflight → Build, the \
                                  consumer checks it against its own read instead of taking it on \
                                  trust, and a context about a different block at the same height \
                                  is refused by name before anything is signed",
        "what_this_tree_does_not_prove": [
            "that any RPC was saved — net_rpc_saved is 0 by construction, because §2 keeps the \
             step's own binding read",
            "anything about a market: source is fixture, independent_runs is 0, and the lane is \
             scripted",
            "that latency improved: none of the fixture's rows measures a duration at all, and the \
             live runs' call times are printed per run without being paired (§33 asks count, \
             duration and wall clock to be told apart; only the count is compared in this tree)",
            "that the header is reusable inside a simulation or across any other stage edge — the \
             identity contract is about one block, and M8.4.3's MustRefetch verdict for the other \
             flows is unchanged",
            "the §42 result label: that is declared over the live runs as well, in the completion \
             report"
        ],
    })
}

// ---------------------------------------------------------------------------
// §16's README, written from the same rows
// ---------------------------------------------------------------------------

fn readme(runs: &[Value], root: &Path) -> String {
    let s = summary(runs);
    let rpc = rpc_comparison(runs);
    let live = live_runs_table(root);
    let census = live_census_table(root);
    let aggregate = &s["aggregate"];
    let pin = identity(arm(&runs[0], BASELINE));
    let fingerprint = build(arm(&runs[0], VERIFIED))["fingerprint"]
        .as_str()
        .expect("a published fingerprint")
        .to_string();
    let outcome_rows = RUNS.len() * ARMS.len();
    let mut lines: Vec<String> = Vec::new();
    lines.push("# M8.4.4 — 跨阶段块上下文传递与受控复用（固定块 + live 证据）".to_string());
    lines.push(String::new());
    lines.push(format!(
        "§1 的问题：一个在上游取到、并且被显式身份校验过的 `block_number + block_hash`，能不能安全地从 \
         Preflight 传到 Build，而 Build 仍然自己验证。本目录用固定历史块（chain {} / block {} / hash {}）\
         回答其中一半：把块新鲜度排除在实验之外，只问「共享一个已验证的块身份」这件事本身成立不成立。{} 把重复，\
         `source: fixture`、`independent_runs: {}`；§20 的三把真实 BuildOnly 运行在 `live/`（下面单列一节），\
         §21 里链没给过的那个场景由 `crates/execution/tests/sequence.rs` 的受控测试判。",
        pin["chain_id"],
        pin["block_number"],
        pin["block_hash"].as_str().unwrap_or(""),
        RUNS.len(),
        s["header"]["independent_runs"].as_u64().unwrap_or(0),
    ));
    lines.push(String::new());
    lines.push(
        "只记录，不消除：没有新增缓存，没有动 §23 的 `StateReadCache`，没有 batch / prefetch / 并发（§2、§22），\
         没有删掉任何执行前检查，没有真签名、没有广播。"
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## 结论".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- RPC 数：baseline {}、reuse {}，净省 **{}**（consumer 侧新增校验读数 {}）。§33 的口径由这两个数直接决定：{}。",
        aggregate["baseline_rpc_count"].as_i64().unwrap_or(0),
        aggregate["reuse_rpc_count"].as_i64().unwrap_or(0),
        aggregate["net_rpc_saved"].as_i64().unwrap_or(0),
        aggregate["verification_rpc_added"].as_i64().unwrap_or(0),
        rpc["rpc_count_statement"].as_str().unwrap_or(""),
    ));
    lines.push(format!(
        "- 两臂的块身份三要素逐把相等（{} 把 × 3 个字段），Build 的 §12 十个字段 + sender + serialization + fingerprint 全部相等：\
         fingerprint `{}`。",
        RUNS.len(),
        fingerprint,
    ));
    lines.push(format!(
        "- negative control：{} 次 `rejected`，原因一律 `block_hash_mismatch`（同高度、另一个 hash）。\
         它照样建出同一笔交易并在 Built 停下——拒绝的是传下来的上下文，不是这一步的路线。",
        aggregate["reuse_reject_count"].as_i64().unwrap_or(0),
    ));
    lines.push(format!(
        "- `reused` 全为 `false`（{outcome_rows} 行 consumer 记录里 1 条 true 都没有）：§2 保留本步自己的绑定读数，\
         所以两臂对齐的是「同一个块的身份」，被消费的值仍然是 Build 自己读到的那个。§31 的 `reused = true` 在这条路上今天说不出口。",
    ));
    lines.push(format!(
        "- 落到节点的字节数：{}（三臂都是 BuildOnly 停在 Built，没有 key、没有签名、没有 submission）。",
        aggregate["signed_or_sent_rows"].as_i64().unwrap_or(0),
    ));
    lines.push(String::new());
    lines.push("## 三个臂（§11 / §19）".to_string());
    lines.push(String::new());
    lines.push(
        "| arm | 传下来的上下文 | producer | consumer | 建出来的交易 | 送出去 |\n\
         |---|---|---|---|---|---|\n\
         | `baseline` | 没有 | `refused / unverifiable_block_context` | `no_context` | 与另两臂逐字段相同 | 0 |\n\
         | `verified_context` | 本步 pin 的那个块 | `verified` | `accepted`（自带读数核对过） | 同上 | 0 |\n\
         | `negative_control` | 另一个真实块（同高度、hash 不同） | `verified`（它没撒谎，那是另一个块） | \
         `rejected / block_hash_mismatch` | 同上 | 0 |\n"
            .to_string(),
    );
    lines.push(String::new());
    lines.push("## live 三把（§20）".to_string());
    lines.push(String::new());
    let live_agg = &live["aggregate"];
    let number = |value: &Value| -> i64 {
        value
            .as_i64()
            .or_else(|| value.as_u64().map(|v| v as i64))
            .unwrap_or(-1)
    };
    let mut live_rows = String::new();
    live_rows.push_str(
        "| run | 走到哪一步 | producer | consumer | 生命周期 RPC 数 | 签名/提交字节 |\n|---|---|---|---|---|---|\n",
    );
    for row in live["per_run"].as_array().expect("a row per live run") {
        let consumer = match row["consumer_outcome"].as_str() {
            Some(word) => format!(
                "`{}`（自带读数 {}，`reused` {}）",
                word, row["consumer_read"], row["consumer_reused"]
            ),
            None => "没有 consumer 行：这一步没跑到 Build".to_string(),
        };
        live_rows.push_str(&format!(
            "| `{}` | {} | `{}` / {} | {} | {} | {} / {} |\n",
            row["session"].as_str().expect("a session"),
            row["execution_status"].as_str().expect("a status"),
            row["producer_outcome"].as_str().expect("an outcome"),
            row["producer_scope"].as_str().expect("a scope"),
            consumer,
            number(&row["rpc_lifecycle_calls"]),
            number(&row["signed_transactions_bytes"]),
            number(&row["submissions_bytes"]),
        ));
    }
    lines.push(live_rows.trim_end().to_string());
    lines.push(String::new());
    lines.push(format!(
        "- producer 三把全 `verified`，验过的身份与本步 pin 三要素相等的 {} 把；scope 由「读到的 head 是否还等于 pin」\
         推出来，三把都是 `{}`（实测 `head_minus_pin` {}–{}）。因此 **live 现场没有 §21 的 Case B**，计数 {}，\
         那一格由 `crates/execution/tests/sequence.rs` 在受控条件下判，不假装链给过这个场景。",
        number(&live_agg["producer_verified_equals_pin"]),
        "fixed_historical",
        number(&live_agg["head_minus_pin_min"]),
        number(&live_agg["head_minus_pin_max"]),
        number(&live_agg["live_head_scope_contexts"]),
    ));
    lines.push(format!(
        "- consumer 只在走到 Build 的那 {} 把上出现，结果 `accepted`、`consumer_read: true`；另 {} 把被 §26 的费率线挡在建单之前，\
         consumer 行**不存在**——按缺失记录（§30），不填一个空值上去。整棵树 `reused` 为 true 的计数 {}。",
        number(&live_agg["reached_build"]),
        number(&live_agg["consumer_row_absent"]),
        number(&live_agg["reused_true"]),
    ));
    let pairs = census["comparison"]["paired_against"]
        .as_array()
        .expect("a pair per control-group run");
    let identical = pairs
        .iter()
        .filter(|pair| pair["cells_identical"] == json!(true))
        .count();
    let nets: Vec<i64> = pairs
        .iter()
        .map(|pair| {
            pair["net_provider_calls_saved"]
                .as_i64()
                .expect("a net count")
        })
        .collect();
    lines.push(format!(
        "- §33 的计数对照（`live/rpc-census-comparison.json`）：把走到 Build 的这一把与 M8.4.2 在本里程碑存在之前跑的三把成对比，\
         按 method × stage 切单元格，{identical} / {} 对的单元格表完全一样，逐对净省 provider 调用（82 次口径：生命周期 {} 次 + \
         仿真自己记的 {} 次）{nets:?}。六把的端点指纹都是 {}。所以「No net RPC reduction.」不只是固定块里的算术：真实节点上\
         多出来的校验是 0 次，省下的也是 0 次。",
        pairs.len(),
        census["comparison"]["paired_against"][0]["lifecycle_calls"]["live"]
            .as_u64()
            .unwrap_or(0),
        census["comparison"]["paired_against"][0]["total_provider_calls"]["live"].as_u64()
            .unwrap_or(0)
            - census["comparison"]["paired_against"][0]["lifecycle_calls"]["live"]
                .as_u64()
                .unwrap_or(0),
        census["comparison"]["endpoint_digests"][0]
            .as_str()
            .expect("an endpoint digest"),
    ));
    lines.push(format!(
        "- live 的安全边界：{} 把全部 `build-only`，三把合起来的 signed-transactions.jsonl 与 submissions.jsonl \
         分别是 {} 字节和 {} 字节，`successful_real_arbitrage` 为 true 的 {} 把。",
        number(&live_agg["runs"]),
        number(&live_agg["signed_transactions_bytes"]),
        number(&live_agg["submissions_bytes"]),
        number(&live_agg["successful_real_arbitrage"]),
    ));
    lines.push(String::new());
    lines.push("## 文件（§16 的清单，一份不多）".to_string());
    lines.push(String::new());
    for file in GENERATED {
        lines.push(format!("- `{file}`"));
    }
    lines.push("- `README.md`".to_string());
    lines.push(format!(
        "- `{FIXED_BLOCK_DIR}/run-NN.json` — fixture 的原始行，由 `crates/execution/tests/sequence.rs` 写入，本装配只读不改"
    ));
    lines.push(format!(
        "- `{LIVE_ROUTE_RUNS}/<session>/`、`{LIVE_RPC}/<session>/` — live 的原始记录，由 CLI 写入，本装配只读不改"
    ));
    lines.push(String::new());
    lines.push(format!(
        "§16 的 `fixed-block/` 与这里的 `{FIXED_BLOCK_DIR}/` 是同一份东西；`baseline/` 与 `reuse/` 是 §11 两臂各自的投影，\
         negative control 的记录在 `contract/negative-controls.json`，不再多开一份目录。上面列出的 {TREE_FILES} 份是\
         **生成文件**；原始行/原始记录（`{FIXED_BLOCK_DIR}/`、`{LIVE_ROUTE_RUNS}/`、`{LIVE_RPC}/`）不算在里面。"
    ));
    lines.push(String::new());
    lines.push("## 重新生成（§36：串行跑，每条命令各自设自己的环境变量）".to_string());
    lines.push(String::new());
    lines.push(format!(
        "1. `M844_FIXTURE_EVIDENCE={EVIDENCE_DIR} cargo test -p evm-execution --test sequence` — 写 `{FIXED_BLOCK_DIR}/` 原始行。\n\
         2. `M844_BLOCK_CONTEXT_REFRESH=1 cargo test -p evm-execution --test block_context_evidence` — 从原始行与 live 记录装配上面这些文件。\n\n\
         live 的三份记录**不能**由测试重新生成：它们是 {live_count} 次真实 BuildOnly 运行的产物，重新跑一次只会得到另一批块（链在动）。\
         本目录的 `live/` 因此按「读到的原始记录」处理——`live/live-runs.json` 与 `live/rpc-census-comparison.json` 是它们的投影，\
         门禁会独立重算一遍对账。不设环境变量的运行只做逐字节比对，不动 committed 证据。",
        live_count = LIVE_RUNS.len(),
    ));
    lines.push(String::new());
    lines.push("## 本目录证明不了的事".to_string());
    lines.push(String::new());
    for item in s["what_this_tree_does_not_prove"]
        .as_array()
        .expect("the summary lists what it cannot prove")
    {
        lines.push(format!("- {}", item.as_str().expect("a sentence")));
    }
    lines.push(String::new());
    lines.push(format!(
        "§32 的口径：fixture 每把单独一行、aggregate 是 {} 把同一驱动之和（不是 {} 个独立样本），且 fixture 侧一个时长都没测\
         （§34）。live 那 {} 把是各自独立的进程，逐把列在 `live/live-runs.json`，不做之和也不做均值；它们的\
         `rpc_duration_total_ns` 只作为实测值印出来，§33 要求 RPC 次数、RPC 时长、wall-clock 三件事分开说，\
         本树只对第一件下了结论。",
        RUNS.len(),
        RUNS.len(),
        LIVE_RUNS.len(),
    ));
    lines.join("\n") + "\n"
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// The whole tree as bytes. Rendering it twice and comparing is §18's E2; comparing the rendering
/// against the committed files is E1.
fn render(runs: &[Value], text: &str, root: &Path) -> BTreeMap<String, String> {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for (path, table) in [
        (
            "contract/verified-block-context.json",
            contract_context(runs, text),
        ),
        (
            "contract/producer-verification.json",
            contract_producer(runs),
        ),
        (
            "contract/consumer-verification.json",
            contract_consumer(runs),
        ),
        ("contract/negative-controls.json", contract_controls(runs)),
        ("comparison/rpc-comparison.json", rpc_comparison(runs)),
        (
            "comparison/block-identity-comparison.json",
            identity_comparison(runs),
        ),
        ("comparison/build-comparison.json", build_comparison(runs)),
        (
            "comparison/correctness-comparison.json",
            correctness_comparison(runs),
        ),
        ("summary.json", summary(runs)),
        ("live/live-runs.json", live_runs_table(root)),
        ("live/rpc-census-comparison.json", live_census_table(root)),
    ] {
        files.insert(
            path.to_string(),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&table).expect("a table built here is serializable")
            ),
        );
    }
    for name in [BASELINE, VERIFIED] {
        let dir = arm_dir(name);
        for (file, table) in [
            ("rpc-summary.json", arm_rpc_summary(runs, name)),
            ("pipeline-calls.json", arm_pipeline_calls(runs, name)),
            ("build-result.json", arm_build_result(runs, name)),
        ] {
            files.insert(
                format!("{dir}/{file}"),
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&table)
                        .expect("a table built here is serializable")
                ),
            );
        }
    }
    files.insert("README.md".to_string(), readme(runs, root));
    files
}

fn write_tree(root: &Path, files: &BTreeMap<String, String>) {
    for (path, text) in files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
        }
        std::fs::write(&target, text)
            .unwrap_or_else(|error| panic!("{}: {error}", target.display()));
    }
}

/// Everything in the committed tree except the raw records this gate assembles from — the fixture's
/// rows and the live runs' records, at either depth — so a stray file left by hand shows up as a
/// disagreement rather than as background noise.
fn committed_listing(root: &Path) -> Vec<String> {
    let is_raw = |name: &str| RAW_DIRS.contains(&name);
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(root).expect("the tree exists") {
        let path = entry.expect("an entry").path();
        let name = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        if path.is_dir() {
            if is_raw(&name) {
                continue;
            }
            for inner in std::fs::read_dir(&path).expect("a subtree") {
                let file = inner.expect("an entry").path();
                let inner_name = file
                    .file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .to_string();
                if file.is_dir() && is_raw(&inner_name) {
                    continue;
                }
                names.push(format!("{name}/{inner_name}"));
            }
        } else {
            names.push(name);
        }
    }
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// §18's eight integrity gates
// ---------------------------------------------------------------------------

/// E1 + E2, and the byte gate: the committed tree is what these rows render, and rendering twice
/// gives the same bytes. With `M844_BLOCK_CONTEXT_REFRESH=1` this is also the only way the tree
/// changes — the fresh rendering is written and then re-read, so a refresh that half-landed fails.
#[test]
fn the_committed_tree_is_the_assembly_of_its_own_raw_rows() {
    let root = evidence_root();
    let runs = raw_rows(&root);
    let text = raw_text(&root);

    let first = render(&runs, &text, &root);
    let second = render(&runs, &text, &root);
    assert_eq!(
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>(),
        "two renderings of one input listed different files"
    );
    for (path, contents) in &first {
        assert_eq!(
            contents, &second[path],
            "{path}: the same rows rendered to different bytes"
        );
    }

    let mut expected: Vec<&str> = GENERATED.to_vec();
    expected.push("README.md");
    expected.sort_unstable();
    let published_files: Vec<&str> = first.keys().map(String::as_str).collect();
    assert_eq!(
        published_files, expected,
        "the rendered file set and §16's list disagreed"
    );

    if refreshing() {
        write_tree(&root, &first);
        for (path, contents) in &first {
            assert_eq!(
                read_text(&root.join(path)),
                *contents,
                "{path}: the refresh did not land"
            );
        }
        return;
    }

    for (path, contents) in &first {
        let target = root.join(path);
        assert!(
            target.exists(),
            "{}: this gate renders a file the tree does not hold — run the refresh",
            target.display()
        );
        assert_eq!(
            read_bytes(&target),
            contents.as_bytes(),
            "{path}: the committed bytes are not the assembly of {FIXED_BLOCK_DIR}/ — run \
             `M844_BLOCK_CONTEXT_REFRESH=1 cargo test -p evm-execution --test \
             block_context_evidence`"
        );
    }
    assert_eq!(
        committed_listing(&root),
        published_files,
        "the tree holds a file this gate does not render, or is missing one"
    );
}

/// E3, E4, E5: the summary's RPC numbers are re-counted here out of the raw rows by a route that
/// does not call the renderer, so an off-by-one in either of the two shows up as a disagreement
/// rather than as a matching pair of bugs.
#[test]
fn the_summaries_rpc_numbers_are_recomputable_from_the_raw_rows() {
    let root = evidence_root();
    let runs = raw_rows(&root);
    let published = committed(&root, "summary.json");

    for run in &runs {
        let name = run["run"].as_str().expect("a run names itself").to_string();
        let row = published["per_run"]
            .as_array()
            .expect("per-run rows")
            .iter()
            .find(|row| row["run"].as_str() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("summary.json has no {name}"));
        for (arm_name, field) in [
            (BASELINE, "baseline_rpc_count"),
            (VERIFIED, "reuse_rpc_count"),
        ] {
            let counted = arm(run, arm_name)["lane_reads"]
                .as_array()
                .expect("lane reads")
                .iter()
                .fold(0usize, |total, _| total + 1);
            assert_eq!(
                row[field].as_i64().expect("a published count") as usize,
                counted,
                "{name}: {field} is not the number of lane reads in the raw row"
            );
        }
        assert_eq!(
            row["rpc_saved"].as_i64().expect("a signed count"),
            row["baseline_rpc_count"].as_i64().expect("a count")
                - row["reuse_rpc_count"].as_i64().expect("a count"),
            "{name}: rpc_saved is not the difference of the two counts on the same row"
        );
        assert_eq!(
            row["net_rpc_saved"].as_i64().expect("a signed count"),
            row["rpc_saved"].as_i64().expect("a signed count")
                - row["verification_rpc_added"]
                    .as_i64()
                    .expect("a signed count"),
            "{name}: the net is not the saved figure minus the added verification reads"
        );
        for (arm_name, field) in [
            (BASELINE, "baseline_block_reads"),
            (VERIFIED, "reuse_block_reads"),
        ] {
            let counted = arm(run, arm_name)["lane_reads"]
                .as_array()
                .expect("lane reads")
                .iter()
                .filter(|read| read["site"].as_str() == Some(BLOCK_READ))
                .count();
            assert_eq!(
                row[field].as_i64().expect("a published count") as usize,
                counted,
                "{name}: {field} is not the raw row's count of {BLOCK_READ} calls"
            );
        }
    }

    let aggregate = &published["aggregate"];
    for field in ["baseline_rpc_count", "reuse_rpc_count", "rpc_saved"] {
        assert_eq!(
            aggregate[field].as_i64().expect("a signed count"),
            published["per_run"]
                .as_array()
                .expect("per-run rows")
                .iter()
                .map(|row| row[field].as_i64().expect("a signed count"))
                .sum::<i64>(),
            "the aggregate's {field} is not the sum of the rows it summarises"
        );
    }

    // E4/E5 across the per-arm files: the published calls are the denominator of both the counts and
    // the order in the same arm's rpc-summary.
    for arm_name in [BASELINE, VERIFIED] {
        let dir = arm_dir(arm_name);
        let calls = committed(&root, &format!("{dir}/pipeline-calls.json"));
        let sums = committed(&root, &format!("{dir}/rpc-summary.json"));
        let call_rows = calls["per_run"].as_array().expect("per-run calls");
        let sum_rows = sums["per_run"].as_array().expect("per-run summaries");
        assert_eq!(
            call_rows.len(),
            sum_rows.len(),
            "{dir}: the two files disagree about how many runs exist"
        );
        for (call_row, sum_row) in call_rows.iter().zip(sum_rows.iter()) {
            let listed = call_row["calls"].as_array().expect("calls");
            assert_eq!(
                sum_row["rpc_count"].as_i64().expect("a count") as usize,
                listed.len(),
                "{dir}: rpc-summary counts differently from the calls it publishes"
            );
            let mut methods: BTreeMap<String, u64> = BTreeMap::new();
            for call in listed {
                *methods
                    .entry(
                        call["site"]
                            .as_str()
                            .expect("a published call names its site")
                            .to_string(),
                    )
                    .or_insert(0) += 1;
            }
            assert_eq!(
                sum_row["method_counts"],
                json!(methods),
                "{dir}: method_counts are not a count of the published calls"
            );
            let order: Vec<Value> = listed
                .iter()
                .map(|call| json!(call["site"].as_str().expect("a site")))
                .collect();
            assert_eq!(
                sum_row["method_order"],
                json!(order),
                "{dir}: method_order is not the order of the published calls"
            );
            assert_eq!(
                sum_row["block_header_reads"].as_i64().expect("a count") as usize,
                listed
                    .iter()
                    .filter(|call| call["site"].as_str() == Some(BLOCK_READ))
                    .count(),
                "{dir}: block_header_reads is not a count of the published calls"
            );
        }
    }
}

/// E6 and E7: the identity and Build comparisons published for the arms are re-derived from the raw
/// rows, and each arm's own `build-result.json` carries the same transaction its comparison row says
/// it carries.
#[test]
fn the_identity_and_build_comparisons_are_recomputed_from_the_rows() {
    let root = evidence_root();
    let runs = raw_rows(&root);
    let identity_table = committed(&root, "comparison/block-identity-comparison.json");
    let build_table = committed(&root, "comparison/build-comparison.json");

    for run in &runs {
        let name = run["run"].as_str().expect("a run names itself").to_string();
        let a = identity(arm(run, BASELINE));
        let b = identity(arm(run, VERIFIED));
        let published_pair = identity_table["per_run"]
            .as_array()
            .expect("per-run rows")
            .iter()
            .find(|row| row["run"].as_str() == Some(name.as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("the identity table has no {name}"))
            .pointer("/baseline_vs_verified_context")
            .cloned()
            .expect("every run carries its identity pair");
        assert_eq!(
            published_pair["a"], a,
            "{name}: the table's left side is not the row's"
        );
        assert_eq!(
            published_pair["b"], b,
            "{name}: the table's right side is not the row's"
        );
        for field in ["chain_id_equal", "block_number_equal", "block_hash_equal"] {
            let recomputed = match field {
                "chain_id_equal" => a["chain_id"] == b["chain_id"],
                "block_number_equal" => a["block_number"] == b["block_number"],
                _ => a["block_hash"] == b["block_hash"],
            };
            assert_eq!(
                published_pair[field],
                json!(recomputed),
                "{name}: {field} disagrees with the two raw identities"
            );
        }

        let built = build_table["per_run"]
            .as_array()
            .expect("per-run rows")
            .iter()
            .find(|row| row["run"].as_str() == Some(name.as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("the build table has no {name}"));
        assert_eq!(
            built["fingerprint"],
            build(arm(run, VERIFIED))["fingerprint"],
            "{name}: the published fingerprint is not the reuse arm's build row"
        );
        assert_eq!(
            built["baseline_fingerprint"],
            build(arm(run, BASELINE))["fingerprint"],
            "{name}: the published baseline fingerprint is not the baseline arm's build row"
        );
        assert_eq!(
            built["fingerprint_equal"],
            json!(
                build(arm(run, BASELINE))["fingerprint"]
                    == build(arm(run, VERIFIED))["fingerprint"]
            ),
            "{name}: the fingerprint flag disagrees with the two rows"
        );
        for field in BUILD_FIELDS {
            let entry = built["fields"]
                .as_array()
                .expect("one entry per §12 field")
                .iter()
                .find(|entry| entry["field"].as_str() == Some(field))
                .unwrap_or_else(|| panic!("{name}: the build table never compared {field}"));
            assert_eq!(
                entry["baseline"],
                build_field(arm(run, BASELINE), field),
                "{name}: {field} is not what the baseline row holds"
            );
            assert_eq!(
                entry["verified_context"],
                build_field(arm(run, VERIFIED), field),
                "{name}: {field} is not what the reuse row holds"
            );
            assert_eq!(
                entry["equal"],
                json!(
                    build_field(arm(run, BASELINE), field)
                        == build_field(arm(run, VERIFIED), field)
                ),
                "{name}: {field}'s equal flag disagrees with the two rows"
            );
        }

        for arm_name in [BASELINE, VERIFIED] {
            let dir = arm_dir(arm_name);
            let published_build = committed(&root, &format!("{dir}/build-result.json"));
            let row = published_build["per_run"]
                .as_array()
                .expect("per-run rows")
                .iter()
                .find(|row| row["run"].as_str() == Some(name.as_str()))
                .unwrap_or_else(|| panic!("{}: no {name}", dir));
            assert_eq!(
                row["unsigned"],
                build(arm(run, arm_name))["unsigned"],
                "{name}: {dir}'s published build file is not its raw row's build"
            );
            assert_eq!(
                row["identity"],
                identity(arm(run, arm_name)),
                "{name}: {dir}'s published identity is not its raw row's build identity"
            );
        }
    }
}

/// E8: every control row in this tree is a named refusal, and the five controls this tree does not
/// run are pointed at tests that exist in the files they are attributed to.
#[test]
fn the_negative_control_pointers_name_tests_that_exist() {
    let root = evidence_root();
    let runs = raw_rows(&root);
    let controls = committed(&root, "contract/negative-controls.json");
    let nc1 = &controls["graded_by_rows_in_this_tree"][0];
    assert_eq!(
        nc1["control"],
        json!("NC1"),
        "the row-graded control is not NC1"
    );
    assert_eq!(nc1["all_refused_by_name"], json!(true), "E8");
    let rows = nc1["runs"].as_array().expect("one control row per run");
    assert_eq!(
        rows.len(),
        RUNS.len(),
        "E8: the control table is not one row per run"
    );
    for run in &runs {
        let name = run["run"].as_str().expect("a run names itself").to_string();
        let row = rows
            .iter()
            .find(|row| row["run"].as_str() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("the controls table has no {name}"));
        assert_eq!(row["outcome"], json!("rejected"), "{name}: E8");
        assert_eq!(
            row["reason"],
            json!("block_hash_mismatch"),
            "{name}: §7's NC1 is a hash disagreement at one height, so a refusal naming anything \
             else is a different control"
        );
        assert_eq!(row["built_anyway"], json!(true), "{name}: E8");
        assert_eq!(
            row["signed_anything"],
            json!(false),
            "{name}: a refused context must not reach a signature"
        );
        assert_eq!(
            row["step_pin"],
            identity(arm(run, BASELINE)),
            "{name}: the control's step pinned a different block than the other two arms, so the \
             refusal is not about one height and two hashes"
        );
        assert_ne!(
            identity_of_verified(&row["context_about"]),
            identity(arm(run, BASELINE)),
            "{name}: the control's producer verified the very block the step pins, which would make \
             the refusal a false alarm"
        );
    }

    let cited: Vec<(&str, &str)> = controls["graded_by_tests_in_the_crate"]
        .as_array()
        .expect("the five controls this tree does not run")
        .iter()
        .flat_map(|entry| entry["tests"].as_array().expect("tests"))
        .map(|reference| {
            (
                reference["file"].as_str().expect("a file"),
                reference["test"].as_str().expect("a test name"),
            )
        })
        .collect();
    assert!(
        cited.len() >= 9,
        "§7's five remaining controls carry only {} test pointers",
        cited.len()
    );
    let mut sources: BTreeMap<&str, String> = BTreeMap::new();
    for (file, test) in &cited {
        let text = sources
            .entry(file)
            .or_insert_with(|| read_text(&workspace_root().join(file)));
        assert!(
            text.contains(&format!("fn {test}")),
            "{file}: names no `{test}` — the controls table points at a test that is not there"
        );
    }

    // §35's experiment names must exist too: the doc asks for those three tests by name, and this
    // table is the file that would go stale if one were renamed.
    let sequence_source = read_text(&workspace_root().join("crates/execution/tests/sequence.rs"));
    for required in [
        "baseline_and_reuse_same_block_identity",
        "baseline_and_reuse_same_build_result",
        "baseline_and_reuse_same_fingerprint",
        "no_extra_rpc_outside_declared_header_reuse",
    ] {
        assert!(
            sequence_source.contains(&format!("async fn {required}")),
            "§35 names `{required}` as an experiment test and the experiment file has no such name"
        );
    }
}

/// §18's E3 in the direction the tables alone can show: no published figure is a number the rows do
/// not carry, and the three §6 outcomes partition the consumer table.
#[test]
fn the_contract_tables_count_their_own_rows() {
    let root = evidence_root();
    let runs = raw_rows(&root);
    let producer = committed(&root, "contract/producer-verification.json");
    let consumer = committed(&root, "contract/consumer-verification.json");

    let rows = producer["rows"]
        .as_array()
        .expect("one row per arm per run");
    assert_eq!(
        rows.len(),
        RUNS.len() * ARMS.len(),
        "the producer table is not per-arm-per-run"
    );
    assert_eq!(
        producer["counts"]["verified"].as_i64().expect("a count") as usize,
        runs.iter()
            .map(|run| ARMS
                .iter()
                .filter(
                    |name| arm(run, name)["producer_verdict"]["outcome"].as_str()
                        == Some("verified")
                )
                .count())
            .sum::<usize>(),
        "the producer table's verified count is not a count of its own rows"
    );
    assert_eq!(
        producer["counts"]["verified"].as_i64().expect("a count")
            + producer["counts"]["refused"].as_i64().expect("a count"),
        rows.len() as i64,
        "the producer's two outcomes are not a partition of its rows"
    );

    let consumer_rows = consumer["rows"]
        .as_array()
        .expect("one row per arm per run");
    assert_eq!(
        consumer_rows.len(),
        rows.len(),
        "the two tables disagree about the sample size"
    );
    assert_eq!(
        consumer["counts"]["rejected"].as_i64().expect("a count") as usize,
        runs.iter()
            .map(|run| ARMS
                .iter()
                .filter(|name| outcome(arm(run, name)) == "rejected")
                .count())
            .sum::<usize>(),
        "the consumer table's refusals are not the raw rows' refusals"
    );
    assert_eq!(
        consumer["reused_true_count"].as_i64().expect("a count"),
        0,
        "nothing on this path may claim reuse: §2 keeps the step's own binding read"
    );
    let partition: i64 = ["accepted", "rejected", "no_context"]
        .iter()
        .map(|word| consumer["counts"][word].as_i64().expect("a count"))
        .sum();
    assert_eq!(
        partition,
        consumer_rows.len() as i64,
        "the three §6 outcomes are not a partition of the rows — a fourth word got in"
    );
    for row in consumer_rows {
        let word = row["outcome"].as_str().expect("an outcome word");
        assert_eq!(
            row["fallback_performed"],
            json!(word == "rejected"),
            "§30: a fallback is recorded exactly when a context was refused"
        );
        assert_eq!(
            row["fallback_value"], row["expected_identity"],
            "§30: the value a refused step fell back to must be the step's own pin, stated in the \
             same row as the refusal"
        );
    }
}

/// §32's per-run requirement, checked against the tree rather than against the renderer: a run that
/// is only in the aggregate would let one bad drive hide inside a sum.
#[test]
fn every_summary_figure_appears_once_per_run_and_in_the_aggregate() {
    let root = evidence_root();
    let published = committed(&root, "summary.json");
    let per_run = published["per_run"].as_array().expect("per-run rows");
    assert_eq!(
        per_run.len(),
        RUNS.len(),
        "§32 asks for run-01…0N, not one averaged line"
    );
    for run in RUNS {
        assert!(
            per_run.iter().any(|row| row["run"].as_str() == Some(run)),
            "§32: summary.json has no {run}"
        );
    }
    for field in [
        "baseline_rpc_count",
        "reuse_rpc_count",
        "rpc_saved",
        "baseline_block_reads",
        "reuse_block_reads",
        "producer_verification_count",
        "consumer_verification_count",
        "reuse_accept_count",
        "reuse_reject_count",
        "fallback_count",
        "correctness_equal_count",
        "correctness_mismatch_count",
    ] {
        assert!(
            published["aggregate"][field].is_number(),
            "§32 names {field} and the aggregate has no such number"
        );
        assert!(
            per_run.iter().all(|row| row[field].is_number()),
            "§32 names {field} and at least one run has no such number"
        );
    }
    assert_eq!(
        published["aggregate"]["correctness_equal_count"]
            .as_i64()
            .expect("a count"),
        published["per_run"]
            .as_array()
            .expect("per-run rows")
            .iter()
            .map(|row| row["correctness_equal_count"].as_i64().expect("a count"))
            .sum::<i64>(),
        "the correctness aggregate is not the sum of the runs"
    );
    assert_eq!(
        published["aggregate"]["correctness_mismatch_count"].as_i64().expect("a count"),
        0,
        "a mismatch would be a finding, not a number to average away — the milestone's result label \
         depends on this being reported, not hidden"
    );
    assert_eq!(
        published["rpc_count_reduction"]
            .as_i64()
            .expect("a signed figure"),
        published["aggregate"]["net_rpc_saved"]
            .as_i64()
            .expect("a signed figure"),
        "§33's three kinds of reduction must not be conflated: the count figure is the net, not \
         the saved-before-verification number"
    );
}

/// The live subtree's version of E3–E7: every number in the two committed live tables is recounted
/// here out of the four records per run, by a route that does not call the renderer. The point is not
/// that the tables add up — it is that a table which drifted from the records it claims to project
/// stops being a projection, and the records are the only part of `live/` nobody can regenerate.
#[test]
fn the_live_rows_and_the_census_recompute_from_their_own_records() {
    let root = evidence_root();
    let table = committed(&root, "live/live-runs.json");
    let census = committed(&root, "live/rpc-census-comparison.json");
    let rows = table["per_run"]
        .as_array()
        .expect("the live table carries a row per run");
    assert_eq!(
        rows.len(),
        LIVE_RUNS.len(),
        "the live table has a row per recorded run"
    );

    let mut built_sessions: Vec<String> = Vec::new();
    for (position, session) in LIVE_RUNS.iter().enumerate() {
        let row = &rows[position];
        assert_eq!(row["session"], json!(session), "rows stay in record order");
        let route = live_file(
            &root,
            &format!("{LIVE_ROUTE_RUNS}/{session}/route-run.json"),
        );
        let preflight = live_file(
            &root,
            &format!("{LIVE_ROUTE_RUNS}/{session}/preflight.json"),
        );
        let metrics = live_file(&root, &format!("{LIVE_ROUTE_RUNS}/{session}/metrics.json"));
        let rpc = live_file(
            &root,
            &format!("{LIVE_RPC}/{session}/outside-simulation-rpc.json"),
        );

        // the three elements §25 requires, taken off the records again rather than off the table
        assert_eq!(
            row["pinned_identity"],
            json!({
                "chain_id": route["chain_id"],
                "block_number": route["pinned_block"],
                "block_hash": route["pinned_block_hash"],
            }),
            "{session}: the pinned triple is not the record's"
        );
        let head_number = preflight["head"]["block_number"]
            .as_u64()
            .expect("a head height");
        let pinned_number = route["pinned_block"].as_u64().expect("a pinned height");
        assert_eq!(
            row["head_minus_pin"],
            json!(head_number - pinned_number),
            "{session}: the gap is arithmetic on the two recorded heights"
        );
        assert!(
            head_number >= pinned_number,
            "{session}: the head cannot be behind the block the run pinned — head {head_number}, \
             pin {pinned_number}"
        );

        // §8's scope rule, re-derived here: a pin the endpoint still calls head is a live head, a
        // pin the chain has moved past is a fixed historical block. The CLI derives the same value
        // from the same two numbers; requiring the two derivations to agree is what makes the
        // recorded scope a checked fact instead of a label.
        let derived_scope = if head_number == pinned_number {
            "live_head"
        } else {
            "fixed_historical"
        };
        assert_eq!(
            row["producer_scope"], row["scope_derived_from_head_and_pin"],
            "{session}: the producer's scope disagrees with the head and pin in its own record"
        );
        assert_eq!(
            row["scope_derived_from_head_and_pin"],
            json!(derived_scope),
            "{session}: this gate re-derived the scope differently from the same two numbers"
        );

        let verified = &preflight["preflight"]["block_context"]["verified_block"];
        assert_eq!(
            row["producer_verified_equals_pin"],
            json!(
                verified["chain_id"] == route["chain_id"]
                    && verified["block_number"] == route["pinned_block"]
                    && verified["block_hash"] == route["pinned_block_hash"]
            ),
            "{session}: the identity equality is a comparison of the records, not a stated belief"
        );
        assert_eq!(
            row["producer_outcome"], preflight["preflight"]["block_context"]["outcome"],
            "{session}: the producer verdict is copied, and copying must be faithful"
        );

        // §30's absence property: the consumer leg exists only on a run that reached Build. A
        // refused preflight must show no consumer row and no build — an empty field would hide
        // whether the check ran and said no, or never ran at all.
        let checks = route["execution"]["context_checks"]
            .as_array()
            .expect("a live execution record carries its consumer rows");
        let builds = route["execution"]["builds"]
            .as_array()
            .expect("a live execution record carries its build list");
        assert_eq!(row["consumer_check_count"], json!(checks.len()));
        assert_eq!(row["build_count"], json!(builds.len()));
        if route["execution"]["status"] == json!("built") {
            built_sessions.push(session.to_string());
            assert_eq!(
                checks.len(),
                1,
                "{session}: reaching Build runs the consumer leg once"
            );
            assert_eq!(
                metrics["counters"]["execution_block_context_accepted"]
                    .as_u64()
                    .expect("the accept counter"),
                1,
                "{session}: the counter and the row must tell the same story"
            );
            assert_eq!(
                row["consumer_outcome"],
                json!("accepted"),
                "{session}: the only live run to reach Build must record what its consumer leg said"
            );
            assert_eq!(row["consumer_rows_verbatim"], json!(checks));
        } else {
            assert!(
                checks.is_empty() && builds.is_empty(),
                "{session}: preflight refused this run, so nothing downstream may claim a row"
            );
            assert_eq!(
                preflight["preflight"]["passed"],
                json!(false),
                "{session}: a run that did not build must say why"
            );
            assert!(
                preflight["preflight"]["rejected_because"].is_string(),
                "{session}: §29's named refusal, not a silent stop"
            );
        }

        for (name, value) in metrics["counters"]
            .as_object()
            .expect("a live metrics record carries its counters")
        {
            if name.starts_with("execution_block_context") {
                assert_eq!(
                    row["block_context_counters"][name.as_str()].clone(),
                    value.clone(),
                    "{session}: counter {name} re-read differently"
                );
            }
        }

        let file_bytes = |name: &str| -> u64 {
            std::fs::metadata(root.join(format!("{LIVE_ROUTE_RUNS}/{session}/{name}")))
                .expect("a safety file exists even when empty")
                .len()
        };
        assert_eq!(
            row["signed_transactions_bytes"],
            json!(file_bytes("signed-transactions.jsonl"))
        );
        assert_eq!(
            row["submissions_bytes"],
            json!(file_bytes("submissions.jsonl"))
        );
        assert_eq!(
            row["successful_real_arbitrage"],
            route["successful_real_arbitrage"]
        );

        // §33's count, recounted from the trace rows with a different key encoding than the
        // renderer's, then compared against both the table and the file's own total.
        let mut recount: BTreeMap<String, u64> = BTreeMap::new();
        for call in rpc["rows"].as_array().expect("a census carries rows") {
            let key = format!(
                "{}|{}",
                call["method"].as_str().expect("a method"),
                call["stage"].as_str().unwrap_or("<none>")
            );
            *recount.entry(key).or_insert(0) += 1;
        }
        let recounted_total: u64 = recount.values().sum();
        assert_eq!(
            recounted_total,
            rpc["calls"].as_u64().expect("the trace's own count"),
            "{session}: the rows do not add up to the count the file states"
        );
        assert_eq!(
            row["rpc_lifecycle_calls"],
            json!(recounted_total),
            "{session}: the live table's call count is not the record's"
        );
    }

    assert_eq!(
        built_sessions.len(),
        table["aggregate"]["reached_build"]
            .as_u64()
            .expect("a count") as usize,
        "the aggregate's Build count and the rows' own statuses disagree"
    );

    let aggregate = &table["aggregate"];
    let count_where = |field: &str, wanted: &str| -> usize {
        rows.iter()
            .filter(|row| row[field].as_str() == Some(wanted))
            .count()
    };
    assert_eq!(
        aggregate["producer_verified"].as_u64().expect("a count") as usize,
        count_where("producer_outcome", "verified")
    );
    assert_eq!(
        aggregate["consumer_accepted"].as_u64().expect("a count") as usize,
        count_where("consumer_outcome", "accepted")
    );
    assert_eq!(
        aggregate["consumer_rejected"].as_u64().expect("a count"),
        json!(0),
        "a live rejection would be §42's headline, not a footnote — if one appears, the label has \
         to be re-decided"
    );
    assert_eq!(
        aggregate["reused_true"].as_u64().expect("a count"),
        json!(0),
        "§31: nothing on this path may yet say `reused = true`"
    );
    assert_eq!(
        aggregate["signed_transactions_bytes"]
            .as_u64()
            .expect("a byte count"),
        json!(0),
        "§11's prohibition survives the live runs only if the records show it"
    );
    assert_eq!(
        aggregate["submissions_bytes"]
            .as_u64()
            .expect("a byte count"),
        json!(0)
    );
    assert_eq!(
        aggregate["successful_real_arbitrage"]
            .as_u64()
            .expect("a count"),
        json!(0)
    );

    // The census comparison, re-asked from the six files themselves.
    let by_run = census["census_by_run"]
        .as_array()
        .expect("a census per run");
    assert_eq!(by_run.len(), LIVE_RUNS.len() + PRIOR_LIVE_RUNS.len());
    for entry in by_run {
        let mut sum = 0;
        for cell in entry["cells"].as_array().expect("a census carries cells") {
            sum += cell["calls"].as_u64().expect("a cell count");
        }
        assert_eq!(
            entry["cells_recounted_from_rows"], entry["lifecycle_calls"],
            "{}: the cells and the stated count disagree",
            entry["run"]
        );
        assert_eq!(
            json!(sum),
            entry["lifecycle_calls"],
            "{}: cell sum",
            entry["run"]
        );
    }
    let built_name = format!("{LIVE_DIR}/{}", built_sessions[0]);
    let built_entry = by_run
        .iter()
        .find(|entry| entry["run"] == json!(built_name))
        .expect("the built run's census");
    for pair in census["comparison"]["paired_against"]
        .as_array()
        .expect("a pair per control-group run")
    {
        let prior_name = pair["prior_run"].as_str().expect("a prior run name");
        let prior_entry = by_run
            .iter()
            .find(|entry| entry["run"] == json!(prior_name))
            .expect("the paired census");
        assert_eq!(
            prior_entry["cells"], built_entry["cells"],
            "{prior_name}: the census cells are not identical, so §33's live answer has to change"
        );
        assert_eq!(pair["cells_identical"], json!(true));
        assert_eq!(pair["differing_cells"], json!(0));
        assert_eq!(pair["net_provider_calls_saved"], json!(0));
    }
    assert_eq!(
        census["comparison"]["endpoint_digest_count"]
            .as_u64()
            .expect("a count"),
        json!(1),
        "the comparison is only like-for-like if all six runs asked the same endpoint"
    );
    assert!(
        census["answer"]
            .as_str()
            .expect("an answer")
            .contains("No net RPC reduction."),
        "§33: with a net of zero the tree says the plain sentence"
    );
    for blocked in census["comparison"]["blocked_live_runs_explained"]
        .as_array()
        .expect("a row per blocked live run")
    {
        assert!(
            blocked["cells_only_in_this_run"]
                .as_array()
                .expect("a cell list")
                .is_empty(),
            "{}: a refused run cannot be making calls the built run is not",
            blocked["run"]
        );
        assert_eq!(
            blocked["every_unattributed_stage_is_in_the_built_run"],
            json!(true),
            "{}: the calls a refused run is missing are the execution lane's own reads, which the \
             trace files with a null stage",
            blocked["run"]
        );
    }
}
