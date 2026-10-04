//! M8.5.1 §34/§37's evidence tree: `data/evidence/m8/m8.5.1/`.
//!
//! # What this directory is for
//!
//! §66 asks one question and the whole tree exists to answer it: 「如果现在把 Opportunity Detection
//! 的 `eth_call` 结果交给 Preflight，Preflight 为什么能或者不能相信这个结果？」 Seven tables answer
//! it — what was asked, how each ask is identified, who holds each answer, how each answer lives,
//! whether the block changes the answer, what would have to change for that to be a false
//! conclusion, and what the verdict is per site and per measured pair.
//!
//! # Nothing here was measured by asking the node again
//!
//! §31 and §32 forbid an instrumentation read, so every figure in every table is recomputed from
//! records this repository already commits:
//!
//! - `data/evidence/m8/cross-stage/runs/*/pipeline-calls.json` — the asks, their stages, callers,
//!   targets and block terms ([`evm_pipeline::eth_call_semantics::RAW_RUNS_GLOB`]);
//! - `data/evidence/m8/cross-stage/reuse-candidates.json` — the 18 directed pairs M8.4.2 counted;
//! - `data/evidence/m8/cross-stage/route-runs/*/` — the *decoded* answers, which is the only place
//!   §42's warning can be checked at the artifact layer rather than the wire layer;
//! - `data/evidence/m7/candidate-fee-measurement.json` — the same pools read at a much earlier
//!   height, which supplies §41's key control without a new call;
//! - `data/evidence/m7/live-pool-activity.json` — the swap/sync census that bounds how far that
//!   equality may be read as independence;
//! - `fixtures/simulation-m7/dump-*.json` — contract bytecode already on disk, so §17's opcode
//!   question is answerable offline.
//!
//! Opening a socket would have been the fastest way to get these numbers and the one way to break
//! §31. It is not used: this file makes no request, holds no client, and the production path is
//! unchanged (§56), which [`the_diagnosis_is_a_reader_and_not_a_writer`] grades.
//!
//! # How these files change
//!
//! Assembly happens under `target/pipeline-tests/`; `M851_ETH_CALL_SEMANTICS_REFRESH=1` copies a
//! fresh assembly over the committed directory, which is the only way these eight files change.
//! Anchor line numbers are resolved out of the source files at assembly time, so a line number in
//! the evidence is measured rather than copied: when the code moves, the byte gate fails until the
//! tables are refreshed from the code that now holds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use evm_pipeline::canonicalization::RpcReadKey;
use evm_pipeline::eth_call_semantics::{
    block_context_reachable_in_code, call_sites, lifecycle_rows, opcode_inventory, verdicts,
    ArtifactClass, BlockDependency, CallSite, CarrierForm, EthCallIdentity, FieldEvidence,
    ReuseBlocker, ReuseClass, ALL_AXES, BLOCK_CONTEXT_OPCODES, ETH_CALL_FILES, EVIDENCE_DIR,
    LIFECYCLE_STEPS, NEGATIVE_CONTROLS, REQUEST_TYPE_FILE, REQUEST_TYPE_TOKEN, RPC_BOUNDARY_FILE,
    RPC_BOUNDARY_TOKEN, SELECTORS, STATE_OPCODES, UNKNOWN_SELECTOR,
};
use evm_pipeline::state_ownership::{Anchor, ProofStatus, SourceKind};

// ---------------------------------------------------------------------------
// paths and the words that name them
// ---------------------------------------------------------------------------

/// The records directory this milestone reads but never writes; §34's own output directory is
/// `EVIDENCE_DIR`, taken from the model so the two cannot disagree about where the tables live.
const RUNS_DIR: &str = "data/evidence/m8/cross-stage/runs";
const ROUTE_RUNS_DIR: &str = "data/evidence/m8/cross-stage/route-runs";
const CALLS_FILE: &str = "pipeline-calls.json";
const ROUTE_RUN_FILE: &str = "route-run.json";
const PREFLIGHT_FILE: &str = "preflight.json";
const CANDIDATES_RECORD: &str = "data/evidence/m8/cross-stage/reuse-candidates.json";
const SUMMARY_RECORD: &str = "data/evidence/m8/cross-stage/duplicate-summary.json";
const FEE_MEASUREMENT_RECORD: &str = "data/evidence/m7/candidate-fee-measurement.json";
const ACTIVITY_RECORD: &str = "data/evidence/m7/live-pool-activity.json";
const BYTECODE_DIR: &str = "fixtures/simulation-m7";
const MODEL_FILE: &str = "crates/pipeline/src/eth_call_semantics.rs";
const WRITER: &str = "crates/pipeline/tests/eth_call_semantics_evidence.rs";
const README_FILE: &str = "README.md";

/// The two pools §41's control is measured on. Both are read at the pin, at the gate's head, and
/// at a height 170,153 blocks earlier, so the same ask has three recorded answers in this
/// repository and none of them was produced for this milestone.
const POOL_A: &str = "0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4";
const POOL_B: &str = "0x5b3c1e3fb6a97c0130ae015ff10f53a1a30c353e";
const EARLY_HEIGHT: &str = "37530593";

/// `M851_ETH_CALL_SEMANTICS_REFRESH` names no directory: its presence is the instruction to copy a
/// fresh assembly over the committed evidence.
fn refreshing() -> bool {
    std::env::var_os("M851_ETH_CALL_SEMANTICS_REFRESH").is_some()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn evidence_dir() -> PathBuf {
    workspace_root().join(EVIDENCE_DIR)
}

fn repo_path(relative: &str) -> PathBuf {
    workspace_root().join(relative)
}

fn scratch(name: &str) -> PathBuf {
    let dir = workspace_root().join("target/pipeline-tests").join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    }
    std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    dir
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

/// Two-space pretty-printing and a trailing newline, as every other evidence file here holds it.
/// `serde_json` without `preserve_order` sorts object keys, which is what makes a re-assembly
/// byte-reproducible.
fn write_table(path: &Path, table: &Value) {
    let mut text = serde_json::to_string_pretty(table).unwrap_or_else(|error| panic!("{error}"));
    text.push('\n');
    write_text(path, &text);
}

fn write_text(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

fn generated_files() -> Vec<String> {
    let mut names: Vec<String> = ETH_CALL_FILES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    names.push(README_FILE.to_string());
    names.sort();
    names
}

fn subdirectories(dir: &str) -> Vec<String> {
    let root = repo_path(dir);
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The seven files' root, read back the way the README and the gates need it.
fn read_root(dir: &Path) -> Value {
    let mut tables = serde_json::Map::new();
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    for name in names {
        if !name.ends_with(".json") {
            continue;
        }
        let stem = name.trim_end_matches(".json").to_string();
        tables.insert(stem, read_json(&dir.join(name)));
    }
    Value::Object(tables)
}

fn rows_of(table: &Value) -> Vec<Value> {
    table["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("table has no `rows` array"))
}

// ---------------------------------------------------------------------------
// anchors: a claim resolves to one place, or the claim fails
// ---------------------------------------------------------------------------

struct Tree {
    lines: BTreeMap<String, Vec<String>>,
}

impl Tree {
    fn new() -> Self {
        Tree {
            lines: BTreeMap::new(),
        }
    }

    fn read(&mut self, file: &str) -> &Vec<String> {
        self.lines.entry(file.to_string()).or_insert_with(|| {
            read_text(&repo_path(file))
                .lines()
                .map(str::to_string)
                .collect()
        })
    }
}

/// The end of a file's production region: the index of its first `#[cfg(test)]` line. A production
/// claim resolved against a line of test code would be a claim about a test.
fn production_end(lines: &[String]) -> usize {
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return index;
        }
    }
    lines.len()
}

fn matches(lines: &[String], limit: usize, token: &str) -> Vec<usize> {
    lines[..limit]
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(token))
        .map(|(index, _)| index + 1)
        .collect()
}

fn reference(
    file: &str,
    token: &str,
    source: &str,
    line: usize,
    matches: usize,
    note: &str,
) -> Value {
    json!({
        "file": file,
        "token": token,
        "source": source,
        "line": line,
        "matches": matches,
        "note": note,
    })
}

/// One anchor, resolved. `code` and `test` demand exactly one matching line; `run_record` demands a
/// committed file that contains the token and may match more than once.
fn resolve(anchor: &Anchor, tree: &mut Tree) -> Value {
    let all = tree.read(anchor.file);
    let limit = match anchor.source {
        SourceKind::Code => production_end(all),
        _ => all.len(),
    };
    let hits = matches(all, limit, anchor.token);
    match anchor.source {
        SourceKind::Code | SourceKind::Test => assert_eq!(
            hits.len(),
            1,
            "{}: the token {:?} matches {} lines of {} — an anchor has to name one place, and two \
             matches mean the claim does not say which line it rests on",
            anchor.note,
            anchor.token,
            hits.len(),
            anchor.file
        ),
        SourceKind::RunRecord => assert!(
            !hits.is_empty(),
            "{}: {:?} does not appear in {} — a run-record anchor points into committed evidence, \
             and a pointer to a field the record does not carry is a claim about a measurement \
             that was never taken",
            anchor.note,
            anchor.token,
            anchor.file
        ),
    }
    reference(
        anchor.file,
        anchor.token,
        anchor.source.as_str(),
        hits[0],
        hits.len(),
        anchor.note,
    )
}

fn resolve_all(anchors: &[Anchor], tree: &mut Tree) -> Vec<Value> {
    anchors.iter().map(|anchor| resolve(anchor, tree)).collect()
}

/// Resolution for a token the model cannot hold in an `Anchor` because it is runtime data: a
/// recorded ask's own dedup key, which is what lets a reader open the file and count it.
fn resolve_record_token(file: &str, token: &str, note: &str, tree: &mut Tree) -> Value {
    let all = tree.read(file);
    let hits = matches(all, all.len(), token);
    assert!(
        !hits.is_empty(),
        "{token:?} does not appear in {file} — a measured row has to point back at the record it \
         was read out of"
    );
    reference(
        file,
        token,
        SourceKind::RunRecord.as_str(),
        hits[0],
        hits.len(),
        note,
    )
}

// ---------------------------------------------------------------------------
// the raw records every table is recomputed from
// ---------------------------------------------------------------------------

/// One recorded `eth_call` ask: the row as published, plus the identity this milestone reads out
/// of it through M8.4.2's own key builder.
struct Ask {
    run: String,
    row: Value,
    key: RpcReadKey,
    identity: EthCallIdentity,
}

fn chain_id_of(run: &str) -> u64 {
    run.split('-')
        .nth(1)
        .and_then(|field| field.parse().ok())
        .unwrap_or_else(|| panic!("{run}: the run directory does not name a chain"))
}

fn asks() -> Vec<Ask> {
    let mut found = Vec::new();
    for run in subdirectories(RUNS_DIR) {
        let path = repo_path(RUNS_DIR).join(&run).join(CALLS_FILE);
        let record = read_json(&path);
        let chain_id = chain_id_of(&run);
        let rows = record["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{path:?} has no `rows` array"));
        for row in rows {
            if row["method"].as_str() != Some("eth_call") {
                continue;
            }
            let key = RpcReadKey::of_row(row, Some(chain_id));
            let identity = EthCallIdentity::of_row(&key)
                .unwrap_or_else(|| panic!("{}: an eth_call row with no identity", row["rpc_id"]));
            found.push(Ask {
                run: run.clone(),
                row: row.clone(),
                key,
                identity,
            });
        }
    }
    found.sort_by(|a, b| {
        (a.run.clone(), a.row["rpc_id"].as_u64().unwrap_or_default())
            .cmp(&(b.run.clone(), b.row["rpc_id"].as_u64().unwrap_or_default()))
    });
    found
}

/// Every ask recorded, under any method — the denominator §39's `eth_call` counts are read \
/// against, so a surface figure is never confused with the run's whole traffic.
fn total_asks() -> usize {
    subdirectories(RUNS_DIR)
        .iter()
        .map(|run| {
            let record = read_json(&repo_path(RUNS_DIR).join(run).join(CALLS_FILE));
            record["rows"]
                .as_array()
                .map(|rows| rows.len())
                .unwrap_or_default()
        })
        .sum()
}

/// The site this build makes an ask from, decided by the recorded stage and the selector the
/// record carries — the same two fields [`CallSite`] declares, so a fifth shape is a model gap
/// rather than a silent bucket.
fn site_for(stage: &str, selector: &str) -> &'static CallSite {
    let found = call_sites().iter().find(|site| {
        site.stage == stage && (site.selectors.contains(&selector) || selector == UNKNOWN_SELECTOR)
    });
    found.unwrap_or_else(|| {
        panic!(
            "a recorded eth_call at stage {stage} asks selector {selector}, \
        which no CallSite in {MODEL_FILE} declares — the model is missing a producer, not the \
        record being short"
        )
    })
}

fn selector_fact(selector: &str) -> Value {
    match SELECTORS.iter().find(|fact| fact.selector == selector) {
        Some(fact) => json!({
            "selector": fact.selector,
            "signature": fact.signature,
            "artifact_class": fact.artifact.as_str(),
            "state_dependency": fact.state_dependency,
            "block_dependency": fact.block_dependency.as_str(),
        }),
        None => json!({
            "selector": selector,
            "signature": UNKNOWN_SELECTOR,
            "artifact_class": ArtifactClass::Unknown.as_str(),
            "state_dependency": UNKNOWN_SELECTOR,
            "block_dependency": BlockDependency::NotAttributable.as_str(),
        }),
    }
}

fn candidate_rows() -> Vec<Value> {
    let record = read_json(&repo_path(CANDIDATES_RECORD));
    let mut rows: Vec<Value> = record["rows"]
        .as_array()
        .expect("the candidates record carries a rows array")
        .iter()
        .filter(|row| row["identity"]["method"].as_str() == Some("eth_call"))
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        a["candidate_id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["candidate_id"].as_str().unwrap_or_default())
    });
    rows
}

/// The decoded answers, at the artifact layer: what the pin's read produced and what the head's
/// read produced for the same pool in the same run. This is the only comparison §42 permits —
/// the record's own values, not a re-derivation from a build result.
fn decoded_answers() -> Vec<Value> {
    let mut rows = Vec::new();
    for run in subdirectories(ROUTE_RUNS_DIR) {
        let dir = repo_path(ROUTE_RUNS_DIR).join(&run);
        let route = read_json(&dir.join(ROUTE_RUN_FILE));
        let preflight = read_json(&dir.join(PREFLIGHT_FILE));
        let head = &preflight["head"];
        let head_pools: BTreeMap<String, &Value> = preflight["pools"]
            .as_array()
            .expect("the gate records the pools it read")
            .iter()
            .map(|pool| (pool["pool"].as_str().unwrap_or_default().to_string(), pool))
            .collect();
        for side in ["buy", "sell"] {
            let leg = &route["route"][side];
            let pool = leg["pool"].as_str().unwrap_or_default().to_string();
            let head_pool = head_pools
                .get(&pool)
                .unwrap_or_else(|| panic!("{run}: the gate recorded no head reading for {pool}"));
            let (pinned_r0, pinned_r1) = (&leg["reserve0"], &leg["reserve1"]);
            let (head_r0, head_r1) = (&head_pool["reserve0"], &head_pool["reserve1"]);
            rows.push(json!({
                "run": run,
                "pool": pool,
                "leg": side,
                "producer": {
                    "stage": "opportunity_detection",
                    "height": route["pinned_block"],
                    "block_hash": route["pinned_block_hash"],
                    "reserve0": pinned_r0,
                    "reserve1": pinned_r1,
                    "block_timestamp_last": leg["getReserves_blockTimestampLast"],
                    "record": format!("{ROUTE_RUNS_DIR}/{run}/{ROUTE_RUN_FILE}"),
                },
                "consumer": {
                    "stage": "preflight",
                    "height": head["block_number"],
                    "block_hash": head["block_hash"],
                    "reserve0": head_r0,
                    "reserve1": head_r1,
                    "block_timestamp_last": head_pool["block_timestamp_last"],
                    "record": format!("{ROUTE_RUNS_DIR}/{run}/{PREFLIGHT_FILE}"),
                },
                "answer_equal": pinned_r0 == head_r0 && pinned_r1 == head_r1,
                "blocks_apart": head["block_number"].as_u64().unwrap_or_default()
                    - route["pinned_block"].as_u64().unwrap_or_default(),
            }));
        }
    }
    rows
}

/// §41's control: the same `to`, the same `getReserves()` calldata, at the earliest height this
/// repository holds a decoded answer for and at the height the recorded pin reads. Both are
/// already committed, so this is a measurement rather than a hypothesis.
fn cross_height_control() -> Vec<Value> {
    let record = read_json(&repo_path(FEE_MEASUREMENT_RECORD));
    let provenance = &record["_provenance"];
    let early_height = provenance["read_at_head"]
        .as_str()
        .unwrap_or(EARLY_HEIGHT)
        .to_string();
    let candidates = record["candidates"]
        .as_array()
        .expect("the fee measurement records its candidates");
    let mut rows = Vec::new();
    for pool in [POOL_A, POOL_B] {
        let mut early: Option<&Value> = None;
        for candidate in candidates {
            for side in ["buying_pool", "selling_pool"] {
                let reading = &candidate[side];
                if reading["address"]
                    .as_str()
                    .map(str::to_lowercase)
                    .as_deref()
                    == Some(pool)
                {
                    early = Some(reading);
                }
            }
        }
        let early =
            early.unwrap_or_else(|| panic!("{pool} is not read in {FEE_MEASUREMENT_RECORD}"));
        let later = decoded_answers()
            .into_iter()
            .find(|row| row["pool"].as_str() == Some(pool))
            .expect("the same pool is read at the recorded pin");
        let producer = &later["producer"];
        rows.push(json!({
            "pool": pool,
            "calldata": "0902f1ac",
            "signature": "getReserves()",
            "earlier": {
                "height": early_height,
                "block_hash": provenance["block_hash"],
                "reserve0": early["reserve0"],
                "reserve1": early["reserve1"],
                "block_timestamp_last": early["getReserves_blockTimestampLast"],
                "record": FEE_MEASUREMENT_RECORD,
            },
            "later": {
                "height": producer["height"],
                "block_hash": producer["block_hash"],
                "reserve0": producer["reserve0"],
                "reserve1": producer["reserve1"],
                "block_timestamp_last": producer["block_timestamp_last"],
                "record": producer["record"],
            },
            "blocks_apart": later["producer"]["height"].as_u64().unwrap_or_default()
                - early_height.parse::<u64>().unwrap_or_default(),
            "answer_equal": early["reserve0"] == producer["reserve0"]
                && early["reserve1"] == producer["reserve1"],
        }));
    }
    rows
}

/// The market-activity census that bounds how far the within-run equality may be read.
fn activity_census() -> Value {
    let record = read_json(&repo_path(ACTIVITY_RECORD));
    let census = record["activity"]
        .as_object()
        .expect("the census records its pools");
    let mut pools = Vec::new();
    for pool in [POOL_A, POOL_B] {
        pools.push(json!({
            "pool": pool,
            "in_census": census.contains_key(pool),
            "census_record": format!("{ACTIVITY_RECORD}"),
        }));
    }
    // The two distances this sentence contrasts are read off the same measured rows the tables
    // publish, so the caveat cannot outlive the numbers it describes.
    let short_gaps: Vec<u64> = decoded_answers()
        .iter()
        .filter_map(|row| row["blocks_apart"].as_u64())
        .collect();
    let long_gaps: Vec<u64> = cross_height_control()
        .iter()
        .filter_map(|row| row["blocks_apart"].as_u64())
        .collect();
    let (Some(short_min), Some(short_max)) = (short_gaps.iter().min(), short_gaps.iter().max())
    else {
        panic!("no within-run comparison was measured, so the short gap has no bounds to name");
    };
    let Some(long_min) = long_gaps.iter().min() else {
        panic!(
            "no cross-height control was measured, so nothing supports the load-bearing finding"
        );
    };
    json!({
        "window_blocks": record["window_blocks"],
        "pools_with_activity": record["pools_with_activity"],
        "dual_venue_pools_with_activity": record["dual_venue_pools_with_activity"],
        "finding": record["finding"],
        "candidate_pools": pools,
        "short_gap_measured": {
            "min_blocks_apart": short_min,
            "max_blocks_apart": short_max,
            "comparisons": short_gaps.len(),
        },
        "long_gap_measured": {
            "min_blocks_apart": long_min,
            "comparisons": long_gaps.len(),
        },
        "what_it_bounds": format!(
            "neither candidate pool appears in this census, so a {short_min}–{short_max} block gap \
            had no recorded reason to change either reserve; the within-run equality therefore does \
            not show the answer is block-independent, and only the pair at {long_min} blocks apart \
            does"
        ),
    })
}

/// Bytecode already on disk, scanned with the PUSH-aware walk. No `eth_getCode` is asked for.
fn bytecode_scans() -> Vec<Value> {
    let dir = repo_path(BYTECODE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().starts_with("dump-"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    let mut rows = Vec::new();
    for path in files {
        let dump = read_json(&path);
        let accounts = dump["accounts"]
            .as_object()
            .expect("a dump records its accounts");
        for pool in [POOL_A, POOL_B] {
            let Some(account) = accounts.get(pool) else {
                continue;
            };
            let hex = account["code"].as_str().unwrap_or_default();
            let code = decode_hex(hex);
            let inventory = opcode_inventory(&code);
            let block: BTreeMap<String, usize> = inventory
                .iter()
                .filter(|(name, _)| name.starts_with("block:"))
                .map(|(name, count)| (name.trim_start_matches("block:").to_string(), *count))
                .collect();
            let state: BTreeMap<String, usize> = inventory
                .iter()
                .filter(|(name, _)| name.starts_with("state:"))
                .map(|(name, count)| (name.trim_start_matches("state:").to_string(), *count))
                .collect();
            rows.push(json!({
                "record": path.strip_prefix(workspace_root()).unwrap_or(&path).display().to_string(),
                "dumped_at_height": dump["block_number"],
                "dumped_at_hash": dump["block_hash"],
                "address": pool,
                "code_bytes": code.len(),
                "block_context_opcodes": block,
                "state_opcodes": state,
                "block_context_reachable": block_context_reachable_in_code(&code),
                "scan_scope": "whole code, PUSH immediates skipped; the walk cannot attribute an \
                    opcode to the dispatched selector, so a non-empty block set is \
                    not_attributable and an empty one is a proof of independence",
            }));
        }
    }
    rows
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let body = hex.strip_prefix("0x").unwrap_or(hex);
    let mut bytes = Vec::with_capacity(body.len() / 2);
    let mut index = 0;
    while index < body.len() {
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        let (Some(hi), Some(lo)) = (
            nibble(body.as_bytes()[index]),
            nibble(body.as_bytes()[index + 1]),
        ) else {
            panic!("{body}: a dump holds hex, not a partial byte")
        };
        bytes.push(hi * 16 + lo);
        index += 2;
    }
    bytes
}

// ---------------------------------------------------------------------------
// table 1: the call surface (§5, §6, §39)
// ---------------------------------------------------------------------------

fn call_surface(tree: &mut Tree) -> Value {
    let asks = asks();
    let mut groups: BTreeMap<(String, String, String, String), Vec<&Ask>> = BTreeMap::new();
    for ask in &asks {
        groups
            .entry((
                ask.identity.stage.clone(),
                ask.identity.caller.clone(),
                ask.identity.selector().to_string(),
                ask.identity.to.clone(),
            ))
            .or_default()
            .push(ask);
    }
    let rows: Vec<Value> = groups
        .iter()
        .map(|((stage, caller, selector, to), list)| {
            let site = site_for(stage, selector);
            let runs: BTreeSet<String> = list.iter().map(|ask| ask.run.clone()).collect();
            let heights: BTreeSet<String> = list
                .iter()
                .map(|ask| ask.identity.block.term.clone())
                .collect();
            let forms: BTreeMap<String, usize> = count_by(list, |ask| ask.identity.block.form);
            let with_block: BTreeSet<String> = list
                .iter()
                .map(|ask| ask.identity.identity_with_block())
                .collect();
            let without_block: BTreeSet<String> = list
                .iter()
                .map(|ask| ask.identity.identity_without_block())
                .collect();
            json!({
                "site": site.id,
                "stage": stage,
                "recorded_caller": caller,
                "caller_family": site.caller_family,
                "to": to,
                "selector": selector,
                "selector_knowledge": selector_fact(selector),
                "asks": list.len(),
                "runs": runs.iter().cloned().collect::<Vec<_>>(),
                "heights": heights.iter().cloned().collect::<Vec<_>>(),
                "block_forms": forms,
                "distinct_identities_with_block": with_block.len(),
                "distinct_identities_without_block": without_block.len(),
                "evidence_refs": list
                    .iter()
                    .map(|ask| {
                        resolve_record_token(
                            &format!("{RUNS_DIR}/{}/{CALLS_FILE}", ask.run),
                            ask.row["dedup_key"].as_str().unwrap_or_default(),
                            "the recorded ask, by its own dedup key",
                            tree,
                        )
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let sites = declared_sites();
    let forms: BTreeMap<String, usize> = count_by(&asks, |ask| ask.identity.block.form);
    json!({
        "milestone": "M8.5.1",
        "question": "every eth_call this build makes, from which code, asking what, at which \
            height — recounted from the published records rather than tallied by hand",
        "recompute": "group the rows of `data/evidence/m8/cross-stage/runs/*/pipeline-calls.json` \
            whose method is eth_call by (stage, caller, selector, target); a row is one logical \
            ask with its HTTP tries listed beside it",
        "unit": "asks",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "source_records": [RAW_RUNS_GLOB_LOCAL, CANDIDATES_RECORD],
        "rpc_boundary": {
            "file": RPC_BOUNDARY_FILE,
            "token": RPC_BOUNDARY_TOKEN,
            "note": "the one place a call becomes bytes on the wire, so the identity's field set \
                is decided by the request type rather than by any caller's taste",
            "evidence_refs": vec![
                resolve_record_token(RPC_BOUNDARY_FILE, RPC_BOUNDARY_TOKEN, "the eth_call branch", tree),
                resolve_record_token(
                    REQUEST_TYPE_FILE,
                    REQUEST_TYPE_TOKEN,
                    "CallRequest has exactly two fields, to and data",
                    tree,
                ),
            ],
        },
        "request_parameters": request_parameters(tree),
        "rows": rows,
        "totals": {
            "eth_call_asks": asks.len(),
            "asks_of_every_method_in_the_same_records": total_asks(),
            "call_sites_declared_by_the_model": sites.len(),
            "call_sites_observed_in_the_records": groups
                .iter()
                .map(|((stage, _, selector, _), _)| format!("{stage}|{}", site_for(stage, selector).id))
                .collect::<BTreeSet<_>>()
                .len(),
            "distinct_targets": asks
                .iter()
                .map(|ask| ask.identity.to.clone())
                .collect::<BTreeSet<_>>()
                .len(),
            "distinct_selectors": asks
                .iter()
                .map(|ask| ask.identity.selector().to_string())
                .collect::<BTreeSet<_>>()
                .len(),
            "selectors_not_in_the_table": asks
                .iter()
                .filter(|ask| !SELECTORS.iter().any(|fact| fact.selector == ask.identity.selector()))
                .count(),
            "distinct_identities_with_block": asks
                .iter()
                .map(|ask| ask.identity.identity_with_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "distinct_identities_without_block": asks
                .iter()
                .map(|ask| ask.identity.identity_without_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "block_forms": forms,
            "by_stage": count_by(&asks, |ask| ask.identity.stage.as_str()),
            "by_caller": count_by(&asks, |ask| ask.identity.caller.as_str()),
            "by_selector": count_by(&asks, |ask| ask.identity.selector()),
        },
        "what_the_counts_exclude": "the simulation's own REVM reads are recorded in the same runs \
            under a different sink; §12's eth_call surface is the pipeline's, and M8.4.2 already \
            published the per-sink split, so this table adds none",
    })
}

const RAW_RUNS_GLOB_LOCAL: &str = "data/evidence/m8/cross-stage/runs/*/pipeline-calls.json";

/// The four non-identity fields, each carrying the reason it is absent rather than a `null`.
fn request_parameters(tree: &mut Tree) -> Value {
    let asks = asks();
    let fields = [
        (
            "from",
            "0x0000000000000000000000000000000000000000",
            "the params object this build writes has two keys, so the node applies its own \
                default sender and the record does not capture what that default was",
        ),
        (
            "value",
            "0",
            "no field of CallRequest carries it, so no ask can move value",
        ),
        (
            "state_override",
            "[]",
            "the params array is two elements — the call object and the block term — so there is \
                no third element to override balance, nonce, code or storage",
        ),
        (
            "gas",
            "node_default",
            "no gas field in the request, so the node answers with its own call ceiling",
        ),
    ];
    json!(fields
        .iter()
        .map(|(name, not_sent, why)| {
            let states: BTreeMap<String, usize> =
                count_by(&asks, |ask| field_of(&ask.identity, name).label());
            json!({
                "field": name,
                "state": field_of(&asks[0].identity, name).label(),
                "states_over_the_asks": states,
                "settled": field_of(&asks[0].identity, name).is_settled(),
                "not_sent_value": not_sent,
                "rule": why,
                "detail": field_of(&asks[0].identity, name).detail(),
                "evidence_refs": vec![
                    resolve_record_token(
                        REQUEST_TYPE_FILE,
                        REQUEST_TYPE_TOKEN,
                        "the two-field request type",
                        tree,
                    ),
                    resolve_record_token(
                        RPC_BOUNDARY_FILE,
                        "\"data\": request.data.to_string()",
                        "the params object, written with those two keys and no third",
                        tree,
                    ),
                ],
            })
        })
        .collect::<Vec<_>>())
}

fn field_of<'a>(identity: &'a EthCallIdentity, name: &str) -> &'a FieldEvidence {
    match name {
        "from" => &identity.from,
        "value" => &identity.value,
        "state_override" => &identity.state_override,
        "gas" => &identity.gas,
        other => panic!("{other} is not one of the four non-identity fields"),
    }
}

fn count_by<T>(rows: &[T], key: impl Fn(&T) -> &str) -> BTreeMap<String, usize> {
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        *tally.entry(key(row).to_string()).or_insert(0) += 1;
    }
    tally
}

fn declared_sites() -> Vec<&'static str> {
    call_sites().iter().map(|site| site.id).collect()
}

// ---------------------------------------------------------------------------
// table 2: the normalized identities (§7, §8, §11, §12, §51)
// ---------------------------------------------------------------------------

fn normalized_identities(tree: &mut Tree) -> Value {
    let asks = asks();
    let rows: Vec<Value> = asks
        .iter()
        .map(|ask| {
            let mut json = ask.identity.to_json();
            let site = site_for(&ask.identity.stage, ask.identity.selector());
            json["site"] = json!(site.id);
            json["run"] = json!(ask.run);
            json["rpc_id"] = json!(ask.row["rpc_id"]);
            json["sink"] = json!(ask.row["sink"]);
            json["logical_request_id"] = json!(ask.row["logical_request_id"]);
            json["category"] = json!(ask.key.category);
            json["category_rule"] = json!(ask.key.category_rule);
            json["selector_knowledge"] = selector_fact(ask.identity.selector());
            json["identity_source"] = json!(ask.key.identity_source);
            json["evidence_refs"] = json!([resolve_record_token(
                &format!("{RUNS_DIR}/{}/{CALLS_FILE}", ask.run),
                ask.row["dedup_key"].as_str().unwrap_or_default(),
                "the recorded ask this identity is read out of",
                tree,
            )]);
            json
        })
        .collect();
    // §12's block distribution, kept as the record names it: a height, never a tag.
    let heights: BTreeMap<String, usize> = count_by(&asks, |ask| ask.identity.block.term.as_str());
    let groups: BTreeMap<String, Vec<&Ask>> = asks.iter().fold(
        BTreeMap::new(),
        |mut map: BTreeMap<String, Vec<&Ask>>, ask| {
            map.entry(ask.identity.identity_without_block())
                .or_default()
                .push(ask);
            map
        },
    );
    json!({
        "milestone": "M8.5.1",
        "question": "what one eth_call asked, in the terms that decide whether two asks are the \
            same ask — §51's rule that the terms come from the canonical key this build already \
            has, not from a second parser",
        "recompute": "each row is one row of `runs/*/pipeline-calls.json` whose method is \
            eth_call, run through `RpcReadKey::of_row` and then `EthCallIdentity::of_row`; the \
            two identity strings are the key's own terms with and without the block term",
        "unit": "asks, one row per ask",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "identity_terms": ["chain_id", "block", "to", "calldata"],
        "why_these_four": "the four that can change what the EVM executes; `from`, `value`, the \
            state override and gas are recorded as absences in `request_parameters` of \
            `call-surface.json`, because §7 forbids assuming them and §36 forbids writing them \
            as nulls",
        "non_identity_terms_recorded": [
            "from",
            "value",
            "state_override",
            "gas",
            "stage",
            "caller",
            "run",
            "sink",
            "category"
        ],
        "block_term": {
            "form_rule": "a decimal height is `number`, a node-resolved word is `tag`; the form is \
                M8.4.2's `RpcReadKey::block_form` rather than a new judgement",
            "hash_provenance_rule": "a record names a height without a hash unless the read that \
                fixed it returned both; `not_in_the_record` is the common case here and is stated \
                rather than left blank",
            "heights": heights,
            "forms": count_by(&asks, |ask| ask.identity.block.form),
        },
        "block_free_groups": groups
            .iter()
            .map(|(key, list)| {
                let distinct: BTreeSet<String> = list
                    .iter()
                    .map(|ask| ask.identity.identity_with_block())
                    .collect();
                json!({
                    "identity_without_block": key,
                    "asks": list.len(),
                    "distinct_identities_with_block": distinct.len(),
                    "heights": list
                        .iter()
                        .map(|ask| ask.identity.block.term.clone())
                        .collect::<BTreeSet<_>>()
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>(),
                    "spans_more_than_one_height": distinct.len() > 1,
                })
            })
            .collect::<Vec<_>>(),
        "totals": {
            "asks": rows.len(),
            "distinct_identities_with_block": asks
                .iter()
                .map(|ask| ask.identity.identity_with_block())
                .collect::<BTreeSet<_>>()
                .len(),
            "distinct_identities_without_block": groups.len(),
            "groups_spanning_more_than_one_height": groups
                .values()
                .filter(|list| {
                    list.iter()
                        .map(|ask| ask.identity.identity_with_block())
                        .collect::<BTreeSet<_>>()
                        .len()
                        > 1
                })
                .count(),
            "rows_with_a_tag_block_term": asks
                .iter()
                .filter(|ask| ask.identity.block.form == "tag")
                .count(),
        },
        "rows": rows,
    })
}

// ---------------------------------------------------------------------------
// table 3: the ownership matrix (§13–§23, §26)
// ---------------------------------------------------------------------------

fn ownership_matrix(tree: &mut Tree) -> Value {
    let verdicts = verdicts();
    let rows: Vec<Value> = call_sites()
        .iter()
        .map(|site| {
            let verdict = verdicts
                .iter()
                .find(|row| row.site == site.id)
                .unwrap_or_else(|| panic!("{} has no verdict", site.id));
            json!({
                "site": site.id,
                "stage": site.stage,
                "caller_family": site.caller_family,
                "selectors": site.selectors,
                "producer_fn": site.producer_fn,
                "producer_type": site.producer_type,
                "rpc_boundary": site.rpc_boundary,
                "result_type": site.result_type,
                "consumer_fn": site.consumer_fn,
                "consumer_type": site.consumer_type,
                "purpose": site.purpose,
                "artifact_class": site.artifact.as_str(),
                "ownership_form": site.ownership.as_str(),
                "result_owner": site.owner,
                "carrier": site.carrier.as_str(),
                "carrier_fields": site.carrier_fields,
                "read_is_the_check": site.read_is_the_check,
                "block_dependency": site.block_dependency.as_str(),
                "axes": verdict.axes.iter().map(|row| json!({
                    "axis": row.axis.as_str(),
                    "status": row.status.as_str(),
                    "note": row.note,
                })).collect::<Vec<_>>(),
                "safe_to_reuse_now": verdict.safe_to_reuse_now,
                "reusable_in_principle": verdict.reusable_in_principle,
                "class": verdict.class.as_str(),
                "blockers": verdict.blockers.iter().map(|b| b.as_str()).collect::<Vec<_>>(),
                "evidence_refs": resolve_all(site.anchors, tree),
            })
        })
        .collect();
    json!({
        "milestone": "M8.5.1",
        "question": "§26's eight axes per eth_call site: who holds the answer, what identifies it, \
            how long it is valid, what invalidates it, what authority it carries, what the \
            consumer can verify, whether it is deterministic, and whether anything carries it \
            across the stage boundary",
        "recompute": "one row per `CallSite` in the model; each axis status is the model's own \
            declaration and every `evidence_refs.line` is resolved against the source file at \
            assembly time",
        "unit": "call sites",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "axes": ALL_AXES.iter().map(|axis| axis.as_str()).collect::<Vec<_>>(),
        "status_vocabulary": ["proven", "partially_proven", "unknown", "not_applicable"],
        "rules": {
            "safe_to_reuse_now": "all eight axes proven, and only that — a partially_proven \
                carrier is not a carrier (§26)",
            "reusable_in_principle": "decided on the artifact's nature, never on the axes, and \
                kept a separate field so a table cannot collapse 「能不能」 into 「现在能不能」 (§27)",
            "not_a_null": "a field this build cannot send is recorded with the rule that keeps it \
                absent, which is a settled answer; not recorded and unknown are not (§36)",
        },
        "artifact_classes": artifact_classes(),
        "rows": rows,
        "totals": {
            "sites": rows.len(),
            "safe_to_reuse_now": rows
                .iter()
                .filter(|row| row["safe_to_reuse_now"] == json!(true))
                .count(),
            "reusable_in_principle": rows
                .iter()
                .filter(|row| row["reusable_in_principle"] == json!(true))
                .count(),
            "read_is_the_check": rows
                .iter()
                .filter(|row| row["read_is_the_check"] == json!(true))
                .count(),
            "by_axis_status": count_axis_status(&rows),
        },
    })
}

fn artifact_classes() -> Vec<Value> {
    [
        ArtifactClass::CanonicalStateRead,
        ArtifactClass::ProtocolDerivedQuote,
        ArtifactClass::FeeOracleEstimate,
        ArtifactClass::EphemeralProbe,
        ArtifactClass::Unknown,
    ]
    .iter()
    .map(|class| {
        json!({
            "class": class.as_str(),
            "sites": call_sites()
                .iter()
                .filter(|site| site.artifact == *class)
                .map(|site| site.id)
                .collect::<Vec<_>>(),
        })
    })
    .collect()
}

fn count_axis_status(rows: &[Value]) -> BTreeMap<String, usize> {
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        for axis in row["axes"].as_array().into_iter().flatten() {
            let key = format!(
                "{}={}",
                axis["axis"].as_str().unwrap_or_default(),
                axis["status"].as_str().unwrap_or_default()
            );
            *tally.entry(key).or_insert(0) += 1;
        }
    }
    tally
}

// ---------------------------------------------------------------------------
// table 4: the lifecycle contracts (§24, M8.4.3's five steps)
// ---------------------------------------------------------------------------

fn lifecycle_contracts(tree: &mut Tree) -> Value {
    let rows = lifecycle_rows();
    let mut grouped: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    let json_rows: Vec<Value> = rows
        .iter()
        .map(|row| serde_json::to_value(row).unwrap_or_else(|error| panic!("{error}")))
        .collect();
    for row in &json_rows {
        grouped
            .entry(row["site"].as_str().unwrap_or_default())
            .or_default()
            .push(row);
    }
    let sites: Vec<Value> = grouped
        .iter()
        .map(|(site, list)| {
            let anchors = call_sites()
                .iter()
                .find(|declared| declared.id == *site)
                .map(|declared| declared.anchors)
                .unwrap_or(&[]);
            json!({
                "site": site,
                "steps": list,
                "evidence_refs": resolve_all(anchors, tree),
            })
        })
        .collect();
    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    for row in &json_rows {
        *statuses
            .entry(row["status"].as_str().unwrap_or_default().to_string())
            .or_insert(0) += 1;
    }
    json!({
        "milestone": "M8.5.1",
        "question": "§24's five lifecycle steps per eth_call site, in M8.4.3's own vocabulary: \
            acquired, validated, published, consumed, invalidated or expired",
        "recompute": "one cell per site per step from `lifecycle_rows()`; a step with no \
            mechanism says so in its note rather than reading as an unexamined `unknown`",
        "unit": "cells",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "steps": LIFECYCLE_STEPS,
        "status_vocabulary": ["proven", "partially_proven", "unknown", "not_applicable"],
        "absent_step_rule": "the fifth step has no mechanism for these artifacts because nothing \
            retains the answer to expire it; that is stated as not_applicable with the reason, \
            which is a finding rather than a gap",
        "sites": sites,
        "totals": {
            "cells": json_rows.len(),
            "by_status": statuses,
        },
    })
}

// ---------------------------------------------------------------------------
// table 5: the dependency matrix (§17–§19, §41, §42)
// ---------------------------------------------------------------------------

fn dependency_matrix(tree: &mut Tree) -> Value {
    let pairs = candidate_rows();
    let answers = decoded_answers();
    let control = cross_height_control();
    let census = activity_census();
    let scans = bytecode_scans();
    let pair_rows: Vec<Value> = pairs
        .iter()
        .map(|pair| {
            let selector = pair["identity"]["terms"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|term| term[0].as_str() == Some("data"))
                .and_then(|term| term[1].as_str().map(str::to_string))
                .unwrap_or_default();
            let selector = selector.chars().take(8).collect::<String>();
            let stage = pair["consumer"]["stage"].as_str().unwrap_or_default();
            let site = site_for(stage, &selector);
            let delta = pair["consumer_block"]
                .as_str()
                .unwrap_or_default()
                .parse::<i64>()
                .unwrap_or_default()
                - pair["producer_block"]
                    .as_str()
                    .unwrap_or_default()
                    .parse::<i64>()
                    .unwrap_or_default();
            let matching = answers.iter().find(|row| {
                row["run"].as_str() == pair["run"].as_str()
                    && row["pool"].as_str() == selector_pool(pair)
            });
            json!({
                "candidate_id": pair["candidate_id"],
                "run": pair["run"],
                "producer": {
                    "stage": pair["producer"]["stage"],
                    "caller": pair["producer"]["caller"],
                    "rpc_id": pair["producer"]["rpc_id"],
                    "height": pair["producer_block"],
                    "form": pair["producer_block_form"],
                },
                "consumer": {
                    "stage": pair["consumer"]["stage"],
                    "caller": pair["consumer"]["caller"],
                    "rpc_id": pair["consumer"]["rpc_id"],
                    "height": pair["consumer_block"],
                    "form": pair["consumer_block_form"],
                    "site": site.id,
                },
                "selector": selector,
                "selector_knowledge": selector_fact(&selector),
                "block_relation": pair["block_relation"],
                "blocks_apart": delta,
                "same_block_free_identity": pair["identity"]["identity_without_block"]
                    .as_str()
                    .unwrap_or_default(),
                "record_verdict": pair["verdict"],
                "record_safe_to_reuse": pair["safe_to_reuse"],
                "this_milestone_class": classify_pair(site, delta).as_str(),
                "blockers": pair_blockers(site, delta)
                    .iter()
                    .map(|b| b.as_str())
                    .collect::<Vec<_>>(),
                "artifact_layer_answer_comparison": matching
                    .map(|row| json!({"equal": row["answer_equal"], "detail": row})),
                "evidence_refs": vec![resolve_record_token(
                    CANDIDATES_RECORD,
                    pair["candidate_id"].as_str().unwrap_or_default(),
                    "the directed pair this row is",
                    tree,
                )],
            })
        })
        .collect();
    json!({
        "milestone": "M8.5.1",
        "question": "§17's decisive question per selector and per measured pair: does the block \
            term change the answer, and on what evidence is that said either way",
        "recompute": "the pairs are `reuse-candidates.json`'s own rows filtered to eth_call; the \
            per-pair answer comparison reads `route-runs/*/route-run.json` and \
            `route-runs/*/preflight.json`, which hold the decoded values the two stages actually \
            got; the cross-height control reads the M7 fee measurement, and the bytecode scan \
            reads the M7 dumps",
        "unit": "pairs, selectors, runs and scans, each with its own rows",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "dependency_vocabulary": ["proven_load_bearing", "not_attributable", "not_applicable"],
        "selector_facts": SELECTORS
            .iter()
            .map(|fact| json!({
                "selector": fact.selector,
                "signature": fact.signature,
                "question": fact.question,
                "returns": fact.returns,
                "artifact_class": fact.artifact.as_str(),
                "state_dependency": fact.state_dependency,
                "block_dependency": fact.block_dependency.as_str(),
                "evidence_refs": resolve_all(fact.anchors, tree),
            }))
            .collect::<Vec<_>>(),
        "opcode_tables": {
            "block_context": BLOCK_CONTEXT_OPCODES
                .iter()
                .map(|(byte, name)| json!({"byte": byte, "name": name}))
                .collect::<Vec<_>>(),
            "state": STATE_OPCODES
                .iter()
                .map(|(byte, name)| json!({"byte": byte, "name": name}))
                .collect::<Vec<_>>(),
            "why_a_linear_walk_is_enough": "an absent block-context opcode anywhere in the code \
                proves the contract cannot read one on the dispatched path; a present one proves \
                nothing, because the walk cannot attribute it to a selector. So the scan can \
                grant independence and can never revoke it — which is why no site in this table \
                is marked safe on the scan alone (§18's asymmetry)",
        },
        "bytecode_scans": scans,
        "decoded_answers_within_a_run": answers,
        "cross_height_control": control,
        "activity_census": census,
        "pair_rows": pair_rows,
        "totals": {
            "pairs": pair_rows.len(),
            "pairs_blocked": pair_rows
                .iter()
                .filter(|row| row["this_milestone_class"] != json!(ReuseClass::ReuseReady.as_str()))
                .count(),
            "pairs_with_equal_answers_within_the_run": answers
                .iter()
                .filter(|row| row["answer_equal"] == json!(true))
                .count(),
            "pairs_compared_within_the_run": answers.len(),
            "cross_height_controls": control.len(),
            "cross_height_controls_answering_differently": control
                .iter()
                .filter(|row| row["answer_equal"] == json!(false))
                .count(),
            "blocks_apart_histogram": pair_rows
                .iter()
                .map(|row| row["blocks_apart"].as_i64().unwrap_or_default())
                .fold(BTreeMap::new(), |mut map: BTreeMap<i64, usize>, delta| {
                    *map.entry(delta).or_insert(0) += 1;
                    map
                }),
            "scans_with_a_block_context_opcode": scans
                .iter()
                .filter(|row| row["block_context_reachable"] == json!(true))
                .count(),
        },
        "what_this_table_refuses_to_infer": "§42: a build result equal to another build result \
            says nothing about whether two eth_call answers are equal, so every equality here is \
            between two recorded answers of the same kind, read from the fields that hold them",
    })
}

fn selector_pool(pair: &Value) -> Option<&str> {
    pair["identity"]["address"].as_str()
}

/// §28's class for one measured pair, decided from the same declarations the site verdicts use
/// plus the pair's own measured block distance.
/// §28's class for one measured pair. The precedence is the model's own (`verdict_for`'s
/// `classify`): the check-is-the-read blocker outranks the block term, because a reused answer
/// there leaves the gate comparing a value with a copy of itself even inside one block.
fn classify_pair(site: &CallSite, delta: i64) -> ReuseClass {
    let blockers = pair_blockers(site, delta);
    if blockers.contains(&ReuseBlocker::ReadIsTheCheck) {
        return ReuseClass::ReuseBlockedByVerification;
    }
    if blockers.contains(&ReuseBlocker::BlockChangesTheAnswer) {
        return ReuseClass::ReuseBlockedByIdentity;
    }
    if blockers.contains(&ReuseBlocker::NoCarrier) {
        return ReuseClass::ReuseBlockedByCarrier;
    }
    if blockers.contains(&ReuseBlocker::FreshnessRuleNotProven) {
        return ReuseClass::ReuseBlockedByFreshness;
    }
    ReuseClass::Unknown
}

/// The blockers one pair carries: the site's own `read_is_the_check`, and a cross-block refusal
/// only where the block term was measured load-bearing for that selector.
fn pair_blockers(site: &CallSite, delta: i64) -> Vec<ReuseBlocker> {
    let mut blockers = Vec::new();
    if site.read_is_the_check {
        blockers.push(ReuseBlocker::ReadIsTheCheck);
    }
    if delta != 0 && site.block_dependency == BlockDependency::ProvenLoadBearing {
        blockers.push(ReuseBlocker::BlockChangesTheAnswer);
    }
    if site.carrier == CarrierForm::None {
        blockers.push(ReuseBlocker::NoCarrier);
    }
    if site.freshness != ProofStatus::Proven {
        blockers.push(ReuseBlocker::FreshnessRuleNotProven);
    }
    blockers
}

// ---------------------------------------------------------------------------
// table 6: the negative controls (§40, §41)
// ---------------------------------------------------------------------------

/// One `key=value` term of a published identity string, read positionally rather than by
/// re-deriving the identity so the counterfactuals below group the strings the tables already
/// print instead of a second parsing of the records.
fn identity_term(identity: &str, term: &str) -> String {
    identity
        .split('|')
        .find(|part| part.starts_with(&format!("{term}=")))
        .and_then(|part| part.split_once('='))
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| panic!("{identity:?} carries no {term} term"))
}

/// §40's counterfactual arithmetic, measured on the corpus: how many distinct identities the same
/// recorded asks form when one term of the key is dropped. Every count in the controls' prose
/// comes from this grouping rather than from a sentence, and the recompute gate re-derives the
/// same numbers from the runs' own `dedup_key` fields.
fn collapses(asks: &[Ask], pairs: &[&Value]) -> Value {
    let group = |drop: &str| -> usize {
        asks.iter()
            .map(|ask| {
                let identity = ask.identity.identity_with_block();
                (
                    if drop == "chain" {
                        String::new()
                    } else {
                        identity_term(&identity, "chain")
                    },
                    if drop == "block" {
                        String::new()
                    } else {
                        identity_term(&identity, "block")
                    },
                    if drop == "to" {
                        String::new()
                    } else {
                        identity_term(&identity, "to")
                    },
                    if drop == "data" {
                        String::new()
                    } else {
                        identity_term(&identity, "data")
                    },
                )
            })
            .collect::<BTreeSet<_>>()
            .len()
    };
    let pair_groups = |keep: &[&str]| -> usize {
        pairs
            .iter()
            .map(|pair| {
                let free = pair["identity"]["identity_without_block"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "{}: a candidate row with no block-free identity to group on",
                            pair["candidate_id"].as_str().unwrap_or_default()
                        )
                    });
                keep.iter()
                    .map(|term| identity_term(free, term))
                    .collect::<Vec<_>>()
            })
            .collect::<BTreeSet<_>>()
            .len()
    };
    json!({
        "asks_in_the_corpus": asks.len(),
        "distinct_with_every_term": group("none"),
        "distinct_if_chain_dropped": group("chain"),
        "distinct_if_block_dropped": group("block"),
        "distinct_if_to_dropped": group("to"),
        "distinct_if_calldata_dropped": group("data"),
        "chains_in_the_corpus": asks
            .iter()
            .map(|ask| identity_term(&ask.identity.identity_with_block(), "chain"))
            .collect::<BTreeSet<_>>()
            .len(),
        "pairs_in_the_corpus": pairs.len(),
        "pair_groups_by_to_and_calldata": pair_groups(&["to", "data"]),
        "pair_groups_by_calldata_alone": pair_groups(&["data"]),
        "pair_groups_by_to_alone": pair_groups(&["to"]),
        "what_the_chain_term_cannot_be_shown_to_do": "every recorded ask in this corpus is on one \
            chain, so dropping the chain term collapses nothing here — the term is kept because \
            the model declares it, and this table says plainly that the corpus provides no \
            control that would catch its absence",
    })
}

fn negative_controls(tree: &mut Tree) -> Value {
    let asks = asks();
    let pairs = candidate_rows();
    let control = cross_height_control();
    let census = activity_census();
    let answers = decoded_answers();
    let differing = control
        .iter()
        .filter(|row| row["answer_equal"] == json!(false))
        .count();
    let long_gap = control
        .iter()
        .filter_map(|row| row["blocks_apart"].as_u64())
        .min()
        .unwrap_or_else(|| panic!("no cross-height control row carries a measured distance"));
    let pair_refs: Vec<&Value> = pairs.iter().collect();
    let collapse = collapses(&asks, &pair_refs);
    let number = |key: &str| collapse[key].as_u64().unwrap_or_default();

    // NC1 and NC2 are run against the corpus rather than against a fixture: a real pair of asks
    // that differs only in `to`, and one that differs only in calldata.
    let nc1 = find_pair(&asks, |a, b| {
        a.identity.block.term == b.identity.block.term
            && a.identity.calldata == b.identity.calldata
            && a.identity.to != b.identity.to
    });
    let nc2 = find_pair(&asks, |a, b| {
        a.identity.block.term == b.identity.block.term
            && a.identity.to == b.identity.to
            && a.identity.calldata != b.identity.calldata
    });
    let nc3: Vec<&Ask> = asks
        .iter()
        .filter(|a| {
            asks.iter().any(|b| {
                a.identity.block.term != b.identity.block.term
                    && a.identity.to == b.identity.to
                    && a.identity.calldata == b.identity.calldata
            })
        })
        .collect();

    json!({
        "milestone": "M8.5.1",
        "question": "§40's six controls, each run against a real parsed identity rather than \
            asserted, plus §41's key control: same target, same calldata, different block",
        "recompute": "NC1 and NC2 are pairs of rows inside `runs/*/pipeline-calls.json` chosen by \
            the predicate printed with them; NC3's count is the pairs the same records form; \
            NC4–NC6 are the request type and the params object, which is where those fields are \
            decided",
        "unit": "controls",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "controls": NEGATIVE_CONTROLS
            .iter()
            .map(|control| {
                let (measured, detail) = match control.id {
                    "NC1" => (
                        nc1.is_some(),
                        nc1
                            .map(|(a, b)| {
                                json!({
                                    "pair_found": true,
                                    "ask_a": ask_ref(a),
                                    "ask_b": ask_ref(b),
                                    "identity_with_block_moved": a.identity.identity_with_block()
                                        != b.identity.identity_with_block(),
                                    "block_free_identity_moved": a.identity.identity_without_block()
                                        != b.identity.identity_without_block(),
                                })
                            })
                            .unwrap_or_else(|| no_pair("to")),
                    ),
                    "NC2" => (
                        nc2.is_some(),
                        nc2
                            .map(|(a, b)| {
                                json!({
                                    "pair_found": true,
                                    "ask_a": ask_ref(a),
                                    "ask_b": ask_ref(b),
                                    "identity_with_block_moved": a.identity.identity_with_block()
                                        != b.identity.identity_with_block(),
                                    "block_free_identity_moved": a.identity.identity_without_block()
                                        != b.identity.identity_without_block(),
                                })
                            })
                            .unwrap_or_else(|| no_pair("calldata")),
                    ),
                    "NC3" => (
                        !nc3.is_empty(),
                        json!({
                            "asks_with_a_sibling_at_another_height": nc3.len(),
                            "directed_pairs_in_the_record": pairs.len(),
                            "identity_with_block_moved": true,
                            "block_free_identity_moved": false,
                        }),
                    ),
                    "NC4" | "NC5" | "NC6" => (
                        asks.iter().all(|ask| {
                            matches!(field_of(&ask.identity, control.field), FieldEvidence::ProvenAbsent(_))
                        }),
                        json!({
                            "asks": asks.len(),
                            "state": field_of(&asks[0].identity, control.field).label(),
                            "settled": field_of(&asks[0].identity, control.field).is_settled(),
                            "rule": field_of(&asks[0].identity, control.field).detail(),
                            "which_is_a_measurement_not_an_absence_of_investigation": "every ask \
                                carries the state, so a single null in the corpus would fail this \
                                control rather than pass it quietly",
                        }),
                    ),
                    other => panic!("{other} is not a control this table knows"),
                };
                json!({
                    "id": control.id,
                    "field": control.field,
                    "participates_in_identity": control.participates_in_identity,
                    "why": control.why,
                    "run_against_the_corpus": measured,
                    "measured": detail,
                })
            })
            .collect::<Vec<_>>(),
        "key_control_same_target_same_calldata_different_block": {
            "asked_by": "§41: this is the control that decides whether the identity may drop the \
                block term, so it is measured rather than assumed",
            "within_the_recorded_runs": answers
                .iter()
                .map(|row| json!({
                    "run": row["run"],
                    "pool": row["pool"],
                    "producer_height": row["producer"]["height"],
                    "consumer_height": row["consumer"]["height"],
                    "blocks_apart": row["blocks_apart"],
                    "answer_equal": row["answer_equal"],
                }))
                .collect::<Vec<_>>(),
            "at_a_distance_where_the_chain_moved": control,
            "verdict": format!(
                "the field is load-bearing: the same target and the same calldata answered \
                 differently in {differing} of {} pools at heights {long_gap} blocks apart, so a \
                 cross-block reuse of a reserve reading is NOT_SAFE_REUSE by measurement",
                control.len()
            ),
            "why_the_short_gap_is_not_the_same_evidence": census["what_it_bounds"],
            "census": census,
            "identity_collapse_counterfactuals": &collapse,
        },
        "what_a_passing_negative_control_would_have_caught": [
            format!(
                "a key that ignored `to` would have merged the {} recorded asks into {}, so the \
                 two candidate pools would have answered for one another",
                number("asks_in_the_corpus"),
                number("distinct_if_to_dropped")
            ),
            format!(
                "a key that ignored calldata would have merged the {} recorded asks into {}, so \
                 token0() and getReserves() would have shared one answer",
                number("asks_in_the_corpus"),
                number("distinct_if_calldata_dropped")
            ),
            format!(
                "a key that ignored the block would have merged the {} recorded asks into {} and \
                 grouped the {} measured pairs into {}, which is precisely the error §41 exists to \
                 prevent",
                number("asks_in_the_corpus"),
                number("distinct_if_block_dropped"),
                number("pairs_in_the_corpus"),
                number("pair_groups_by_to_and_calldata")
            ),
            format!(
                "a key that ignored `chain_id` would have collapsed nothing in this corpus: the {} \
                 recorded asks all sit on {chains} chain value, so no control here could catch the \
                 term going missing — the term is kept because the model declares it, and the \
                 table says plainly that the corpus does not test it",
                number("asks_in_the_corpus"),
                chains = number("chains_in_the_corpus")
            ),
            "an identity that wrote `from: null` instead of a proven absence would have read as \
                investigated while answering nothing".to_string(),
        ],
        "boundary_refs": vec![
            resolve_record_token(
                RPC_BOUNDARY_FILE,
                RPC_BOUNDARY_TOKEN,
                "the eth_call branch that fixes the field set",
                tree,
            ),
            resolve_record_token(
                REQUEST_TYPE_FILE,
                REQUEST_TYPE_TOKEN,
                "the two-field request type",
                tree,
            ),
        ],
    })
}

fn ask_ref(ask: &Ask) -> Value {
    json!({
        "run": ask.run,
        "rpc_id": ask.row["rpc_id"],
        "identity": ask.identity.identity_with_block(),
    })
}

/// §36: a control the corpus cannot supply is published as `pair_found: false` with the reason
/// stated, never as a null a reader would have to interpret as "investigated".
fn no_pair(field: &str) -> Value {
    json!({
        "pair_found": false,
        "why": format!("no two recorded asks differ only in {field}, so this control is published \
            as not run rather than as passed"),
    })
}

/// The first pair of recorded asks satisfying the predicate, with the earlier one named first.
fn find_pair(asks: &[Ask], predicate: impl Fn(&Ask, &Ask) -> bool) -> Option<(&Ask, &Ask)> {
    for (index, first) in asks.iter().enumerate() {
        for second in asks[index + 1..].iter() {
            if predicate(first, second) {
                return Some((first, second));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// table 7: the reuse verdicts (§26–§28, §58, §66)
// ---------------------------------------------------------------------------

fn reuse_verdicts(tree: &mut Tree) -> Value {
    let verdicts = verdicts();
    let sites: Vec<Value> = verdicts
        .iter()
        .map(|verdict| {
            let mut json = verdict.to_json();
            let anchors = call_sites()
                .iter()
                .find(|site| site.id == verdict.site)
                .map(|site| site.anchors)
                .unwrap_or(&[]);
            json["evidence_refs"] = json!(resolve_all(anchors, tree));
            json
        })
        .collect();
    let pairs = dependency_matrix_pairs();
    let pairs_reuse_ready = pairs
        .iter()
        .filter(|row| row["this_milestone_class"] == json!(ReuseClass::ReuseReady.as_str()))
        .count();
    let mut blockers: BTreeMap<String, usize> = BTreeMap::new();
    for row in &pairs {
        for blocker in row["blockers"].as_array().into_iter().flatten() {
            *blockers
                .entry(blocker.as_str().unwrap_or_default().to_string())
                .or_insert(0) += 1;
        }
    }
    json!({
        "milestone": "M8.5.1",
        "question": "the answer to §66: if Opportunity Detection's eth_call result were handed to \
            Preflight now, why could or could not Preflight trust it",
        "recompute": "the six site rows are `verdicts()` with their anchors resolved; the 18 pair \
            rows are `reuse-candidates.json`'s eth_call rows classified by the same declarations \
            plus each pair's measured block distance",
        "unit": "sites and measured pairs, kept as two row sets because they answer two questions",
        "generated_by": WRITER,
        "model": MODEL_FILE,
        "class_vocabulary": [
            ReuseClass::ReuseReady.as_str(),
            ReuseClass::PropagationPossible.as_str(),
            ReuseClass::ReuseBlockedByIdentity.as_str(),
            ReuseClass::ReuseBlockedByFreshness.as_str(),
            ReuseClass::ReuseBlockedByOwnership.as_str(),
            ReuseClass::ReuseBlockedByInvalidation.as_str(),
            ReuseClass::ReuseBlockedByVerification.as_str(),
            ReuseClass::ReuseBlockedByDeterminism.as_str(),
            ReuseClass::ReuseBlockedByCarrier.as_str(),
            ReuseClass::Unknown.as_str(),
        ],
        "blocker_vocabulary": [
            ReuseBlocker::NoCarrier.as_str(),
            ReuseBlocker::OwnerNotEstablished.as_str(),
            ReuseBlocker::FreshnessRuleNotProven.as_str(),
            ReuseBlocker::InvalidationNotEstablished.as_str(),
            ReuseBlocker::BlockDependencyUnproven.as_str(),
            ReuseBlocker::ResultNotRecorded.as_str(),
            ReuseBlocker::ConsumerCannotVerify.as_str(),
            ReuseBlocker::LatestTag.as_str(),
            ReuseBlocker::BlockChangesTheAnswer.as_str(),
            ReuseBlocker::ReadIsTheCheck.as_str(),
        ],
        "sites": sites,
        "pairs": pairs,
        "why_the_class_and_the_record_verdict_name_different_things": "a record's own verdict names \
            the rule it applies — M8.4.2 refuses a pair whose block term differs, because two asks \
            that differ by height are not the same read — while this milestone's class names the \
            blocker that would still hold if that rule were relaxed: for every pair measured here \
            the consumer's check is itself a second reading of the same value, so handing over the \
            producer's answer would move the refusal out of the gate and leave nothing in its \
            place. Both are published per row, so the difference is shown rather than argued.",
        "totals": {
            "sites": sites.len(),
            "safe_to_reuse_now": sites
                .iter()
                .filter(|row| row["safe_to_reuse_now"] == json!(true))
                .count(),
            "reusable_in_principle": sites
                .iter()
                .filter(|row| row["reusable_in_principle"] == json!(true))
                .count(),
            "pairs": pairs.len(),
            "pairs_reuse_ready": pairs_reuse_ready,
            "blockers": blockers,
        },
        "outcome": {
            "label": "REUSE_BLOCKED",
            "why_this_label": "§58 counts SAFE_CANDIDATE_FOUND, REUSE_BLOCKED and \
                NOT_ENOUGH_EVIDENCE as successes; this is REUSE_BLOCKED because every measured \
                pair is refused with a named cause the records support, not because the question \
                is unanswerable",
            "candidates_found": pairs.len(),
            "candidates_safe": pairs_reuse_ready,
            "one_sentence_answer_to_66": "Preflight could not trust the handed-over reading, \
                because the reading Preflight needs is the one at the head and its job is to be \
                the second opinion: `Repricing::expected_output` refuses when the head's reserves \
                are missing rather than falling back to the finding's numbers, so a copy of the \
                finding's answer would leave the gate comparing a value with a copy of itself \
                while the block term the records show is measured load-bearing",
            "net_rpc_saving": 0,
            "why_no_saving": "no eth_call could be removed without removing a check, and §31's \
                stage forbids optimising anything, so this milestone publishes the eligibility \
                answer and stops there",
        },
        "cross_stage_carrier": carrier_rows(),
    })
}

/// §23's carrier question answered per site from the fields the code actually has.
fn carrier_rows() -> Vec<Value> {
    call_sites()
        .iter()
        .map(|site| {
            json!({
                "site": site.id,
                "carrier": site.carrier.as_str(),
                "fields": site.carrier_fields,
                "crosses_a_stage_boundary": site.carrier
                    != evm_pipeline::eth_call_semantics::CarrierForm::None,
            })
        })
        .collect()
}

/// The pair rows on their own, so the verdict table and the dependency table cannot drift.
fn dependency_matrix_pairs() -> Vec<Value> {
    dependency_matrix(&mut Tree::new())
        .get("pair_rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// README
// ---------------------------------------------------------------------------

fn build_readme(root: &Value) -> String {
    let surface = &root["call-surface"];
    let identities = &root["normalized-identities"];
    let ownership = &root["ownership-matrix"];
    let lifecycle = &root["lifecycle-contracts"];
    let dependency = &root["dependency-matrix"];
    let controls = &root["negative-controls"];
    let verdicts = &root["reuse-verdicts"];
    let pair_rows = dependency["pair_rows"]
        .as_array()
        .expect("the dependency matrix publishes its pair rows");
    let mut lines: Vec<String> = Vec::new();
    push(
        &mut lines,
        "# M8.5.1 — `eth_call` 语义与生命周期诊断（证据）",
    );
    push(&mut lines, "");
    push(&mut lines, "## 一句话结论（白话）");
    push(&mut lines, "");
    push(
        &mut lines,
        "结论：**不能信**，而且**省不出 RPC**。Preflight 要的那次读数就是「头块这次」，它的职责是当第二双眼睛：\
            代码里 `Repricing::expected_output` 在头部读数缺失时是直接拒绝，不会退回用发现阶段的数字，\
            所以把上游的副本递进去，闸门就变成拿一个数字跟自己比。记录里的块字段又是实测会改答案的（远距对照）。",
    );
    push(
        &mut lines,
        &format!(
            "本轮净省 RPC = **{}**，判定 = **{}**。",
            shown(&verdicts["outcome"]["net_rpc_saving"]),
            verdicts["outcome"]["label"].as_str().unwrap_or_default(),
        ),
    );
    push(&mut lines, "");
    push(
        &mut lines,
        "「省不出」不是「还没找到」：省不掉的那一次就是检查本身，删它等于删闸门。",
    );
    push(&mut lines, "");
    push(
        &mut lines,
        "§66 要求的那句原文（逐字来自 `reuse-verdicts.json`）：",
    );
    push(&mut lines, "");
    push(
        &mut lines,
        verdicts["outcome"]["one_sentence_answer_to_66"]
            .as_str()
            .unwrap_or_default(),
    );
    push(&mut lines, "");
    push(&mut lines, "## 数字");
    push(&mut lines, "");
    push(
        &mut lines,
        &format!(
            "- 记录的 `eth_call` 请求：**{}** 次（全部方法 {} 次里的），来自 {} 把已提交的 run；\
                去重后带块的语义身份 **{}** 个，去掉块 **{}** 个",
            shown(&surface["totals"]["eth_call_asks"]),
            shown(&surface["totals"]["asks_of_every_method_in_the_same_records"]),
            subdirectories(RUNS_DIR).len(),
            shown(&identities["totals"]["distinct_identities_with_block"]),
            shown(&identities["totals"]["distinct_identities_without_block"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- 跨阶段候选对：**{}** 对，其中 {} 对是 `opportunity_detection → preflight`、{} 对跨块；\
                判定为可复用 **{}** 对（结果标签 {}）",
            shown(&dependency["totals"]["pairs"]),
            pair_rows
                .iter()
                .filter(|row| {
                    row["producer"]["stage"] == json!("opportunity_detection")
                        && row["consumer"]["stage"] == json!("preflight")
                })
                .count(),
            pair_rows
                .iter()
                .filter(|row| row["block_relation"] == json!("different_block"))
                .count(),
            shown(&verdicts["totals"]["pairs_reuse_ready"]),
            verdicts["outcome"]["label"].as_str().unwrap_or_default(),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- {} 个调用点里 `safe_to_reuse_now = true` 的：**{}**；`reusable_in_principle = true` 的：**{}**（两个字段是分开的，见 §27）",
            shown(&ownership["totals"]["sites"]),
            shown(&verdicts["totals"]["safe_to_reuse_now"]),
            shown(&verdicts["totals"]["reusable_in_principle"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- 生命周期格子：**{}** 格（{} 站 × {} 步），状态分布 {}",
            shown(&lifecycle["totals"]["cells"]),
            lifecycle["sites"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            lifecycle["steps"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            shown(&lifecycle["totals"]["by_status"]),
        ),
    );
    push(
        &mut lines,
        &format!(
            "- 块这个字段到底改不改答案：**同一次 run 内**（相差 {}–{} 块）比了 {} 组，答案相同 {} 组；\
                **相隔 {} 块**的两把读数，{} 口池答案**都不同**（{} 组里 {} 组不同）",
            shown(&dependency["activity_census"]["short_gap_measured"]["min_blocks_apart"]),
            shown(&dependency["activity_census"]["short_gap_measured"]["max_blocks_apart"]),
            shown(&dependency["totals"]["pairs_compared_within_the_run"]),
            shown(&dependency["totals"]["pairs_with_equal_answers_within_the_run"]),
            shown(&dependency["activity_census"]["long_gap_measured"]["min_blocks_apart"]),
            shown(&dependency["totals"]["cross_height_controls"]),
            shown(&dependency["totals"]["cross_height_controls"]),
            shown(&dependency["totals"]["cross_height_controls_answering_differently"]),
        ),
    );
    push(&mut lines, "");
    push(
        &mut lines,
        &format!(
            "短距的「相同」**不能**读成「块无关」：这 {} 口池在 M7 的 {} 块 swap/sync 普查里\
                **一次都没出现**（冷池），所以那 {}–{} 块本来就没有理由变。真正证明「块会改答案」的是\
                远距那把——同一个 `to`、同一份 `getReserves()` 数据、两个高度，两个字段都变了。",
            dependency["activity_census"]["candidate_pools"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            shown(&dependency["activity_census"]["window_blocks"]),
            shown(&dependency["activity_census"]["short_gap_measured"]["min_blocks_apart"]),
            shown(&dependency["activity_census"]["short_gap_measured"]["max_blocks_apart"]),
        ),
    );
    push(&mut lines, "");
    push(&mut lines, &format!("## {} 张表", ETH_CALL_FILES.len()));
    push(&mut lines, "");
    push(
        &mut lines,
        &format!(
            "| 文件 | 回答什么 | 行/格数量 |
| --- | --- | --- |
| `{}` | 谁在什么阶段用什么 calldata 问了什么 | {} |
| `{}` | 每个请求的语义身份 + {} 个非身份字段的「为什么不存在」 | {} |
| `{}` | {} 个复用轴 × {} 个调用点 | {} |
| `{}` | {} 个生命周期步骤 × {} 个调用点 | {} |
| `{}` | 块是否改变答案（选择器/配对/字节码/普查） | {} |
| `{}` | NC1–NC{} 跑在真实身份上，不是写在纸上 | {} |
| `{}` | 每站每对的复用判定 + §66 一句话答案 | {} |",
            ETH_CALL_FILES[0],
            surface["rows"].as_array().map(Vec::len).unwrap_or_default(),
            ETH_CALL_FILES[1],
            surface["request_parameters"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            identities["rows"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ETH_CALL_FILES[2],
            ownership["axes"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ownership["rows"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ownership["rows"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ETH_CALL_FILES[3],
            lifecycle["steps"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            lifecycle["sites"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            lifecycle["totals"]["cells"],
            ETH_CALL_FILES[4],
            dependency["pair_rows"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ETH_CALL_FILES[5],
            controls["controls"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            controls["controls"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            ETH_CALL_FILES[6],
            verdicts["sites"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
        ),
    );
    push(&mut lines, "");
    push(&mut lines, "## 本轮没做、也不允许做的事");
    push(&mut lines, "");
    for item in [
        "没有新增任何 RPC：所有数字都从已提交记录重算，没有为了「确认一次 eth_call」再打一次 eth_call；本轮真实运行次数 = 0",
        "没有做任何优化：没加缓存、没加批处理、没减 eth_call、没删 preflight 的第二次读",
        "没改生产逻辑：新增的只有诊断模块和它的证据表",
        "没有把不存在的字段写成 null 当「已调查」：from / value / state_override / gas 四条都带「为什么不存在」的规则",
        "没有从「Build 结果相同」反推「eth_call 结果相同」：所有相等比较都在同种已记录答案之间做",
        "没有受控 A/B 复用实验（那是 M8.5 后续阶段，且必须先有本阶段的资格结论）",
    ] {
        push(&mut lines, &format!("- {item}"));
    }
    push(&mut lines, "");
    push(&mut lines, "## 这套证据回答不了什么");
    push(&mut lines, "");
    for item in [
        "**答案本身没被记录**：trace 记的是请求，不是返回字节，所以「两次读数是否逐字节相同」只能在 artifact 层比（reserve0/reserve1 这些字段），不能在线上比",
        "**块内路径不可归因**：字节码里扫不到区块上下文 opcode，可以证明这个合约的读法与块无关；反过来不行——扫到 TIMESTAMP 不等于 getReserves() 走得到它",
    ] {
        push(&mut lines, &format!("- {item}"));
    }
    push(
        &mut lines,
        &format!(
            "- **冷池之外没有第二个样本**：短距相等的对照只有「链上确实动过」的远距那把，同一口池、\
                同一 calldata，样本就是那 {} 个；要更多样本就得真跑，那是后续阶段的事",
            shown(&dependency["totals"]["cross_height_controls"])
        ),
    );
    push(
        &mut lines,
        "- **所有权不是记录字段**：谁能持有/失效一份答案，只能从代码结构判定；这条判定对生产代码零改动",
    );
    push(&mut lines, "");
    push(&mut lines, "## 怎么读锚点");
    push(&mut lines, "");
    push(
        &mut lines,
        &format!(
            "每张表的 `evidence_refs` 都是装配时**解析出来的行号**：token 在生产区必须恰好命中一行，\
                命中两行就装配失败。代码改了位置、表没重刷，门就红——这是 M8.4.3 立下的规矩，本轮沿用。\
                本轮共发布 {} 个锚点。",
            shown(&json!(published_refs(root))),
        ),
    );
    push(&mut lines, "");
    push(&mut lines, "## 重新生成");
    push(&mut lines, "");
    push(
        &mut lines,
        "```bash
M851_ETH_CALL_SEMANTICS_REFRESH=1 cargo test -p evm-pipeline --test eth_call_semantics_evidence -- --test-threads=1
```
装配只读 `data/evidence/m8/cross-stage/`、`data/evidence/m7/`、`fixtures/simulation-m7/` 里的\
                已提交记录，不打开任何 socket。表内不含时钟字段，所以两次装配逐字节相同。",
    );
    push(&mut lines, "");
    push(&mut lines, "---");
    push(
        &mut lines,
        &format!(
            "生成者：`{WRITER}`；模型：`{MODEL_FILE}`。本 README 由同一装配写出，不在表里手抄数字。"
        ),
    );
    format!("{}\n", lines.join("\n"))
}

fn push(lines: &mut Vec<String>, text: &str) {
    lines.push(text.to_string());
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn published_refs(root: &Value) -> usize {
    let mut arrays = Vec::new();
    all_ref_arrays(root, &mut arrays);
    arrays.iter().map(Vec::len).sum()
}

fn all_ref_arrays(value: &Value, out: &mut Vec<Vec<Value>>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == "evidence_refs" {
                    if let Value::Array(list) = child {
                        out.push(list.clone());
                    }
                }
                all_ref_arrays(child, out);
            }
        }
        Value::Array(values) => {
            for child in values {
                all_ref_arrays(child, out);
            }
        }
        _ => {}
    }
}

/// Every anchor the model declares, in one place, so the tables and the gate cannot disagree
/// about which anchors the model owns.
fn model_anchors() -> Vec<&'static Anchor> {
    call_sites()
        .iter()
        .flat_map(|site| site.anchors.iter())
        .chain(SELECTORS.iter().flat_map(|fact| fact.anchors.iter()))
        .collect()
}

fn declared_anchors() -> usize {
    model_anchors().len()
}

// ---------------------------------------------------------------------------
// assembly
// ---------------------------------------------------------------------------

fn assemble_to(dir: &Path) -> Vec<String> {
    let mut tree = Tree::new();
    write_table(&dir.join(ETH_CALL_FILES[0]), &call_surface(&mut tree));
    write_table(
        &dir.join(ETH_CALL_FILES[1]),
        &normalized_identities(&mut tree),
    );
    write_table(&dir.join(ETH_CALL_FILES[2]), &ownership_matrix(&mut tree));
    write_table(
        &dir.join(ETH_CALL_FILES[3]),
        &lifecycle_contracts(&mut tree),
    );
    write_table(&dir.join(ETH_CALL_FILES[4]), &dependency_matrix(&mut tree));
    write_table(&dir.join(ETH_CALL_FILES[5]), &negative_controls(&mut tree));
    write_table(&dir.join(ETH_CALL_FILES[6]), &reuse_verdicts(&mut tree));

    let root = read_root(dir);
    write_text(&dir.join(README_FILE), &build_readme(&root));

    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// gates
// ---------------------------------------------------------------------------

/// §34's byte gate: the committed directory is one assembly of the model and the records, and
/// nothing else is in it.
#[test]
fn the_committed_directory_is_a_reassembly_of_the_model_and_the_records() {
    let dir = scratch("m851-byte-gate");
    let names = assemble_to(&dir);
    assert_eq!(
        names,
        generated_files(),
        "not the file set one assembly of the model and the records writes"
    );

    if refreshing() {
        let committed = evidence_dir();
        std::fs::create_dir_all(&committed)
            .unwrap_or_else(|error| panic!("{}: {error}", committed.display()));
        for name in &names {
            std::fs::copy(dir.join(name), committed.join(name)).unwrap_or_else(|error| {
                panic!("{name}: the refresh could not be committed: {error}")
            });
        }
        eprintln!("refreshed {EVIDENCE_DIR}: {}", names.join(", "));
        return;
    }

    let committed = evidence_dir();
    let mut committed_files: Vec<String> = std::fs::read_dir(&committed)
        .unwrap_or_else(|error| panic!("{}: {error}", committed.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    committed_files.sort();
    assert_eq!(
        committed_files, names,
        "the committed directory holds a different file set than the model and the records write"
    );
    for name in &names {
        let fresh = read_bytes(&dir.join(name));
        let committed_bytes = read_bytes(&committed.join(name));
        assert_eq!(
            fresh, committed_bytes,
            "{name}: the committed bytes are not what one assembly of the model and the records \
             produces — refresh with M851_ETH_CALL_SEMANTICS_REFRESH=1"
        );
    }

    // A second assembly into a different scratch directory is byte-identical, which is the only
    // way "no clock in the tables" is a checked claim rather than a sentence.
    let again = scratch("m851-byte-gate-again");
    let second_names = assemble_to(&again);
    assert_eq!(second_names, names);
    for name in &names {
        assert_eq!(
            read_bytes(&dir.join(name)),
            read_bytes(&again.join(name)),
            "{name}: two assemblies in the same tree differ, so a clock or an unordered map \
             leaked into the tables"
        );
    }
}

/// §10/§35's traceability: every anchor the model declares is published, every published
/// reference resolves to a line that exists today, and no reference is published without a note.
/// The count is checked as a floor against the model rather than against a constant, because a
/// constant here would be a number someone typed.
#[test]
fn every_anchor_the_model_declares_is_published_with_a_resolved_line() {
    let root = read_root(&evidence_dir());
    let mut arrays = Vec::new();
    all_ref_arrays(&root, &mut arrays);
    let published: Vec<&Value> = arrays.iter().flatten().collect();

    let model = declared_anchors();
    assert!(
        published.len() >= model,
        "the tables publish {} references while the model declares {model} anchors, so an anchor \
         the model claims is not in the evidence at all",
        published.len()
    );
    let published_pairs: BTreeSet<(String, String)> = published
        .iter()
        .map(|reference| {
            (
                reference["file"].as_str().unwrap_or_default().to_string(),
                reference["token"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    for anchor in model_anchors() {
        assert!(
            published_pairs.contains(&(anchor.file.to_string(), anchor.token.to_string())),
            "{}:{} is declared by the model and published by no table",
            anchor.file,
            anchor.token
        );
    }
    for reference in published {
        assert!(
            reference["line"].as_u64().unwrap_or_default() > 0,
            "a reference resolved to no line: {reference}"
        );
        assert!(
            !reference["note"].as_str().unwrap_or_default().is_empty(),
            "a reference carries no claim: {reference}"
        );
    }
}

/// §39's recount: the surface table is the records counted, and a figure that drifts from the
/// committed record files fails here rather than in a report.
#[test]
fn the_surface_is_the_records_counted() {
    let root = read_root(&evidence_dir());
    let surface = &root["call-surface"];
    let asks = asks();
    assert_eq!(
        surface["totals"]["eth_call_asks"]
            .as_u64()
            .unwrap_or_default() as usize,
        asks.len(),
        "the published ask count is not the record count"
    );
    let mut row_total = 0;
    for row in rows_of(surface) {
        row_total += row["asks"].as_u64().unwrap_or_default();
    }
    assert_eq!(
        row_total as usize,
        asks.len(),
        "the per-site rows do not sum to the asks the records hold"
    );
    assert_eq!(
        surface["totals"]["asks_of_every_method_in_the_same_records"]
            .as_u64()
            .unwrap_or_default() as usize,
        total_asks(),
        "the denominator is not the records' own total either"
    );
    // The published counts reconcile against M8.4.2's own summary, which this milestone did not
    // write: the eth_call asks it counted are the asks here.
    let summary = read_json(&repo_path(SUMMARY_RECORD));
    let per_run_asks: usize = summary["per_run"]
        .as_array()
        .expect("the summary lists its runs")
        .iter()
        .map(|run| run["asks"].as_u64().unwrap_or_default() as usize)
        .sum();
    assert_eq!(
        per_run_asks,
        total_asks(),
        "the runs M8.4.2 summarised are not the runs read here"
    );
    assert_eq!(
        surface["totals"]["selectors_not_in_the_table"]
            .as_u64()
            .unwrap_or_default(),
        0,
        "a recorded selector is not in the knowledge table, so its artifact class would be a \
         guess"
    );
}

/// §36 as a gate: no identity field is a bare null, and no absence is claimed without a rule.
#[test]
fn no_identity_field_is_a_bare_null() {
    let root = read_root(&evidence_dir());
    for row in rows_of(&root["normalized-identities"]) {
        for field in ["from", "value", "state_override", "gas"] {
            let cell = &row[field];
            assert_ne!(
                cell,
                &Value::Null,
                "{field} is a bare null, which §36 forbids: a missing field must say which kind \
                 of nothing it is"
            );
            let state = cell["state"].as_str().unwrap_or_default();
            assert!(
                [
                    "proven_absent",
                    "recorded",
                    "not_applicable",
                    "not_recorded",
                    "unknown"
                ]
                .contains(&state),
                "{field} carries {state:?}, outside the published vocabulary"
            );
            assert!(
                cell["settled"].as_bool().unwrap_or(false),
                "{field} reads as {state:?}, which is not a settled answer"
            );
            assert!(
                cell["detail"].is_string(),
                "{field} is settled without naming the rule or the value"
            );
        }
    }
    // The parameters table states the same four rules with the code beside them.
    assert_eq!(
        root["call-surface"]["request_parameters"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        4
    );
}

/// §41's key control, measured: the block term changes the answer, so cross-block reuse is not \
/// safe — and the short-gap equality is not allowed to stand in for that finding.
#[test]
fn the_block_term_is_measured_load_bearing_and_the_short_gap_is_not_read_as_independence() {
    let root = read_root(&evidence_dir());
    let dependency = &root["dependency-matrix"];
    let control = dependency["cross_height_control"]
        .as_array()
        .expect("the cross-height control has rows");
    assert_eq!(
        control.len(),
        2,
        "both candidate pools are read at both heights"
    );
    for row in control {
        assert_eq!(
            row["answer_equal"],
            json!(false),
            "{} answered the same at {} and {}, which would remove the measured basis for the \
             cross-block refusal",
            row["pool"],
            row["earlier"]["height"],
            row["later"]["height"]
        );
        assert!(
            row["blocks_apart"].as_u64().unwrap_or_default() > 1_000,
            "the control pair sits {} blocks apart, which is not far enough to expect a move",
            row["blocks_apart"]
        );
    }
    let within = dependency["decoded_answers_within_a_run"]
        .as_array()
        .expect("the within-run comparisons are published");
    assert_eq!(within.len(), 6);
    for row in within {
        assert!(
            row["answer_equal"].as_bool().unwrap_or(false),
            "the pin and the head of {} disagree within one run, which would make the recorded \
             gate's equality claim false",
            row["pool"]
        );
        assert!(
            row["blocks_apart"].as_u64().unwrap_or_default() < 100,
            "{} is not the short-gap case this table is bounding",
            row["pool"]
        );
    }
    // The caveat has to be in the file, not in a sentence elsewhere.
    let census = &dependency["activity_census"];
    for pool in census["candidate_pools"]
        .as_array()
        .expect("the census names both candidate pools")
    {
        assert_eq!(
            pool["in_census"],
            json!(false),
            "{} does appear in the activity census, so the cold-pool caveat would be wrong",
            pool["pool"]
        );
    }
    assert!(census["what_it_bounds"]
        .as_str()
        .unwrap_or_default()
        .contains(
            "does \
not show the answer is block-independent"
        ));
}

/// §28/§58: the verdict set is complete and no cell is left unjudged.
#[test]
fn every_site_and_every_measured_pair_carries_a_class_and_a_named_blocker() {
    let root = read_root(&evidence_dir());
    let verdicts = &root["reuse-verdicts"];
    assert_eq!(
        verdicts["sites"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        call_sites().len()
    );
    for site in verdicts["sites"].as_array().expect("site verdicts") {
        assert_eq!(
            site["axes"].as_array().map(Vec::len).unwrap_or_default(),
            ALL_AXES.len(),
            "{} is missing an axis, so §26's rule cannot be applied to it",
            site["site"]
        );
        if !site["safe_to_reuse_now"].as_bool().unwrap_or(false) {
            assert!(
                !site["blockers"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .is_empty(),
                "{} is not safe and names nothing blocking it",
                site["site"]
            );
        }
    }
    assert_eq!(
        verdicts["totals"]["safe_to_reuse_now"]
            .as_u64()
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        verdicts["totals"]["pairs_reuse_ready"]
            .as_u64()
            .unwrap_or_default(),
        0
    );
    for pair in verdicts["pairs"].as_array().expect("pair verdicts") {
        assert_ne!(
            pair["this_milestone_class"],
            json!(ReuseClass::ReuseReady.as_str()),
            "{} is suddenly reuse-ready, which the model does not claim",
            pair["candidate_id"]
        );
        assert!(
            !pair["blockers"]
                .as_array()
                .unwrap_or(&Vec::new())
                .is_empty(),
            "{} is blocked without a reason",
            pair["candidate_id"]
        );
    }
    assert_eq!(
        verdicts["outcome"]["label"].as_str(),
        Some("REUSE_BLOCKED"),
        "the outcome label is one §58 does not recognise"
    );
    assert_eq!(
        verdicts["outcome"]["net_rpc_saving"]
            .as_u64()
            .unwrap_or_default(),
        0,
        "§31: this stage proves eligibility and optimises nothing"
    );
}

/// §49/§50/§56 as a gate rather than a claim: this milestone's code is read by tests only, it \
/// adds no call to any endpoint, and the tables carry the accounting.
#[test]
fn the_diagnosis_is_a_reader_and_not_a_writer() {
    // 1. Nothing in any crate's production tree references the module.
    let mut offenders: Vec<String> = Vec::new();
    for crate_name in [
        "chain",
        "core",
        "execution",
        "graph",
        "opportunity",
        "pipeline",
        "protocol",
        "risk",
        "simulation",
    ] {
        let src = repo_path(&format!("crates/{crate_name}/src"));
        if !src.is_dir() {
            continue;
        }
        for path in walk_files(&src) {
            let text = read_text(&path);
            if path
                .file_name()
                .map(|name| name == "eth_call_semantics.rs")
                .unwrap_or(false)
            {
                continue;
            }
            for line in text
                .lines()
                .filter(|line| line.contains("eth_call_semantics"))
            {
                // The module declaration in the pipeline's own lib.rs is the only production
                // reference a diagnostic-only module can have, and it makes none.
                if line.trim().starts_with("pub mod eth_call_semantics;") {
                    continue;
                }
                offenders.push(format!("{}: {}", path.display(), line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a diagnostic module that production code calls is a production change: {offenders:?}"
    );

    // 2. The published accounting says what this milestone did not do.
    let root = read_root(&evidence_dir());
    let verdicts = &root["reuse-verdicts"];
    assert_eq!(
        verdicts["outcome"]["net_rpc_saving"]
            .as_u64()
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        verdicts["outcome"]["candidates_found"]
            .as_u64()
            .unwrap_or_default(),
        candidate_rows().len() as u64
    );

    // 3. No new cache, batch, prefetch or concurrency is even mentioned as done — the tables
    //    describe a diagnosis, and any optimisation word would be a claim about code this
    //    milestone did not write.
    let text = read_text(&repo_path(MODEL_FILE));
    for forbidden in [
        "StateReadCache",
        "insert_or_update",
        "join_all",
        "buffer_unordered",
    ] {
        assert!(
            !text.contains(forbidden),
            "{MODEL_FILE} names {forbidden}, which would mean this stage touched the reuse or \
             concurrency paths §30/§32 forbid"
        );
    }
}

/// §53/§54's shape: the tables use the published vocabularies and invent no words. The
/// block-dependency words are read out of the dependency matrix's own published vocabulary, so a
/// word invented in one table and absent from the other fails here rather than in prose.
#[test]
fn the_tables_use_the_published_vocabulary() {
    let root = read_root(&evidence_dir());
    let statuses = ["proven", "partially_proven", "unknown", "not_applicable"];
    let dependency_words: Vec<&str> = root["dependency-matrix"]["dependency_vocabulary"]
        .as_array()
        .expect("the dependency matrix publishes its vocabulary")
        .iter()
        .map(|word| word.as_str().unwrap_or_default())
        .collect();
    assert_eq!(dependency_words.len(), 3);
    let mut checked = 0;
    for row in rows_of(&root["ownership-matrix"]) {
        for axis in row["axes"].as_array().expect("axes per row") {
            let status = axis["status"].as_str().unwrap_or_default();
            assert!(
                statuses.contains(&status),
                "{status:?} is outside the four statuses §53 allows"
            );
            checked += 1;
        }
        assert!(
            dependency_words.contains(&row["block_dependency"].as_str().unwrap_or("")),
            "{}: {:?} is outside the block-dependency vocabulary {dependency_words:?}",
            row["site"],
            row["block_dependency"]
        );
    }
    assert_eq!(checked, call_sites().len() * ALL_AXES.len());
    for cell in root["lifecycle-contracts"]["sites"]
        .as_array()
        .expect("lifecycle sites")
        .iter()
        .flat_map(|site| site["steps"].as_array().cloned().unwrap_or_default())
    {
        assert!(
            statuses.contains(&cell["status"].as_str().unwrap_or_default()),
            "a lifecycle cell carries an unpublished status: {cell}"
        );
    }
}

/// §17/§18's asymmetry, checked against the offline bytecode rather than asserted: these pools do
/// read block context somewhere, so the scan cannot grant independence, and the sites that lean on
/// the scan are still not safe.
#[test]
fn the_offline_scan_shows_presence_that_cannot_be_attributed() {
    let root = read_root(&evidence_dir());
    let scans = root["dependency-matrix"]["bytecode_scans"]
        .as_array()
        .expect("the scans are published");
    assert_eq!(scans.len(), 4, "two pools at two dumped heights");
    let code_bytes: BTreeSet<u64> = scans
        .iter()
        .map(|row| row["code_bytes"].as_u64().unwrap_or_default())
        .collect();
    assert_eq!(
        code_bytes.len(),
        1,
        "the pools' code length moved between the two dumped heights, so a scan of one height \
         would not describe the other"
    );
    for row in scans {
        assert!(
            row["block_context_reachable"].as_bool().unwrap_or(false),
            "{} shows no block-context opcode, which would be a proof of independence and would \
             contradict the site table's not_attributable",
            row["address"]
        );
        assert!(
            row["state_opcodes"]
                .as_object()
                .map(|map| !map.is_empty())
                .unwrap_or(false),
            "{} reads no state at all, which cannot be true of a pair",
            row["address"]
        );
    }
    // The model's own asymmetry rule, published and enforced by the same table.
    assert!(
        root["dependency-matrix"]["opcode_tables"]["why_a_linear_walk_is_enough"]
            .as_str()
            .unwrap_or_default()
            .contains("can never revoke it")
    );
}

/// The README echoes the tables rather than restating them by hand.
#[test]
fn the_readme_figures_are_the_tables_figures() {
    let root = read_root(&evidence_dir());
    let readme = read_text(&evidence_dir().join(README_FILE));
    let surface = &root["call-surface"];
    for figure in [
        shown(&surface["totals"]["eth_call_asks"]),
        shown(&surface["totals"]["asks_of_every_method_in_the_same_records"]),
        shown(&root["normalized-identities"]["totals"]["distinct_identities_with_block"]),
        shown(&root["normalized-identities"]["totals"]["distinct_identities_without_block"]),
        shown(&root["lifecycle-contracts"]["totals"]["cells"]),
        shown(&root["dependency-matrix"]["totals"]["pairs"]),
        shown(&root["reuse-verdicts"]["totals"]["safe_to_reuse_now"]),
    ] {
        assert!(
            readme.contains(&figure),
            "the README never prints {figure}, so either the README or the table has gone stale"
        );
    }
    assert!(
        readme.contains("净省 RPC = **0**"),
        "the headline number is missing"
    );
    assert!(
        readme.contains("冷池"),
        "the caveat that bounds the equality is missing from the plain-language section"
    );
    for name in generated_files() {
        if name == README_FILE {
            continue;
        }
        assert!(
            readme.contains(&format!("`{name}`")),
            "the README does not list {name}"
        );
    }
}

/// A negative control for the gate itself: a wrong blocker word, a missing anchor note, or a pair
/// claimed safe would all be caught by the tables rather than by prose.
#[test]
fn a_claim_of_safety_would_have_to_survive_the_axis_rule() {
    for site in call_sites() {
        let verdict = verdicts()
            .into_iter()
            .find(|row| row.site == site.id)
            .expect("every site is judged");
        let proven = ALL_AXES
            .iter()
            .filter(|axis| {
                verdict
                    .axes
                    .iter()
                    .any(|row| row.axis == **axis && row.status == ProofStatus::Proven)
            })
            .count();
        if proven == ALL_AXES.len() {
            assert!(
                verdict.safe_to_reuse_now,
                "{} has all eight axes proven and is still refused, which the rule forbids",
                site.id
            );
        } else {
            assert!(
                !verdict.safe_to_reuse_now,
                "{} is safe with only {} of {} axes proven",
                site.id,
                proven,
                ALL_AXES.len()
            );
        }
    }
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{dir:?}: {error}"));
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}
