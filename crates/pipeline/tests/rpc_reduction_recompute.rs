//! M8.6 §24's second gate: can this directory be rebuilt from the raw records without the model?
//!
//! # Why a second gate, and why it imports nothing
//!
//! [`rpc_reduction_evidence`] checks the eleven files against `m86_census` — the model, the assembly,
//! and the fold. That is one implementation reading its own output. §24's whole claim is that the
//! figures are 可回算, and a claim about recomputability is worth only what an *independent*
//! recomputation makes it worth. So this file imports no module: it re-implements the grouping, the
//! caller masking, the identity grammar, the duplicate fold and the row rules from the record fields
//! up, and compares what it gets with what the tables print. Where the two disagree, one of them is
//! wrong, and this gate cannot tell which — that is the point of writing it twice. The union fold in
//! [`the_duration_columns_are_the_records_own_intervals_merged`] is the check that earns its keep on
//! this design: the first version of the model's merge overwrote each run's total instead of adding
//! to it, and a single implementation could not have said so.
//!
//! # What it re-derives
//!
//! * 246 asks in three run records → 44 site groups, each group's count, its per-run split, and the
//!   number of distinct recorded keys the group holds;
//! * each group's published identity string, rebuilt by splitting the record's own `dedup_key` into
//!   segments and rendering them in alphabetical order;
//! * 78 pair rows → each candidate's exact-duplicate count, and the 42 that is the census's and the
//!   matrix's and §7's first measure;
//! * the safe column, which has nothing behind it: 0 of 78 pairs have all five reuse conditions met
//!   and 0 are marked safe by the record, so no candidate can print a number above 0;
//! * every `totals`/`counts` block in the directory, as the arithmetic of the rows it prints;
//! * §25's six negative controls, run here rather than read from the file that reports them, plus
//!   §24's fourteenth rule in the form only a recompute gate can state — a hand-edited row has to
//!   break the reconciliation with the records, not merely the row's own internal rules;
//! * §26's zero: nothing in the directory was measured by asking the node anything, and no clock
//!   decides a judgement.
//!
//! # How these files change
//!
//! They do not, from here. This gate only reads. The committed directory is rewritten by
//! `M86_CENSUS_REFRESH=1 cargo test -p evm-pipeline --test rpc_reduction_evidence`, and that gate's
//! own byte test is what proves a refresh reproduces what is committed.

#![recursion_limit = "2048"]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

// ---------------------------------------------------------------------------
// the corpus, by relative path
// ---------------------------------------------------------------------------

/// The directory under test.
const EVIDENCE_DIR: &str = "data/evidence/m8/m8.6";
/// §4's traffic: the three M8.4.2 live BuildOnly runs, 82 recorded asks each.
const RUNS_DIR: &str = "data/evidence/m8/cross-stage/runs";
const CALLS_FILE: &str = "pipeline-calls.json";
/// M8.4.2's pair rows — §2.2's three layers, measured before this milestone existed.
const PAIRS_FILE: &str = "data/evidence/m8/cross-stage/reuse-candidates.json";

const CENSUS: &str = "rpc-census.json";
const SURFACE: &str = "rpc_surface.json";
const CANDIDATES: &str = "rpc-reduction-candidates.json";
const MATRIX: &str = "reduction-matrix.json";
const REJECTED: &str = "rejected-opportunities.json";
const QUEUE: &str = "priority-queue.json";
const SAVING: &str = "saving-kinds.json";
const CONTROLS: &str = "negative-controls.json";
const FLOW: &str = "information_flow.json";
const VERDICT: &str = "final-verdict.json";

/// Every JSON file the gate reads. §22's one label and §26's zero are checked against all ten.
const FILES: [&str; 10] = [
    CENSUS, SURFACE, CANDIDATES, MATRIX, REJECTED, QUEUE, SAVING, CONTROLS, FLOW, VERDICT,
];

// ---------------------------------------------------------------------------
// the key this gate rebuilds: (method, stage, masked caller)
// ---------------------------------------------------------------------------

/// The stage or caller a record leaves unstamped.
const UNSTAMPED: &str = "<unstamped>";
/// What a caller label's address-shaped runs become.
const MASKED_ADDRESS: &str = "<addr>";
/// What `step 3` and `nonce 12` become.
const MASKED_INDEX: &str = "N";
/// The shortest `0x` run this gate masks. Below it, a four-byte selector is data the label keeps:
/// `views: token0()` and `views: token1()` are two sites, and a key that merged them would merge two
/// candidates' evidence without saying so.
const MIN_MASKED_HEX: usize = 8;
/// What an ask the record carries no `dedup_key` for counts as. Spelled out here rather than
/// imported, so the model cannot redefine the disclosure by rewording a constant.
const UNKEYED: &str = "<no recorded key>";

/// A call site as this gate identifies it: three record fields, one of them masked. A presentation
/// key, never an identity — the identity is [`identity_of`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    method: String,
    stage: String,
    caller: String,
}

impl Site {
    fn label(&self) -> String {
        format!("{}|{}|{}", self.method, self.stage, self.caller)
    }

    fn of_row(row: &Value) -> Self {
        Self {
            method: txt(row, "method"),
            stage: opt_txt(row, "stage").unwrap_or_else(|| UNSTAMPED.to_string()),
            caller: mask_caller(opt_txt(row, "caller").as_deref()),
        }
    }

    /// A census, surface, or flow row's own view of the site it describes.
    fn of_table_row(row: &Value) -> Self {
        Self {
            method: txt(row, "method"),
            stage: opt_txt(row, "stage").unwrap_or_else(|| UNSTAMPED.to_string()),
            caller: txt(row, "caller"),
        }
    }

    /// One side of a published pair. The method is the pair's, not the side's: M8.4.2 keys a pair by
    /// the one identity both asks share, and stores it once on the row.
    fn of_pair_side(pair: &Value, side: &str) -> Self {
        let object = &pair[side];
        Self {
            method: txt(&pair["identity"], "method"),
            stage: opt_txt(object, "stage").unwrap_or_else(|| UNSTAMPED.to_string()),
            caller: mask_caller(opt_txt(object, "caller").as_deref()),
        }
    }
}

/// `step <digits>`, `nonce <digits>`, and an address-shaped `0x` run collapse; everything else,
/// including the selector text, survives. Written from the rule rather than copied from the model,
/// and the site-count test below is what shows the two implementations agree.
fn mask_caller(caller: Option<&str>) -> String {
    let Some(text) = caller else {
        return UNSTAMPED.to_string();
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        let mut consumed = false;
        for word in ["step ", "nonce "] {
            let prefix: Vec<char> = word.chars().collect();
            let after = index + prefix.len();
            if after > chars.len() || chars[index..after] != prefix[..] {
                continue;
            }
            let mut tail = after;
            while tail < chars.len() && chars[tail].is_ascii_digit() {
                tail += 1;
            }
            if tail > after {
                out.push_str(word);
                out.push_str(MASKED_INDEX);
                index = tail;
                consumed = true;
                break;
            }
        }
        if consumed {
            continue;
        }
        if chars[index..].starts_with(&['0', 'x'][..]) {
            let mut tail = index + 2;
            while tail < chars.len() && chars[tail].is_ascii_hexdigit() {
                tail += 1;
            }
            if tail - (index + 2) >= MIN_MASKED_HEX {
                out.push_str(MASKED_ADDRESS);
                index = tail;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// the identity this gate rebuilds: the record's own `dedup_key`, re-rendered
// ---------------------------------------------------------------------------

/// The terms M8.4.2's key grammar builds, and the segment each `dedup_key` class puts them in. A
/// class this function does not know panics rather than returning an empty term set: an identity this
/// gate cannot rebuild is an identity it cannot check, and the difference matters.
fn terms_of(key: &str) -> BTreeMap<String, String> {
    let parts: Vec<&str> = key.split('|').collect();
    let say = |index: usize| {
        parts
            .get(index)
            .unwrap_or_else(|| panic!("{key:?}: too few segments for a {:?} key", parts.first()))
            .to_string()
    };
    let mut terms = BTreeMap::new();
    match parts[0] {
        "storage" => {
            assert!(
                parts.len() == 5,
                "{key:?}: storage is kind|chain|block|address|slot"
            );
            terms.insert("address".to_string(), say(3));
            terms.insert("slot".to_string(), say(4));
        }
        "balance" | "code" | "nonce" => {
            assert!(
                parts.len() == 4,
                "{key:?}: an account key is kind|chain|block|address"
            );
            terms.insert("address".to_string(), say(3));
        }
        "call" => {
            assert!(
                parts.len() == 5,
                "{key:?}: a call key is kind|chain|block|to|data"
            );
            terms.insert("to".to_string(), say(3));
            terms.insert("data".to_string(), say(4));
        }
        "block" => {
            assert!(
                parts.len() == 5 && parts[3] == "hydrated",
                "{key:?}: a block key is kind|chain|block|hydrated|<bool>"
            );
            terms.insert("hydrated".to_string(), say(4));
        }
        other => panic!("{key:?}: a key class {other:?} this gate has no grammar for"),
    }
    terms.insert("chain".to_string(), say(1));
    terms.insert("block".to_string(), say(2));
    terms
}

/// One ask's identity, rebuilt from its own record row: `method=` first, then the key's terms in
/// alphabetical order. An ask with no recorded key is one of §4's parameterless requests, whose whole
/// business content is which method of which endpoint it is.
fn identity_of(row: &Value) -> String {
    let method = txt(row, "method");
    match row["dedup_key"].as_str() {
        None => format!(
            "method={method}|endpoint={}|method_kind={method}",
            txt(row, "endpoint_id")
        ),
        Some(key) => {
            let rendered: Vec<String> = terms_of(key)
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            format!("method={method}|{}", rendered.join("|"))
        }
    }
}

/// The group member whose recorded key the census publishes as the group's identity. The choice is a
/// function of the group's own key set and of nothing else — the smallest recorded `dedup_key`, and
/// for a group no row keys (only a parameterless method can be one) the smallest `(run, rpc_id)` —
/// so re-reading the same records in another order cannot move a printed identity. That is §24's
/// twelfth rule stated as a selection rule rather than as a claim about the output.
fn representative(members: &[Value]) -> &Value {
    let rank = |row: &Value| {
        (
            u8::from(row["dedup_key"].as_str().is_none()),
            row["dedup_key"].as_str().unwrap_or_default().to_string(),
            txt(row, "run"),
            row["rpc_id"].as_u64().unwrap_or_default(),
        )
    };
    members
        .iter()
        .min_by_key(|row| rank(row))
        .unwrap_or_else(|| panic!("a group with no records has no identity to publish"))
}

/// The `(name, value)` pairs of a printed identity string, in the order they print.
fn identity_terms(identity: &str) -> Vec<(String, String)> {
    identity
        .split('|')
        .filter(|term| !term.is_empty())
        .map(|term| match term.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (term.to_string(), String::new()),
        })
        .collect()
}

/// The term names M8.4.2's grammar builds. A term outside this list is a clock, a position, or a
/// field this gate has not read the code for — and all three are reasons to stop, not to guess.
const IDENTITY_TERMS: [&str; 11] = [
    "method",
    "chain",
    "block",
    "to",
    "data",
    "value",
    "address",
    "slot",
    "hydrated",
    "endpoint",
    "method_kind",
];

// ---------------------------------------------------------------------------
// the clock fold, rebuilt
// ---------------------------------------------------------------------------

/// One site's timing, folded the way §24's thirteenth rule needs it folded: merged within a run,
/// because a run's stamps share one monotonic origin, then added across runs, because two runs' do
/// not.
#[derive(Clone, Debug, Default)]
struct Fold {
    asks: u64,
    sum_ns: u64,
    union_ns: u64,
    attempts: u64,
    overlaps: usize,
    rows_without_interval: usize,
}

fn ms(ns: u64) -> f64 {
    (ns as f64 / 1_000_000f64 * 100f64).round() / 100f64
}

fn fold_times(rows: &[Value]) -> BTreeMap<String, Fold> {
    let mut sums: BTreeMap<String, Fold> = BTreeMap::new();
    let mut intervals: BTreeMap<(String, String), Vec<(u64, u64)>> = BTreeMap::new();
    for row in rows {
        let label = Site::of_row(row).label();
        let fold = sums.entry(label.clone()).or_default();
        fold.asks += 1;
        fold.sum_ns += count(row, "duration_ns");
        fold.attempts += row["attempts"]
            .as_array()
            .map_or(1, |list| list.len() as u64);
        match (row["started_ns"].as_u64(), row["finished_ns"].as_u64()) {
            (Some(started), Some(finished)) if finished >= started => {
                intervals
                    .entry((label, txt(row, "run")))
                    .or_default()
                    .push((started, finished));
            }
            _ => fold.rows_without_interval += 1,
        }
    }
    for ((label, _run), mut list) in intervals {
        let fold = sums
            .get_mut(&label)
            .expect("a label is counted with its own intervals");
        list.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (started, finished) in list {
            match merged.last_mut() {
                Some(last) if started <= last.1 => {
                    fold.overlaps += 1;
                    if finished > last.1 {
                        last.1 = finished;
                    }
                }
                _ => merged.push((started, finished)),
            }
        }
        fold.union_ns += merged
            .iter()
            .map(|(started, finished)| finished - started)
            .sum::<u64>();
    }
    sums
}

// ---------------------------------------------------------------------------
// reading, and the small typed views over a JSON value
// ---------------------------------------------------------------------------

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(relative: &str) -> Value {
    let path = if Path::new(relative).is_absolute() {
        PathBuf::from(relative)
    } else {
        repo().join(relative)
    };
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn published(name: &str) -> Value {
    read_json(&format!("{EVIDENCE_DIR}/{name}"))
}

/// Every numeric field in a record whose name claims an RPC saving, collected as `path = value`.
/// §15/§16's inherited verdicts are only inherited if the record they cite really says zero.
fn rpc_zero_fields(value: &Value, path: &str, out: &mut Vec<(String, u64)>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let here = format!("{path}/{key}");
                match child.as_u64() {
                    Some(number) => {
                        let name = key.to_lowercase();
                        if name.contains("rpc")
                            && (name.contains("saved")
                                || name.contains("saving")
                                || name.contains("reduction"))
                        {
                            out.push((here, number));
                        }
                    }
                    None => rpc_zero_fields(child, &here, out),
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                rpc_zero_fields(item, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn rows_of(name: &str) -> Vec<Value> {
    let table = published(name);
    table["rows"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{}: no `rows` array", table["file"]))
}

/// The three run directories, sorted by name.
fn run_directories() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo().join(RUNS_DIR))
        .expect("the runs directory is part of the committed corpus")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// All 246 recorded asks, each carrying the run directory it was read out of. The gate trusts a
/// file's own `run` column only after checking it names that directory, so a row copied from one run
/// into another cannot quietly move an interval onto a different clock.
fn raw_rows() -> Vec<Value> {
    let mut found = Vec::new();
    for run in run_directories() {
        let table = read_json(&format!("{RUNS_DIR}/{run}/{CALLS_FILE}"));
        for row in table["rows"].as_array().expect("a run's rows") {
            assert_eq!(
                opt_txt(row, "run").as_deref(),
                Some(run.as_str()),
                "{run}: a row's own `run` column names {:?}",
                opt_txt(row, "run")
            );
            let mut row = row.clone();
            row["_directory"] = Value::String(run.clone());
            found.push(row);
        }
    }
    found
}

fn pairs() -> Vec<Value> {
    read_json(PAIRS_FILE)["rows"]
        .as_array()
        .expect("the pair record")
        .clone()
}

fn txt(row: &Value, field: &str) -> String {
    opt_txt(row, field).unwrap_or_else(|| panic!("{field} is not a string in {row}"))
}

fn opt_txt(row: &Value, field: &str) -> Option<String> {
    match &row[field] {
        Value::Null => None,
        value => value.as_str().map(str::to_string),
    }
}

fn count(row: &Value, field: &str) -> u64 {
    row[field]
        .as_u64()
        .unwrap_or_else(|| panic!("{field} is not a count in {row}"))
}

/// A published `{key: count}` object, read as a map so a printing order can never be an argument.
fn counts(value: &Value) -> BTreeMap<String, u64> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected a count object, found {value}"))
        .iter()
        .map(|(key, cell)| (key.clone(), cell.as_u64().unwrap_or_default()))
        .collect()
}

fn tally(values: &[String]) -> BTreeMap<String, u64> {
    let mut map: BTreeMap<String, u64> = BTreeMap::new();
    for value in values {
        *map.entry(value.clone()).or_insert(0) += 1;
    }
    map
}

/// The census's rows, indexed by the `site_id` the candidate table uses to name a producer or a
/// consumer. Every fold below enters a site through this map, so a candidate naming a site the
/// census does not publish is caught here rather than folding quietly to zero.
fn sites_by_id() -> BTreeMap<String, Site> {
    let mut map = BTreeMap::new();
    for row in rows_of(CENSUS) {
        let previous = map.insert(txt(&row, "site_id"), Site::of_table_row(&row));
        assert!(
            previous.is_none(),
            "two census rows share a site_id: {:?}",
            row["site_id"]
        );
    }
    map
}

/// The records, grouped by this gate's key. Deterministic in insertion order, which is what lets the
/// identity tests below name a group's first row without reading the table to learn which row the
/// model chose.
fn group_rows(rows: &[Value]) -> BTreeMap<Site, Vec<Value>> {
    let mut groups: BTreeMap<Site, Vec<Value>> = BTreeMap::new();
    for row in rows {
        groups
            .entry(Site::of_row(row))
            .or_default()
            .push(row.clone());
    }
    groups
}

// ---------------------------------------------------------------------------
// §24's rules 4–6: the duplicate fold, rebuilt from the pair rows
// ---------------------------------------------------------------------------

/// One candidate's evidence as this gate folds it: the pairs of its (producer, consumer) shape, and
/// §2.2's three layers counted apart.
#[derive(Debug, Default)]
struct Folded {
    pairs: usize,
    exact: usize,
    other: usize,
    conditions_met: usize,
    marked_safe: usize,
}

fn conditions_all_met(pair: &Value) -> bool {
    let conditions = pair["conditions"].as_array().cloned().unwrap_or_default();
    !conditions.is_empty()
        && conditions
            .iter()
            .all(|item| item["outcome"].as_str() == Some("checked_met"))
}

/// Fold the record's pairs onto one candidate row. A `same_site` candidate (§8's Pattern A inside one
/// call site, where the producer and the consumer are one shape) matches on a single key, and a pair
/// whose two sides are different sites must not be able to satisfy it.
fn fold_candidate(candidate: &Value, sites: &BTreeMap<String, Site>, pairs: &[Value]) -> Folded {
    let id = txt(candidate, "candidate");
    let producer_id = txt(candidate, "producer");
    let consumer_id = txt(candidate, "consumer");
    let producer = sites
        .get(&producer_id)
        .unwrap_or_else(|| panic!("{id}: producer {producer_id:?} is not a census site"));
    let consumer = sites
        .get(&consumer_id)
        .unwrap_or_else(|| panic!("{id}: consumer {consumer_id:?} is not a census site"));
    let same_site = producer_id == consumer_id;
    let mut out = Folded::default();
    for pair in pairs {
        let consumer_side = Site::of_pair_side(pair, "consumer");
        let producer_side = Site::of_pair_side(pair, "producer");
        let matched = if same_site {
            producer_side == consumer_side && consumer_side == *consumer
        } else {
            producer_side == *producer && consumer_side == *consumer
        };
        if !matched {
            continue;
        }
        out.pairs += 1;
        match pair["duplicate_type"].as_str() {
            Some("exact_duplicate") => out.exact += 1,
            Some(_) => out.other += 1,
            None => panic!("{}: a pair with no duplicate_type", pair["candidate_id"]),
        }
        if conditions_all_met(pair) {
            out.conditions_met += 1;
        }
        if pair["safe_to_reuse"].as_bool() == Some(true) {
            out.marked_safe += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// §24's rules 8, 11, 12 and 14, re-implemented as row rules
// ---------------------------------------------------------------------------

/// §11's three roles that carry the "this read *is* the check" default.
const CHECK_ROLES: [&str; 3] = [
    "SAFETY_GATE",
    "INDEPENDENT_RECHECK",
    "FINAL_EXECUTION_GUARD",
];
/// The two proof kinds that credit a saving, out of the three values the field may hold.
const CREDITING_PROOFS: [&str; 2] = [
    "every_reuse_condition_met_in_the_record",
    "the_record_itself_marks_the_pair_safe_to_reuse",
];

/// The candidate-row rules this gate re-implements, so a mutation has to slip past a second,
/// separately written set. Each is one of §24's rules stated as a condition on printed fields.
fn rule_errors(candidate: &Value) -> Vec<String> {
    let id = candidate["candidate"].as_str().unwrap_or("<unnamed>");
    let theoretical = candidate["theoretical_saving"].as_u64().unwrap_or_default();
    let safe = candidate["safe_saving"].as_u64().unwrap_or_default();
    let role = candidate["verification_role"].as_str().unwrap_or_default();
    let alternative = candidate["alternative_verification"]
        .as_str()
        .unwrap_or_default();
    let proof = candidate["safe_reuse_proof"]["kind"]
        .as_str()
        .unwrap_or_default();
    let is_check = CHECK_ROLES.contains(&role);
    let mut errors = Vec::new();
    if safe > theoretical {
        errors.push(format!(
            "{id}: safe_saving {safe} exceeds the theoretical ceiling {theoretical}, which §13 \
             forbids"
        ));
    }
    if safe > 0 && !CREDITING_PROOFS.contains(&proof) {
        errors.push(format!(
            "{id}: safe_saving {safe} with proof {proof:?}, which is not one of \
             {CREDITING_PROOFS:?} — §24's sixth figure"
        ));
    }
    if safe > 0 && is_check && alternative != "EXISTING_OTHER_CHECK" {
        errors.push(format!(
            "{id}: safe_saving {safe} while the consumer's role is {role} and its alternative is \
             {alternative} — §11's default, §25's NC4"
        ));
    }
    if role == "NONE"
        && candidate["role_none_reason"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    {
        errors.push(format!(
            "{id}: role NONE with no reason named — §24's eighth rule"
        ));
    }
    let claims_the_read_is_the_check = candidate["blockers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|blocker| blocker.as_str() == Some("read_is_the_check"));
    if claims_the_read_is_the_check && !is_check {
        errors.push(format!(
            "{id}: a blocker says the read IS the check while {role} says it verifies nothing — \
             §25's NC2"
        ));
    }
    if candidate["duplicate_status"]["identity"]["uses_row_index"].as_bool() == Some(true) {
        errors.push(format!(
            "{id}: an identity built from a row index — §24's twelfth rule"
        ));
    }
    if candidate["duplicate_status"]["identity"]["uses_duration"].as_bool() == Some(true) {
        errors.push(format!(
            "{id}: an identity built from a duration — §24's tenth rule"
        ));
    }
    errors
}

// ---------------------------------------------------------------------------
// §24's rules 1–3: the census and the surface rebuild from the run records
// ---------------------------------------------------------------------------

/// §24's first rule, recomputed by a fold that never opens a table to learn how to group: the 246
/// asks of the three runs collapse into exactly the 44 rows the census prints, under a key built here
/// from the method, the stage, and this gate's own masking of the caller.
#[test]
fn the_census_groups_rebuild_from_the_run_records_alone() {
    let raw = raw_rows();
    assert_eq!(
        raw.len(),
        246,
        "the committed corpus is three runs of 82 asks"
    );
    let groups = group_rows(&raw);
    let published_rows = rows_of(CENSUS);
    assert_eq!(
        groups.len(),
        published_rows.len(),
        "this fold found {} groups and the census prints {} rows",
        groups.len(),
        published_rows.len()
    );
    let mut unstamped = 0_usize;
    let mut multi_key = 0_usize;
    for row in &published_rows {
        let site = Site::of_table_row(row);
        assert_eq!(site.label(), txt(row, "key"), "{site:?}: the printed label");
        let members = groups
            .get(&site)
            .unwrap_or_else(|| panic!("{}: no recorded ask folds to this row", site.label()));
        assert_eq!(
            members.len() as u64,
            count(row, "asks"),
            "{}: {} asks fold here and the row prints {}",
            site.label(),
            members.len(),
            count(row, "asks")
        );
        assert_eq!(
            count(row, "count"),
            count(row, "asks"),
            "{}: §18's `count` and its echo disagree",
            site.label()
        );
        let mut per_run: BTreeMap<String, u64> = BTreeMap::new();
        for member in members {
            *per_run.entry(txt(member, "_directory")).or_insert(0) += 1;
        }
        let published_split = counts(&row["asks_per_run"]);
        assert_eq!(
            published_split.values().sum::<u64>(),
            members.len() as u64,
            "{}: the printed split does not add to the row's own asks",
            site.label()
        );
        assert_eq!(
            published_split,
            per_run,
            "{}: the per-run split is not the records' split",
            site.label()
        );
        let variants: BTreeSet<String> = members
            .iter()
            .map(|member| member["dedup_key"].as_str().unwrap_or(UNKEYED).to_string())
            .collect();
        assert_eq!(
            variants.len() as u64,
            count(row, "recorded_key_variants"),
            "{}: the row prints {} key shapes and the records hold {}",
            site.label(),
            count(row, "recorded_key_variants"),
            variants.len()
        );
        assert!(
            variants.len() as u64 <= members.len() as u64,
            "{}: more key shapes than asks",
            site.label()
        );
        if site.stage == UNSTAMPED {
            unstamped += 1;
        }
        if variants.len() > 1 {
            multi_key += 1;
        }
    }
    // Both fallbacks have to be exercised, or the fold would pass on a corpus that never needs them.
    assert!(
        unstamped > 0,
        "no site is unstamped, so the connect-time leg is not in this fold at all"
    );
    assert!(
        multi_key > 0,
        "no site holds more than one recorded key, so `recorded_key_variants` is a constant"
    );
    assert_eq!(
        count(
            &published(CENSUS)["totals"],
            "sites_with_more_than_one_recorded_key"
        ) as usize,
        multi_key,
        "the multi-key total is not this fold's count"
    );
    let printed: BTreeSet<Site> = published_rows.iter().map(Site::of_table_row).collect();
    assert_eq!(
        printed,
        groups.keys().cloned().collect::<BTreeSet<Site>>(),
        "a group this fold found is not a row, or a row is not a group"
    );
}

/// §24's eleventh rule, over every row: the identity a census row prints is its own record's
/// `dedup_key` re-rendered by this gate's grammar, every term is a business field, and no term is a
/// clock or a position.
#[test]
fn the_identities_rebuild_from_the_record_s_own_key_grammar() {
    let raw = raw_rows();
    let groups = group_rows(&raw);
    let mut terms_seen: BTreeSet<String> = BTreeSet::new();
    let mut unkeyed_rows = 0_usize;
    for row in rows_of(CENSUS) {
        let site = Site::of_table_row(&row);
        let members = groups
            .get(&site)
            .expect("the group test above covers every row");
        let first = representative(members);
        let rebuilt = identity_of(first);
        assert_eq!(
            rebuilt,
            txt(&row, "identity"),
            "{}: the identity this gate rebuilds from {:?} is not what the row prints",
            site.label(),
            first["dedup_key"]
        );
        let printed = identity_terms(&rebuilt);
        assert_eq!(
            printed[0].0,
            "method",
            "{}: an identity has to start by naming its method",
            site.label()
        );
        let names: Vec<String> = printed
            .iter()
            .skip(1)
            .map(|(name, _)| name.clone())
            .collect();
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "{}: the terms are not alphabetical, so a fold over them would be order-sensitive: \
             {names:?}",
            site.label()
        );
        for (name, value) in &printed {
            assert!(
                IDENTITY_TERMS.contains(&name.as_str()),
                "{}: {name}={value} is not a term M8.4.2's grammar builds",
                site.label()
            );
            assert!(
                !value.is_empty(),
                "{}: {name}= is an empty term, which is a hole or a placeholder, not a business \
                 field",
                site.label()
            );
            terms_seen.insert(name.clone());
        }
        if first["dedup_key"].is_null() {
            unkeyed_rows += 1;
            assert!(
                names.contains(&"endpoint".to_string())
                    && names.contains(&"method_kind".to_string())
                    && !names.contains(&"block".to_string()),
                "{}: a parameterless site is keyed by which endpoint answered, never by a height",
                site.label()
            );
        } else {
            assert!(
                names.contains(&"chain".to_string()) && names.contains(&"block".to_string()),
                "{}: a keyed read has to say which chain and which block it asked about",
                site.label()
            );
        }
        let declared: BTreeSet<String> = row["identity_fields"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| item.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            declared,
            names.iter().cloned().collect::<BTreeSet<String>>(),
            "{}: the row's `identity_fields` are not the terms its identity prints",
            site.label()
        );
    }
    assert_eq!(
        unkeyed_rows, 7,
        "the surface counts seven parameterless sites; another number here means the two folds \
         disagree about which requests carry no key"
    );
    for expected in [
        "chain", "block", "to", "data", "address", "slot", "hydrated", "endpoint",
    ] {
        assert!(
            terms_seen.contains(expected),
            "no identity in the census carries a {expected:?} term, so the allow-list above has \
             never been exercised against it"
        );
    }
    // The record's own integrity, which the fold depends on: the segments of a `dedup_key` have to be
    // the row's own columns. A key that stopped matching its row would let an identity describe an
    // ask nobody sent.
    for row in &raw {
        let Some(key) = row["dedup_key"].as_str() else {
            continue;
        };
        let parts: Vec<&str> = key.split('|').collect();
        assert_eq!(parts[1], "91342", "{key}: a key naming another chain");
        let target = opt_txt(row, "target").map(|value| value.to_lowercase());
        if parts[0] == "block" {
            assert_eq!(
                target,
                Some(parts[2].to_lowercase()),
                "{key}: a header read whose target column is not its key's block segment"
            );
        } else if let Some(target) = target {
            assert!(
                parts.iter().skip(3).any(|segment| *segment == target),
                "{key}: the row's target {target} is in no segment of its own key"
            );
        }
        if let Some(slot) = opt_txt(row, "slot").map(|value| value.to_lowercase()) {
            assert!(
                parts.iter().any(|segment| *segment == slot),
                "{key}: the row's slot {slot} is not a segment of its own key"
            );
        }
    }
}

/// §24's second and third rules through this gate's fold: the surface table's per-method, per-stage
/// and per-run counts are the records counted by a pass that never reads the table.
#[test]
fn the_surface_counts_rebuild_from_the_records_too() {
    let raw = raw_rows();
    let table = published(SURFACE);
    let surface = table["rows"].as_array().cloned().unwrap_or_default();

    let mut by_method: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_stage: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_run: BTreeMap<String, u64> = BTreeMap::new();
    let mut attempts = 0_u64;
    for row in &raw {
        *by_method.entry(txt(row, "method")).or_insert(0) += 1;
        *by_stage
            .entry(opt_txt(row, "stage").unwrap_or_else(|| UNSTAMPED.to_string()))
            .or_insert(0) += 1;
        *by_run.entry(txt(row, "_directory")).or_insert(0) += 1;
        attempts += row["attempts"]
            .as_array()
            .map_or(1, |list| list.len() as u64);
    }
    let as_map = |list: &Value, field: &str| -> BTreeMap<String, u64> {
        list.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|row| (txt(row, field), count(row, "asks")))
            .collect()
    };
    assert_eq!(
        as_map(&table["by_method"], "method"),
        by_method,
        "the surface's per-method counts are not the records' per-method counts"
    );
    assert_eq!(
        as_map(&table["by_stage"], "stage"),
        by_stage,
        "the surface's per-stage counts are not the records' per-stage counts"
    );
    assert_eq!(
        as_map(&table["counts"]["asks_per_run"], "run"),
        by_run,
        "the per-run split is not the runs'"
    );
    assert_eq!(by_run.len(), 3, "three committed runs");

    let totals = &table["counts"];
    assert_eq!(count(totals, "sites") as usize, surface.len());
    assert_eq!(count(totals, "asks_total") as usize, raw.len());
    assert_eq!(count(totals, "methods_traced") as usize, by_method.len());
    assert_eq!(count(totals, "stages") as usize, by_stage.len());
    assert_eq!(count(totals, "physical_http_attempts"), attempts);

    let groups = group_rows(&raw);
    let times = fold_times(&raw);
    let mut identity_sources: BTreeMap<String, u64> = BTreeMap::new();
    for row in &surface {
        let site = Site::of_table_row(row);
        let members = groups.get(&site).expect("covered by the census fold");
        assert_eq!(
            count(row, "asks_total") as usize,
            members.len(),
            "{}: the surface's asks are not this group's",
            site.label()
        );
        *identity_sources
            .entry(txt(row, "identity_source"))
            .or_insert(0) += 1;
        let fold = times
            .get(&site.label())
            .unwrap_or_else(|| panic!("{}: no timing fold", site.label()));
        assert_eq!(count(&row["timing"], "asks"), fold.asks);
        assert_eq!(count(&row["timing"], "physical_attempts"), fold.attempts);
        assert_eq!(
            txt(&groups[&site][0], "method"),
            site.method,
            "{}: a group whose members disagree about their method cannot have one identity",
            site.label()
        );
    }
    assert_eq!(
        identity_sources
            .get("identity_from_the_recorded_dedup_key")
            .copied()
            .unwrap_or_default(),
        37,
        "37 sites carry a `dedup_key`; the split this gate counted is {identity_sources:?}"
    );
    assert_eq!(
        identity_sources
            .get("identity_from_a_parameterless_request")
            .copied()
            .unwrap_or_default(),
        7,
        "seven sites ask a parameterless method; the split is {identity_sources:?}"
    );
    // §4's other half: a surface row is a call site this gate can find in the code, and its source
    // anchor is not the table's own invention.
    for row in &surface {
        assert!(
            txt(row, "source_file").starts_with("crates/"),
            "{}: a call site outside the workspace",
            row["site_id"]
        );
        assert!(count(row, "source_line") > 0);
    }
}

/// §24's thirteenth rule's other half: the two duration columns are the records' own intervals —
/// merged within a run, added across runs — and nothing else.
#[test]
fn the_duration_columns_are_the_records_own_intervals_merged() {
    let raw = raw_rows();
    let times = fold_times(&raw);
    let mut sum_total = 0_u64;
    let mut union_total = 0_u64;
    let mut overlaps = 0_usize;
    let mut without = 0_usize;
    for row in rows_of(CENSUS) {
        let site = Site::of_table_row(&row);
        let fold = times
            .get(&site.label())
            .unwrap_or_else(|| panic!("{}: no fold", site.label()));
        assert_eq!(
            row["rpc_sum_duration_ms"].as_f64(),
            Some(ms(fold.sum_ns)),
            "{}: the sum column is not the durations added",
            site.label()
        );
        assert_eq!(
            row["union_duration_ms"].as_f64(),
            Some(ms(fold.union_ns)),
            "{}: the union column is not the merged intervals",
            site.label()
        );
        assert!(
            fold.union_ns <= fold.sum_ns,
            "{}: a union above the sum means the merge is wrong",
            site.label()
        );
        if fold.overlaps == 0 {
            assert_eq!(
                fold.union_ns,
                fold.sum_ns,
                "{}: with no overlapping pair the union is the sum",
                site.label()
            );
        }
        let timing = &row["duration"];
        assert_eq!(count(timing, "asks"), fold.asks);
        assert_eq!(count(timing, "overlap_rows"), fold.overlaps as u64);
        assert_eq!(
            count(timing, "rows_without_interval"),
            fold.rows_without_interval as u64
        );
        sum_total += fold.sum_ns;
        union_total += fold.union_ns;
        overlaps += fold.overlaps;
        without += fold.rows_without_interval;
    }
    let totals = published(CENSUS)["totals"].clone();
    assert_eq!(
        totals["rpc_sum_duration_ms"].as_f64(),
        Some(ms(sum_total)),
        "the census's total sum is not this fold's"
    );
    assert_eq!(
        totals["union_duration_ms"].as_f64(),
        Some(ms(union_total)),
        "the census's total union is not this fold's — a merge that overwrote one run's total \
         instead of accumulating would show here and in every per-row column above"
    );
    assert_eq!(count(&totals, "overlap_rows") as usize, overlaps);
    assert_eq!(count(&totals, "rows_without_interval") as usize, without);
    // M8.3.3 pins state reads at concurrency 1, so no two asks of one site overlap, and the two
    // columns are therefore equal everywhere. If that stops being true, this is the assertion to
    // revisit — not the fold.
    assert_eq!(
        overlaps, 0,
        "{overlaps} overlapping intervals: the corpus is no longer the serial one that makes \
         union == sum an explanation rather than a coincidence"
    );
    assert_eq!(without, 0, "every recorded ask carries both stamps");
}

// ---------------------------------------------------------------------------
// §24's rules 4–6: the candidates and their counts
// ---------------------------------------------------------------------------

/// §24's fourth, fifth and sixth rules: the 18, the 42 and the 0 are all folded out of the 78 pair
/// rows here, and each candidate's printed figures are its own share of them.
#[test]
fn every_duplicate_count_is_a_fold_of_the_record_s_pair_rows() {
    let pairs = pairs();
    assert_eq!(pairs.len(), 78, "M8.4.2 published 78 pair rows");
    let sites = sites_by_id();
    let candidates = rows_of(CANDIDATES);
    let mut folded_exact = 0_usize;
    let mut folded_other = 0_usize;
    let mut with_pairs = 0_usize;
    for candidate in &candidates {
        let id = txt(candidate, "candidate");
        let folded = fold_candidate(candidate, &sites, &pairs);
        assert_eq!(
            folded.exact as u64,
            count(candidate, "duplicate_count_exact"),
            "{id}: {folded:?} folded from the pair rows, and the row prints {} exact",
            count(candidate, "duplicate_count_exact")
        );
        assert_eq!(
            folded.exact as u64,
            count(candidate, "theoretical_saving"),
            "{id}: §13's ceiling is the measured exact-duplicate count, so a row printing a \
             different {folded:?} is not the fold"
        );
        assert_eq!(
            folded.exact as u64,
            count(&candidate["three_layers"], "physical_duplicate"),
            "{id}: §2.2's first layer and the ceiling are the same measurement"
        );
        assert_eq!(
            folded.conditions_met as u64,
            count(candidate, "reusable_in_principle"),
            "{id}: §2.2's second layer is the pairs whose five conditions all resolved met"
        );
        assert_eq!(
            folded.marked_safe as u64,
            count(candidate, "safe_to_reuse_now"),
            "{id}: §2.2's third layer is the pairs the record itself marks safe"
        );
        assert_eq!(
            folded.pairs as u64,
            count(&candidate["duplicate_status"], "exact_duplicate_pairs")
                + candidate["duplicate_status"]["by_duplicate_class"]
                    .as_object()
                    .map(|classes| classes
                        .iter()
                        .filter(|(class, _)| *class != "exact_duplicate")
                        .map(|(_, cells)| cells.as_u64().unwrap_or_default())
                        .sum::<u64>())
                    .unwrap_or_default(),
            "{id}: the pairs this gate matched are not the classes the row breaks out"
        );
        folded_exact += folded.exact;
        folded_other += folded.other;
        if folded.exact > 0 {
            with_pairs += 1;
        }
    }
    let exact_in_record = pairs
        .iter()
        .filter(|pair| pair["duplicate_type"].as_str() == Some("exact_duplicate"))
        .count();
    assert_eq!(
        folded_exact, exact_in_record,
        "the candidates fold {folded_exact} of the record's {exact_in_record} exact pairs"
    );
    assert_eq!(exact_in_record, 42, "§24's fifth figure");
    assert_eq!(
        with_pairs, 14,
        "14 of the 18 candidates name a shape the records produced"
    );
    assert_eq!(
        folded_other, 18,
        "the 18 candidates' rows carry {folded_other} of the record's same-target pairs"
    );
    // The other half of those 36 pairs is not lost: §18's census counts every one of them at site
    // level, whether or not the pair's producer→consumer edge is a §6 candidate. Saying so here is
    // what keeps the 18 above from reading like a claim that the census saw only 18.
    let record_other = pairs.len() - exact_in_record;
    let site_level_other: u64 = rows_of(CENSUS)
        .iter()
        .map(|row| count(row, "duplicate_count_other_classes"))
        .sum();
    assert_eq!(
        site_level_other, record_other as u64,
        "the census's site-level other-duplicate column does not add up to the record's {record_other} \
         non-exact pairs, so the 18 above and the {site_level_other} here are two different claims"
    );
    assert_eq!(record_other, 36, "78 pair rows, 42 of them exact");
    let total_printed: u64 = candidates
        .iter()
        .map(|candidate| count(candidate, "theoretical_saving"))
        .sum();
    assert_eq!(
        total_printed, exact_in_record as u64,
        "the candidates' ceilings add to {total_printed} while the record has {exact_in_record} \
         exact pairs — a pair is being claimed twice"
    );
    for row in rows_of(MATRIX) {
        let id = txt(&row, "candidate");
        let candidate = candidates
            .iter()
            .find(|candidate| candidate["candidate"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("{id}: a matrix row with no candidate behind it"));
        assert_eq!(
            count(&row, "theoretical_saving"),
            count(candidate, "theoretical_saving"),
            "{id}: the matrix and the candidate table print different ceilings"
        );
        assert_eq!(
            count(&row, "avoidable_count"),
            count(&row, "theoretical_saving"),
            "{id}: the matrix's avoidable count is its ceiling"
        );
    }
}

/// §24's sixth rule and §2.2's third layer, said the only way they can be said on this corpus: the
/// safe column is 0 because the records hold nothing to put in it, and this gate looked in the pair
/// rows rather than reading it off the tables.
#[test]
fn the_safe_saving_column_has_nothing_behind_it() {
    let pairs = pairs();
    let met = pairs.iter().filter(|pair| conditions_all_met(pair)).count();
    let marked = pairs
        .iter()
        .filter(|pair| pair["safe_to_reuse"].as_bool() == Some(true))
        .count();
    assert_eq!(
        met, 0,
        "{met} pairs now have all five reuse conditions resolved met, so §2.2's second layer has to \
         be recomputed from the fold rather than asserted at zero"
    );
    assert_eq!(
        marked, 0,
        "{marked} pairs are marked safe by M8.4.2's own record, which is a crediting proof"
    );
    // The zero has to be a counted thing, not an absent one: the pairs do carry the five conditions,
    // and at least some of them resolve met.
    let outcomes: BTreeMap<String, usize> = pairs
        .iter()
        .flat_map(|pair| pair["conditions"].as_array().cloned().unwrap_or_default())
        .fold(BTreeMap::new(), |mut map, condition| {
            *map.entry(txt(&condition, "outcome")).or_insert(0) += 1;
            map
        });
    assert!(
        *outcomes.get("checked_met").unwrap_or(&0) > 0,
        "no condition resolves anywhere, so the second layer was never measured: {outcomes:?}"
    );
    assert!(
        *outcomes.get("not_checkable_from_a_record").unwrap_or(&0) > 0,
        "nothing is uncheckable, which would mean the corpus answers every reuse question itself"
    );
    for candidate in rows_of(CANDIDATES) {
        let id = txt(&candidate, "candidate");
        assert_eq!(
            count(&candidate, "safe_saving"),
            0,
            "{id}: a safe saving of {} appeared, and the records hold {met} met-condition pairs and \
             {marked} marked-safe pairs",
            count(&candidate, "safe_saving")
        );
        let proof = candidate["safe_reuse_proof"]["kind"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !CREDITING_PROOFS.contains(&proof),
            "{id}: the row's proof kind {proof:?} is a crediting one while the fold finds nothing \
             to credit"
        );
        let errors = rule_errors(&candidate);
        assert!(errors.is_empty(), "{id}: {errors:?}");
    }
    for row in rows_of(CENSUS) {
        let label = Site::of_table_row(&row).label();
        assert_eq!(count(&row, "safe_reuse"), 0, "{label}");
        assert_eq!(count(&row, "reusable_in_principle"), 0, "{label}");
        assert_eq!(count(&row, "safe_saving"), 0, "{label}");
    }
    assert_eq!(count(&published(CANDIDATES)["totals"], "safe_saving"), 0);
    assert_eq!(count(&published(MATRIX)["totals"], "safe_saving"), 0);
}

/// §7's five measures, recomputed: three of them are this gate's own numbers, and the fifth is the
/// one §7's worked example exists to keep from being read as a sum.
#[test]
fn the_five_measures_stay_five_separate_numbers() {
    let pairs = pairs();
    let sites = sites_by_id();
    let physical: u64 = rows_of(CANDIDATES)
        .iter()
        .map(|candidate| fold_candidate(candidate, &sites, &pairs).exact as u64)
        .sum();
    let semantic: u64 = pairs
        .iter()
        .filter(|pair| conditions_all_met(pair) || pair["safe_to_reuse"].as_bool() == Some(true))
        .count() as u64;
    let measures = published(SAVING)["measures"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(measures.len(), 5, "§7 asks for exactly five measures");
    let printed: BTreeMap<String, u64> = measures
        .iter()
        .map(|row| (txt(row, "measure"), count(row, "value")))
        .collect();
    assert_eq!(printed.len(), 5, "five distinct measure names");
    assert_eq!(
        printed["physical_duplicate_saving"], physical,
        "§7's first measure is the exact-duplicate count this gate folded"
    );
    assert_eq!(printed["semantic_reuse_saving"], semantic);
    assert_eq!(
        printed["verification_removal_saving"], 0,
        "§2.1: nothing in M8.6 removes a check"
    );
    assert_eq!(
        printed["parallel_wall_time_saving"], 0,
        "M8.3.3's concurrency default is unchanged, so no wall time was claimed"
    );
    assert_eq!(printed["total_safe_rpc_saving"], 0, "§22's answer");
    let mut how_seen: BTreeSet<String> = BTreeSet::new();
    for row in &measures {
        let name = txt(row, "measure");
        assert!(
            !txt(row, "what_it_does_not_mean").is_empty(),
            "{name}: a measure without its own disclaiming line is the §7 mistake the table exists \
             to prevent"
        );
        let how = txt(row, "how_computed");
        assert!(
            !how.is_empty() && !txt(row, "unit").is_empty(),
            "{name}: a measure with no stated computation or no unit cannot be one of §7's five"
        );
        assert!(
            !txt(row, "reconciles_with").is_empty(),
            "{name}: §7's measures stay separate because each names the figure it reconciles with"
        );
        assert!(
            how_seen.insert(how),
            "{name}: two measures state the same computation, which is §7's summing mistake \
             wearing two names"
        );
    }
    let arithmetic = published(SAVING)["arithmetic"].clone();
    let measured_here = counts(&arithmetic["measured_here"]);
    for (name, value) in &printed {
        assert_eq!(
            measured_here.get(name),
            Some(value),
            "{name} prints {value} as a measure and {:?} as an arithmetic input",
            measured_here.get(name)
        );
    }
    let first_four: u64 = [
        "physical_duplicate_saving",
        "semantic_reuse_saving",
        "verification_removal_saving",
        "parallel_wall_time_saving",
    ]
    .iter()
    .map(|name| measured_here[*name])
    .sum();
    assert_eq!(count(&arithmetic, "sum_of_the_first_four"), first_four);
    assert_eq!(
        arithmetic["is_the_fifth_the_sum_of_the_first_four"].as_bool(),
        Some(false),
        "the fifth measure is a subset of the first, not an addition to it (§13)"
    );
    assert_ne!(first_four, measured_here["total_safe_rpc_saving"]);
    let example = counts(&arithmetic["the_task_books_example"]);
    assert_eq!(
        example["physical_duplicate_saving"], 41,
        "§7's worked example is 41/0/0/0/0, and this table must not quietly rewrite it to match its \
         own 42"
    );
    assert_eq!(measured_here["physical_duplicate_saving"], 42);
    let verdict = published(VERDICT)["five_measures"].clone();
    for name in printed.keys() {
        assert_eq!(
            verdict[name].as_u64(),
            printed.get(name).copied(),
            "final-verdict prints a different {name} from saving-kinds"
        );
    }
    let diagnostics = verdict["diagnostics"].clone();
    assert_eq!(
        count(
            &diagnostics,
            "pairs_with_every_reuse_condition_met_in_the_record"
        ),
        pairs.iter().filter(|pair| conditions_all_met(pair)).count() as u64
    );
    assert_eq!(
        count(&diagnostics, "pairs_the_record_itself_marks_safe_to_reuse"),
        pairs
            .iter()
            .filter(|pair| pair["safe_to_reuse"].as_bool() == Some(true))
            .count() as u64
    );
    assert_eq!(
        count(&diagnostics, "theoretical_saving_over_all_candidates"),
        physical
    );
    assert!(
        count(&diagnostics, "exact_pairs_behind_a_check_role") > 0,
        "no exact pair sits behind a check role, which would leave §11 untested"
    );
    assert!(
        count(&diagnostics, "exact_pairs_behind_a_check_role") <= physical,
        "more pairs behind a check than exist at all"
    );
}

// ---------------------------------------------------------------------------
// §24's rules 1, 4 and 9: the tables add up to each other
// ---------------------------------------------------------------------------

/// Every `totals` and `counts` block in the directory, checked as the arithmetic of the rows it
/// prints. A total that is typed rather than folded fails here even when the rows agree with each
/// other.
#[test]
fn every_published_total_is_the_rows_it_prints_added_up() {
    let candidates = rows_of(CANDIDATES);
    let totals = published(CANDIDATES)["totals"].clone();
    assert_eq!(count(&totals, "candidates") as usize, candidates.len());
    assert_eq!(
        count(&totals, "safe_saving"),
        candidates
            .iter()
            .map(|row| count(row, "safe_saving"))
            .sum::<u64>()
    );
    assert_eq!(
        count(&totals, "theoretical_saving"),
        candidates
            .iter()
            .map(|row| count(row, "theoretical_saving"))
            .sum::<u64>()
    );
    let field = |name: &str| -> Vec<String> {
        candidates
            .iter()
            .map(|row| txt(row, name))
            .collect::<Vec<String>>()
    };
    assert_eq!(counts(&totals["by_pattern"]), tally(&field("pattern")));
    assert_eq!(counts(&totals["by_priority"]), tally(&field("priority")));
    assert_eq!(counts(&totals["by_verdict"]), tally(&field("verdict")));

    let census = rows_of(CENSUS);
    let census_totals = published(CENSUS)["totals"].clone();
    assert_eq!(count(&census_totals, "rows") as usize, census.len());
    for name in ["asks", "duplicate_count", "safe_saving"] {
        assert_eq!(
            count(&census_totals, name),
            census.iter().map(|row| count(row, name)).sum::<u64>(),
            "the census's {name} total is not its rows"
        );
    }
    assert_eq!(
        count(&census_totals, "physical_http_attempts"),
        census
            .iter()
            .map(|row| count(&row["duration"], "physical_attempts"))
            .sum::<u64>()
    );

    let matrix = rows_of(MATRIX);
    let matrix_totals = published(MATRIX)["totals"].clone();
    assert_eq!(count(&matrix_totals, "candidates") as usize, matrix.len());
    for name in [
        "current_count",
        "avoidable_count",
        "safe_saving",
        "theoretical_saving",
    ] {
        assert_eq!(
            count(&matrix_totals, name),
            matrix.iter().map(|row| count(row, name)).sum::<u64>(),
            "the matrix's {name} total is not its rows"
        );
    }

    let queue = rows_of(QUEUE);
    assert_eq!(
        counts(&published(QUEUE)["by_priority"]),
        tally(
            &queue
                .iter()
                .map(|row| txt(row, "priority"))
                .collect::<Vec<String>>()
        )
    );
    let mut positions: Vec<u64> = queue
        .iter()
        .map(|row| count(row, "queue_position"))
        .collect();
    positions.sort_unstable();
    assert_eq!(
        positions,
        (1..=queue.len() as u64).collect::<Vec<u64>>(),
        "the queue's positions are not 1..=n, so a row is duplicated or missing"
    );

    let rejected = rows_of(REJECTED);
    let rejected_counts = published(REJECTED)["counts"].clone();
    assert_eq!(count(&rejected_counts, "rejected") as usize, rejected.len());
    assert_eq!(
        count(&rejected_counts, "pseudo_optimizations_excluded") as usize,
        rejected.len(),
        "§23: every rejection in the table is an excluded pseudo-optimization"
    );
    let reject_band: BTreeSet<String> = candidates
        .iter()
        .filter(|row| row["priority"].as_str() == Some("REJECT"))
        .map(|row| txt(row, "candidate"))
        .collect();
    let rejected_rows: BTreeSet<String> = rejected
        .iter()
        .map(|row| txt(row, "candidate_id"))
        .collect();
    assert_eq!(
        rejected_rows, reject_band,
        "§20's table is not §14's REJECT band"
    );
    for row in &rejected {
        let reasons = row["reason"].as_array().cloned().unwrap_or_default();
        assert!(
            !reasons.is_empty(),
            "{}: §24's ninth rule — a rejection with no blocker",
            row["candidate_id"]
        );
        let candidate = candidates
            .iter()
            .find(|candidate| candidate["candidate"].as_str() == row["candidate_id"].as_str())
            .expect("covered by the band check above");
        let declared: BTreeSet<String> = ["blockers", "derived_blockers"]
            .iter()
            .flat_map(|key| candidate[*key].as_array().cloned().unwrap_or_default())
            .map(|item| item.as_str().unwrap_or_default().to_string())
            .collect();
        for reason in &reasons {
            let reason = reason.as_str().unwrap_or_default();
            assert!(
                declared.contains(reason) || reason == "safe_saving_zero",
                "{}: the rejection names {reason:?}, which the candidate's own row does not \
                 publish — {declared:?}",
                row["candidate_id"]
            );
        }
    }

    let verdict = published(VERDICT);
    let counts_block = verdict["counts"].clone();
    assert_eq!(
        count(&counts_block, "candidates") as usize,
        candidates.len()
    );
    assert_eq!(
        count(&counts_block, "sites_on_the_hot_path") as usize,
        census.len()
    );
    assert_eq!(
        count(&counts_block, "methods_traced") as usize,
        census
            .iter()
            .map(|row| txt(row, "method"))
            .collect::<BTreeSet<String>>()
            .len()
    );
    assert_eq!(
        count(&counts_block, "rejected_pseudo_optimizations") as usize,
        rejected.len()
    );
    // The candidate table publishes only the bands it uses; the verdict block publishes all five of
    // §14's, including the empty ones. Both have to say the same thing about each band.
    let folded_bands = tally(&field("priority"));
    let five_bands: BTreeMap<String, u64> = ["P0", "P1", "P2", "P3", "REJECT"]
        .iter()
        .map(|band| {
            (
                band.to_string(),
                folded_bands.get(*band).copied().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        counts(&counts_block["by_priority"]),
        five_bands,
        "§14's five bands, counted from the candidates and counted in the verdict block, disagree"
    );
    assert_eq!(
        count(&counts_block, "asks_without_recorded_identity"),
        raw_rows()
            .iter()
            .filter(|row| row["dedup_key"].is_null())
            .count() as u64
    );
    let block = verdict["verdict_block"].clone();
    assert_eq!(count(&block, "current_hot_path_rpc_count"), 246);
    assert_eq!(
        count(&block, "safe_reducible_rpc"),
        count(&census_totals, "safe_saving")
    );
    assert_eq!(
        count(&block, "theoretical_reducible_rpc"),
        count(&totals, "theoretical_saving")
    );
    let band_total: u64 = ["P0", "P1", "P2", "P3", "REJECT"]
        .iter()
        .map(|band| count(&block, band))
        .sum();
    assert_eq!(band_total, candidates.len() as u64);
}

/// The flow table is the only one in the directory that prints `flows` rather than `rows`, and it
/// joins to the census by `site_id` — which makes it a third witness of each site's identity.
#[test]
fn the_flow_table_describes_every_site_the_census_prints() {
    let table = published(FLOW);
    let flows = table["flows"].as_array().cloned().unwrap_or_default();
    let census = rows_of(CENSUS);
    assert_eq!(flows.len(), census.len(), "one flow per call site, §10");
    let census_by_site: BTreeMap<String, &Value> = census
        .iter()
        .map(|row| (txt(row, "site_id"), row))
        .collect();
    assert_eq!(census_by_site.len(), census.len(), "one site_id, two rows");
    for flow in &flows {
        let site = txt(flow, "site");
        let twin = census_by_site
            .get(&site)
            .unwrap_or_else(|| panic!("{site}: a flow with no census row"));
        assert_eq!(
            txt(flow, "recorded_identity"),
            txt(twin, "identity"),
            "{site}: the flow table and the census print different identities for one site"
        );
        assert!(
            !txt(flow, "asked_at").is_empty(),
            "{site}: a flow with no call site named"
        );
    }
    let census_sites: BTreeSet<String> = census_by_site.keys().cloned().collect();
    let flow_sites: BTreeSet<String> = flows.iter().map(|flow| txt(flow, "site")).collect();
    assert_eq!(census_sites, flow_sites, "a site is in one table only");
    let containers = table["containers"].as_array().cloned().unwrap_or_default();
    assert!(
        !containers.is_empty(),
        "§10's carrier question would go unasked"
    );
    for container in &containers {
        assert_eq!(
            container["holds_revm_canonical_state"].as_bool(),
            Some(false),
            "{}: a carrier holding REVM canonical state is the §10 claim this milestone may not \
             make",
            container["carrier_const"]
        );
    }
    assert_eq!(
        table["new_rpc"].as_u64(),
        Some(0),
        "the flow table is a reading of committed records"
    );
}

// ---------------------------------------------------------------------------
// §25's six controls, re-run here
// ---------------------------------------------------------------------------

/// §25's NC1: five business fields, each moved on a copy of one recorded ask, each re-keyed by this
/// gate's own grammar. An identity that survives a mutation of the fields it is made of is not made
/// of them.
fn nc1_fires() -> bool {
    let row = raw_rows()
        .into_iter()
        .find(|row| {
            row["dedup_key"].as_str()
                == Some("call|91342|37700740|0x2a3ceafba30f6626170cbb0cd67392efb94bd9a4|0dfe1681")
        })
        .expect("the corpus's first eth_call row");
    let before = identity_of(&row);
    let key = row["dedup_key"].as_str().unwrap_or_default();
    let parts: Vec<&str> = key.split('|').collect();
    let mutations: [(String, Value); 5] = [
        (
            "method".to_string(),
            Value::String("eth_getBalance".to_string()),
        ),
        (
            "dedup_key".to_string(),
            Value::String(format!(
                "{}|{}|{}|{}|{}",
                parts[0], parts[1], parts[2], parts[3], "0dfe1680"
            )),
        ),
        (
            "dedup_key".to_string(),
            Value::String(format!(
                "{}|{}|{}|{}0|{}",
                parts[0], parts[1], parts[2], parts[3], parts[4]
            )),
        ),
        (
            "dedup_key".to_string(),
            Value::String(format!(
                "{}|{}|{}|{}|{}",
                parts[0], parts[1], "37700741", parts[3], parts[4]
            )),
        ),
        (
            "dedup_key".to_string(),
            Value::String(format!(
                "{}|{}|{}|{}|{}",
                parts[0], "91343", parts[2], parts[3], parts[4]
            )),
        ),
    ];
    mutations.iter().all(|(field, value)| {
        let mut copy = row.clone();
        copy[field] = value.clone();
        let after = identity_of(&copy);
        assert_ne!(
            after, before,
            "a mutation of {field} left the identity at {before}"
        );
        after != before
    })
}

/// §25's controls two to four, run against this gate's own row rules.
fn nc_fires(control: &str) -> bool {
    let candidates = rows_of(CANDIDATES);
    let find = |name: &str| -> Value {
        candidates
            .iter()
            .find(|row| row["candidate"].as_str() == Some(name))
            .cloned()
            .unwrap_or_else(|| panic!("{name}: no candidate row"))
    };
    match control {
        "NC2" => {
            let target = candidates
                .iter()
                .find(|row| {
                    row["blockers"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .any(|blocker| blocker.as_str() == Some("read_is_the_check"))
                })
                .expect("at least one consumer carries a check role");
            let id = txt(target, "candidate");
            assert!(
                rule_errors(target).is_empty(),
                "{id}: the clean row already fails this gate's rules"
            );
            let mut mutated = target.clone();
            mutated["verification_role"] = Value::String("NONE".to_string());
            mutated["role_none_reason"] = Value::String(String::new());
            let errors = rule_errors(&mutated);
            assert_eq!(
                errors.len(),
                2,
                "{id}: blanking a check role to NONE has to trip both §24's eighth rule and the \
                 blocker that names the read as the check, and it tripped {errors:?}"
            );
            true
        }
        "NC3" => {
            let target = find("chainid.connect_to_preflight");
            let mut mutated = target.clone();
            mutated["safe_saving"] = Value::from(1_u64);
            let errors = rule_errors(&mutated);
            errors
                .iter()
                .any(|error| error.contains("safe_saving 1 with proof"))
        }
        "NC4" => {
            let target = find("chainid.connect_to_preflight");
            let ceiling = count(&published(CENSUS)["totals"], "duplicate_count");
            let mut mutated = target.clone();
            mutated["safe_saving"] = Value::from(ceiling);
            mutated["theoretical_saving"] = Value::from(ceiling);
            mutated["safe_reuse_proof"]["kind"] =
                Value::String("every_reuse_condition_met_in_the_record".to_string());
            let errors = rule_errors(&mutated);
            assert!(
                !errors.iter().any(|error| error.contains("proof")),
                "{target:#?}: a crediting proof kind should silence the proof rule, and it did not: {errors:?}"
            );
            errors
                .iter()
                .any(|error| error.contains("INDEPENDENT_RECHECK"))
        }
        "NC5" => {
            let before = fold_candidate_shape(&candidates);
            let mut shuffled = pairs();
            shuffled.reverse();
            let sites = sites_by_id();
            let after: Vec<(String, usize)> = candidates
                .iter()
                .map(|row| {
                    (
                        txt(row, "candidate"),
                        fold_candidate(row, &sites, &shuffled).exact,
                    )
                })
                .collect();
            // The two halves of §25's NC5: the pair rows reordered must not move a candidate's
            // counts, and the ask rows reordered must not move a group's printed identity. The
            // second is compared as a full shape, not as a length — 44 groups in either order says
            // nothing about which record each one of them names.
            let shape = |rows: &[Value]| -> Vec<(String, usize, String)> {
                group_rows(rows)
                    .iter()
                    .map(|(site, members)| {
                        (
                            site.label(),
                            members.len(),
                            identity_of(representative(members)),
                        )
                    })
                    .collect()
            };
            let mut reversed = raw_rows();
            reversed.reverse();
            assert_eq!(
                shape(&reversed),
                shape(&raw_rows()),
                "reversing the records moved a group"
            );
            before == after
        }
        "NC6" => {
            let raw = raw_rows();
            let semantic = |rows: &[Value]| -> Vec<(String, String, u64)> {
                group_rows(rows)
                    .iter()
                    .map(|(site, members)| {
                        (
                            site.label(),
                            identity_of(representative(members)),
                            members.len() as u64,
                        )
                    })
                    .collect()
            };
            let before = semantic(&raw);
            let mut reclocked = raw.clone();
            for row in &mut reclocked {
                let duration = row["duration_ns"].as_u64().unwrap_or_default();
                let started = row["started_ns"].as_u64().unwrap_or_default();
                row["duration_ns"] = Value::from(duration * 7 + 1);
                row["started_ns"] = Value::from(started + 41);
                row["finished_ns"] = Value::from(started + 41 + duration * 7 + 1);
            }
            assert_eq!(
                before,
                semantic(&reclocked),
                "moving every clock stamp in the records moved a semantic column"
            );
            let label =
                "eth_call|build|before-snapshot: native and input-token balances".to_string();
            let before_fold = fold_times(&raw);
            let after_fold = fold_times(&reclocked);
            let moved = before_fold[&label].union_ns != after_fold[&label].union_ns
                && before_fold[&label].sum_ns != after_fold[&label].sum_ns;
            assert!(
                moved,
                "the clock did not move, so this control proves nothing about it"
            );
            let verdicts: BTreeSet<String> =
                candidates.iter().map(|row| txt(row, "priority")).collect();
            verdicts.len() > 1 && moved
        }
        other => panic!("{other}: not one of §25's controls"),
    }
}

fn fold_candidate_shape(candidates: &[Value]) -> Vec<(String, usize)> {
    let sites = sites_by_id();
    let pairs = pairs();
    candidates
        .iter()
        .map(|row| {
            (
                txt(row, "candidate"),
                fold_candidate(row, &sites, &pairs).exact,
            )
        })
        .collect()
}

/// §25, run rather than reported: the six controls fire against this gate's own grouping, identity
/// grammar and row rules, so the published `fired` flags are a second witness of the same six
/// outcomes rather than the only one.
#[test]
fn the_six_negative_controls_fire_on_this_gate_too() {
    let rows = rows_of(CONTROLS);
    assert_eq!(rows.len(), 6, "§25 lists six");
    let published_flags: BTreeMap<String, bool> = rows
        .iter()
        .map(|row| (txt(row, "control"), row["fired"].as_bool() == Some(true)))
        .collect();
    assert!(
        published_flags.values().all(|fired| *fired),
        "a published control says it did not fire"
    );
    assert_eq!(
        published(CONTROLS)["all_green"].as_bool(),
        Some(true),
        "six controls fired and the table's own verdict says otherwise"
    );
    let here: BTreeMap<String, bool> = [
        ("NC1", nc1_fires()),
        ("NC2", nc_fires("NC2")),
        ("NC3", nc_fires("NC3")),
        ("NC4", nc_fires("NC4")),
        ("NC5", nc_fires("NC5")),
        ("NC6", nc_fires("NC6")),
    ]
    .into_iter()
    .map(|(control, fired)| (control.to_string(), fired))
    .collect();
    for (control, fired) in &here {
        assert!(
            *fired,
            "{control}: this gate re-ran the mutation and it did not fire, so the rule it exists to \
             prove is not enforced here — and the published flag is then one implementation reading \
             itself, not a check"
        );
        assert_eq!(
            published_flags.get(control.as_str()),
            Some(fired),
            "{control}: the published flag and this re-run disagree"
        );
    }
    // Each control has to name the rule it exists to prove, or it is a mutation without a point.
    for row in &rows {
        let rule = txt(row, "rule_it_must_trip");
        assert!(rule.contains("§"), "{}: {rule:?}", row["control"]);
        assert!(
            !txt(row, "mutates").is_empty(),
            "{}: a control with nothing to mutate",
            row["control"]
        );
        // `target` is the row the control mutates, printed as the fields it reads — so a control that
        // names no field is a mutation with no object, which is the same nothing as an empty string.
        let target = row["target"].as_object();
        assert!(
            target.is_some_and(|fields| !fields.is_empty()),
            "{}: a control with nothing to mutate it on",
            row["control"]
        );
    }
    for candidate in rows_of(CANDIDATES) {
        let errors = rule_errors(&candidate);
        assert!(errors.is_empty(), "{}: {errors:?}", candidate["candidate"]);
    }
}

/// §24's fourteenth rule in the form only a recompute gate can state: a hand-edited row has to break
/// the reconciliation with the records, not merely the row's own internal rules.
#[test]
fn a_hand_edited_row_cannot_survive_the_record_reconciliation() {
    let sites = sites_by_id();
    let pairs = pairs();
    let candidates = rows_of(CANDIDATES);
    let original = published(CANDIDATES);
    assert_eq!(
        candidates
            .iter()
            .map(|row| count(row, "safe_saving"))
            .sum::<u64>(),
        0
    );

    // Edit one row the way an agent under pressure to find a saving would: raise its credit and
    // assert the proof kind that credits it. Nothing in the record moves.
    let mut edited = candidates.clone();
    let target = edited
        .iter()
        .position(|row| row["candidate"].as_str() == Some("chainid.connect_to_preflight"))
        .expect("§17's Candidate 5 leg");
    edited[target]["safe_saving"] = Value::from(1_u64);
    edited[target]["safe_reuse_proof"]["kind"] =
        Value::String("the_record_itself_marks_the_pair_safe_to_reuse".to_string());

    // 1. The asserted proof kind has nothing behind it: the fold still finds no safe pair.
    let folded = fold_candidate(&edited[target], &sites, &pairs);
    assert_eq!(
        folded.marked_safe, 0,
        "the fold found a pair the record marks safe, so this edit would be an honest correction \
         and the whole test has to be rewritten against the new corpus"
    );
    assert_eq!(
        folded.conditions_met, 0,
        "and the same for the five-condition proof"
    );
    assert_ne!(
        folded.marked_safe as u64,
        count(&edited[target], "safe_saving"),
        "an asserted kind does not create a safe pair"
    );
    // 2. The table's total stops being the arithmetic of its rows.
    let edited_total: u64 = edited.iter().map(|row| count(row, "safe_saving")).sum();
    assert_eq!(edited_total, 1);
    assert_ne!(
        edited_total,
        count(&original["totals"], "safe_saving"),
        "a one-row edit that left the total unchanged would be invisible to the totals check"
    );
    // 3. The five measures would disagree with each other: a nonzero credit against a zero total.
    assert_eq!(
        published(SAVING)["measures"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|row| row["measure"].as_str() == Some("total_safe_rpc_saving"))
            .map(|row| count(row, "value")),
        Some(0),
        "the committed §7 total, which the edit above contradicts"
    );
    // 4. A priority edit is caught the same way: the band counts are a tally of the rows, and four
    //    files print the same tally.
    let mut reprioritised = candidates.clone();
    let reject_index = reprioritised
        .iter()
        .position(|row| row["priority"].as_str() == Some("REJECT"))
        .expect("15 of the 18 rows are REJECT");
    reprioritised[reject_index]["priority"] = Value::String("P0".to_string());
    let edited_tally = tally(
        &reprioritised
            .iter()
            .map(|row| txt(row, "priority"))
            .collect::<Vec<String>>(),
    );
    assert_ne!(
        edited_tally,
        counts(&original["totals"]["by_priority"]),
        "moving one row out of REJECT has to show in the table's own band counts"
    );
    assert_eq!(
        published(VERDICT)["counts"]["by_priority"]["P0"].as_u64(),
        Some(0),
        "§14's P0 band is empty in the committed tables, so an edit that opened it would have to \
         change the tally in four files at once"
    );
    assert_eq!(
        published(MATRIX)["totals"]["safe_saving"].as_u64(),
        Some(0),
        "and the matrix repeats the same total"
    );
}

/// §24's twelfth rule, run on this gate's fold: the records in any order and the pair rows in any
/// order produce the same census and the same ceilings. Order is not an input to anything here.
#[test]
fn no_fold_in_this_gate_reads_a_position() {
    let raw = raw_rows();
    let shape = |rows: &[Value]| -> Vec<(String, u64, String)> {
        group_rows(rows)
            .iter()
            .map(|(site, members)| {
                (
                    site.label(),
                    members.len() as u64,
                    identity_of(representative(members)),
                )
            })
            .collect()
    };
    let forward = shape(&raw);
    let mut reversed = raw.clone();
    reversed.reverse();
    assert_eq!(
        forward,
        shape(&reversed),
        "reversing the records moved a group"
    );
    let mut by_clock = raw.clone();
    by_clock.sort_by_key(|row| std::cmp::Reverse(row["duration_ns"].as_u64().unwrap_or_default()));
    // The permutation has to be a real permutation, or the equality below proves nothing: compare
    // the order of the records themselves, not the census.
    let order = |rows: &[Value]| -> Vec<(String, u64, u64)> {
        rows.iter()
            .map(|row| {
                (
                    txt(row, "run"),
                    row["rpc_id"].as_u64().unwrap_or_default(),
                    row["duration_ns"].as_u64().unwrap_or_default(),
                )
            })
            .collect()
    };
    assert_ne!(
        order(&raw),
        order(&by_clock),
        "the records were already duration-sorted, so this control did not move anything"
    );
    assert_eq!(
        forward,
        shape(&by_clock),
        "a duration-sorted read of the same records produced a different census"
    );
    let mut by_method = raw.clone();
    by_method.sort_by_key(|row| {
        (
            txt(row, "method"),
            row["rpc_id"].as_u64().unwrap_or_default(),
        )
    });
    assert_eq!(forward, shape(&by_method));
    // The queue's sort key: five terms, none of them a duration, and the slot where a clock would
    // sit holds the constant that §24's tenth rule put there.
    let queue_sort = published(QUEUE)["sort"].clone();
    assert_eq!(
        queue_sort["duration_used_as_sort_key"].as_bool(),
        Some(false),
        "§21's queue says it sorts without a clock"
    );
    let keys = queue_sort["keys_in_order"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for row in rows_of(QUEUE) {
        let id = txt(&row, "candidate");
        let key = row["sort_key"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            key.len(),
            keys.len(),
            "{id}: the sort key and the sort key's description are different lengths"
        );
        assert_eq!(
            key[1].as_u64(),
            Some(u64::MAX),
            "{id}: the queue's second sort term is the constant that replaces a duration, and this \
             row prints {:?}",
            key[1]
        );
        assert_eq!(
            id,
            key[4].as_str().unwrap_or_default(),
            "{id}: the last sort term is the candidate's own name"
        );
        for (term, name) in key.iter().zip(keys.iter()) {
            let rendered = term.to_string();
            for forbidden in ["duration", "elapsed", "ms", "ns"] {
                assert!(
                    !rendered.contains(forbidden) && !name.to_string().contains(forbidden),
                    "{id}: a sort term mentions {forbidden}: {rendered} / {name}"
                );
            }
        }
    }
    // §24's tenth rule as a figure this gate folds itself: an inversion is a pair of candidates
    // where the slower one carries the worse band. A clock order would have none.
    let band = |label: &str| -> u64 {
        match label {
            "P0" => 0,
            "P1" => 1,
            "P2" => 2,
            "P3" => 3,
            "REJECT" => 4,
            other => panic!("{other}: not one of §14's five bands"),
        }
    };
    let measured: Vec<(f64, u64)> = rows_of(MATRIX)
        .iter()
        .map(|row| {
            (
                row["total_duration_ms"].as_f64().unwrap_or_default(),
                band(&txt(row, "priority")),
            )
        })
        .collect();
    let mut inversions = 0_usize;
    for i in 0..measured.len() {
        for j in i + 1..measured.len() {
            let (a, b) = (&measured[i], &measured[j]);
            if (a.0 > b.0 && a.1 > b.1) || (b.0 > a.0 && b.1 > a.1) {
                inversions += 1;
            }
        }
    }
    assert_eq!(
        published(MATRIX)["duration_is_not_a_rank"]["inversions_found"].as_u64(),
        Some(inversions as u64),
        "{inversions} inversions folded here, and the matrix publishes a different figure"
    );
    assert!(
        inversions > 0,
        "no candidate pair disagrees with the clock, so this table could have been ranked by \
         duration and §24's tenth rule would be unverifiable"
    );
    // Reading the same rows in duration order does not produce the band order.
    let mut by_slowest = measured.clone();
    by_slowest.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let bands: Vec<u64> = by_slowest.iter().map(|(_, b)| *b).collect();
    assert!(
        bands.windows(2).any(|pair| pair[0] > pair[1])
            && bands.windows(2).any(|pair| pair[0] < pair[1]),
        "the duration order is monotone in the priority bands, which is what §24's tenth rule \
         forbids: {bands:?}"
    );
}

// ---------------------------------------------------------------------------
// §26 and §22: nothing spent, nothing decided by a clock
// ---------------------------------------------------------------------------

/// §26's `new_rpc = 0`, §22's one label, §3's prohibitions, and §17's marked-but-not-run experiment
/// — read out of all ten files rather than quoted from one of them.
#[test]
fn nothing_here_was_measured_by_asking_the_node() {
    for name in FILES {
        let table = published(name);
        let new_rpc = if name == VERDICT {
            table["verdict_block"]["new_rpc"].as_u64()
        } else {
            table["new_rpc"].as_u64()
        };
        assert_eq!(
            new_rpc,
            Some(0),
            "{name}: §26 asks for new_rpc = 0 and the file prints {new_rpc:?}"
        );
        assert_eq!(
            table["milestone"].as_str(),
            Some("M8.6"),
            "{name}: a file from another milestone is in this directory"
        );
        if name != VERDICT {
            assert!(
                txt(&table, "evidence_first_rule").starts_with("§2.4"),
                "{name}: every table but the verdict states §2.4's evidence-first rule in its own \
                 header"
            );
        }
    }
    let verdict = published(VERDICT);
    let block = &verdict["verdict_block"];
    assert_eq!(count(block, "broadcasts"), 0);
    assert_eq!(count(block, "signatures"), 0);
    assert_eq!(count(block, "real_arbitrage"), 0);
    let label = txt(&verdict, "label");
    assert_eq!(label, "NO_SAFE_RPC_REDUCTION_FOUND", "§22's one label");
    assert_eq!(block["M8.6 RESULT"].as_str(), Some(label.as_str()));
    assert_eq!(block["label"].as_str(), Some(label.as_str()));
    let reached: u64 = published(CANDIDATES)["totals"]["by_verdict"]
        .as_object()
        .map(|bands| {
            bands
                .iter()
                .filter(|(verdict_name, _)| **verdict_name == label)
                .map(|(_, cells)| cells.as_u64().unwrap_or_default())
                .sum()
        })
        .unwrap_or_default();
    assert!(
        reached > 0,
        "the label the milestone reached is not a verdict any candidate reached"
    );
    assert!(
        verdict["candidates_found"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a candidate was found while the label still says none was"
    );
    // §17's Candidate 1: marked, priced, and not run.
    let insufficient = verdict["insufficient_evidence"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let oracle = insufficient
        .iter()
        .find(|row| row["candidate"].as_str() == Some("l1fee.preflight_oracle"))
        .expect("§17's Candidate 1 stays marked");
    assert_eq!(oracle["next_step"].as_str(), Some("EXPERIMENT_REQUIRED"));
    assert!(
        count(oracle, "would_cost") > 0,
        "the experiment is priced, which is what makes the marking a refusal rather than a phrase"
    );
    let l1fee = rows_of(CANDIDATES)
        .into_iter()
        .find(|row| row["candidate"].as_str() == Some("l1fee.preflight_oracle"))
        .expect("Candidate 1's row");
    assert_eq!(
        l1fee["next_step"].as_str(),
        Some("EXPERIMENT_REQUIRED"),
        "the candidate row and the verdict table must not disagree about what happens next"
    );
    assert_eq!(
        count(&l1fee, "safe_saving"),
        0,
        "and nothing was credited for it"
    );
    assert_eq!(l1fee["requires_new_experiment"].as_bool(), Some(true));
    // No clock key inside any §14 clause, and the timing block disclaims itself on every row.
    for candidate in rows_of(CANDIDATES) {
        let id = txt(&candidate, "candidate");
        let clauses = candidate["priority_derivation"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !clauses.is_empty(),
            "{id}: a priority with no derivation printed"
        );
        for clause in &clauses {
            let printed = serde_json::to_string(clause).unwrap_or_default();
            for needle in ["duration", "elapsed", "generated_at", "started_ns"] {
                assert!(
                    !printed.contains(needle),
                    "{id}: a §14 clause reads a clock — {printed}"
                );
            }
        }
        // Every row carries the disclaimer as an object: the durations it prints are diagnostic, and
        // the object's own rule string is what says so. A row that printed timings with no rule beside
        // them would leave §24's tenth rule to the reader's good intentions.
        let timing = candidate["timing_is_diagnostic_only"]
            .as_object()
            .unwrap_or_else(|| panic!("{id}: the timing block is not an object"));
        assert!(
            timing.contains_key("rule"),
            "{id}: the timing block prints durations with no rule disclaiming them"
        );
        let rule = candidate["timing_is_diagnostic_only"]["rule"]
            .as_str()
            .unwrap_or_default();
        assert!(
            rule.contains("§24's tenth rule"),
            "{id}: the timing block does not disclaim itself"
        );
    }
}

/// The last thing two implementations can settle between them: that the ten files agree with each
/// other on every figure they repeat. A number that appears in three tables and is edited in one is
/// a contradiction this gate can see without the records at all.
#[test]
fn the_ten_files_repeat_the_same_figures_to_each_other() {
    let candidates = rows_of(CANDIDATES);
    let matrix = rows_of(MATRIX);
    let queue = rows_of(QUEUE);
    let census = rows_of(CENSUS);
    let surface = rows_of(SURFACE);
    let rejected = rows_of(REJECTED);

    let by_name: BTreeMap<String, &Value> = candidates
        .iter()
        .map(|row| (txt(row, "candidate"), row))
        .collect();
    assert_eq!(by_name.len(), candidates.len(), "two candidates, one name");
    for table in [&matrix, &queue] {
        assert_eq!(
            table.len(),
            candidates.len(),
            "every §6 candidate is in every table that ranks candidates"
        );
        for row in table {
            let id = txt(row, "candidate");
            let candidate = by_name
                .get(&id)
                .unwrap_or_else(|| panic!("{id}: a row with no candidate behind it"));
            for name in ["priority", "verdict", "safe_saving", "theoretical_saving"] {
                assert_eq!(
                    row[name], candidate[name],
                    "{id}: the {name} here is not the candidate's own"
                );
            }
            assert_eq!(
                count(row, "avoidable_count"),
                count(candidate, "avoidable_rpc_count").min(count(row, "theoretical_saving")),
                "{id}: the avoidable count is the ceiling wherever the ceiling is the smaller"
            );
        }
    }
    let census_duplicates: u64 = census.iter().map(|row| count(row, "duplicate_count")).sum();
    let candidate_ceiling: u64 = candidates
        .iter()
        .map(|row| count(row, "theoretical_saving"))
        .sum();
    assert_eq!(
        census_duplicates, candidate_ceiling,
        "the census counts {census_duplicates} duplicate asks and the candidates claim a ceiling of \
         {candidate_ceiling}"
    );
    let census_sites: BTreeSet<String> = census.iter().map(|row| txt(row, "site_id")).collect();
    let surface_sites: BTreeSet<String> = surface.iter().map(|row| txt(row, "site_id")).collect();
    assert_eq!(census_sites, surface_sites, "one site in one table only");
    for line in &surface {
        let site = txt(line, "site_id");
        let twin = census
            .iter()
            .find(|row| row["site_id"].as_str() == Some(site.as_str()))
            .unwrap_or_else(|| panic!("{site}: a surface row with no census row"));
        assert_eq!(
            opt_txt(line, "verification_role"),
            opt_txt(twin, "verification_role"),
            "{site}: the two tables grade the same site's duty differently"
        );
        assert_eq!(
            opt_txt(line, "semantic_class"),
            opt_txt(twin, "semantic_class"),
            "{site}: two §5 classes for one site"
        );
        assert_eq!(
            opt_txt(line, "carrier"),
            opt_txt(twin, "carrier"),
            "{site}: two §10 carriers for one site"
        );
    }
    let patterns: BTreeSet<String> = candidates.iter().map(|row| txt(row, "pattern")).collect();
    assert_eq!(
        patterns.len(),
        6,
        "§8's five patterns plus §10's information-already-held: {patterns:?}"
    );
    let roles: BTreeSet<String> = candidates
        .iter()
        .map(|row| txt(row, "verification_role"))
        .collect();
    assert!(
        roles.len() > 1,
        "one verification role across all 18 candidates would make §11 a single answer"
    );
    let classes: BTreeSet<String> = census
        .iter()
        .map(|row| txt(row, "semantic_class"))
        .collect();
    assert!(
        classes.len() >= 5,
        "§5 asks for nine classes to be told apart; the census uses {classes:?}"
    );
    // §15 and §16: the two verdicts this census inherits rather than re-asks. The check is against
    // the records they were earned in, not against a table that repeats them.
    let inherited: Vec<Value> = published(SAVING)["inherited_verdicts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let labelled: Vec<&Value> = inherited
        .iter()
        .filter(|verdict| verdict["label"].is_string())
        .collect();
    assert_eq!(
        labelled.len(),
        2,
        "§15's REUSE_BLOCKED and §16's propagation verdict, found {inherited:?}"
    );
    for verdict in &labelled {
        let label = txt(verdict, "label");
        let source = txt(verdict, "source");
        let record = read_json(&source);
        let source_text = std::fs::read_to_string(repo().join(&source))
            .unwrap_or_else(|error| panic!("{}: {error}", source));
        let mut zeros: Vec<(String, u64)> = Vec::new();
        rpc_zero_fields(&record, "", &mut zeros);
        assert!(
            !zeros.is_empty(),
            "{label}: the record at {source} reports no RPC-saving figure at all, so there is \
             nothing here to inherit a zero from"
        );
        assert!(
            zeros.iter().all(|(_, value)| *value == 0),
            "{label}: {source} states {zeros:?}, which is not the zero-saving verdict §15/§16 inherits"
        );
        let in_source = source_text.contains(&label);
        match verdict["label_recorded_in"].as_str() {
            Some(path) => {
                let text = std::fs::read_to_string(repo().join(path))
                    .unwrap_or_else(|error| panic!("{path}: {error}"));
                assert!(
                    text.contains(&label),
                    "{label}: this entry points at {path} for the wording and that file does not \
                     contain it"
                );
            }
            None => assert!(
                in_source,
                "{label}: {source} does not state the label this entry says it inherits"
            ),
        }
        assert!(
            in_source || verdict["label_recorded_in"].is_string(),
            "{label}: the entry cites {source}, which does not state it, and names no file that does"
        );
        let saved: Vec<u64> = [
            "aggregate_net_rpc_saved",
            "net_rpc_saving",
            "rpc_count_reduction",
        ]
        .iter()
        .filter_map(|field| verdict[*field].as_u64())
        .collect();
        assert!(
            !saved.is_empty() && saved.iter().all(|value| *value == 0),
            "{label}: the entry's own net figure is {saved:?}"
        );
    }
    // Where the inheritance lands: §15's label on the eth_call rejections, §16's on every Pattern C
    // rejection.
    let mut reuse_blocked = 0_usize;
    let mut propagation = 0_usize;
    for row in &rejected {
        let name = txt(row, "candidate");
        let label = row["inherited_verdict"]["label"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if name.contains("eth_call") {
            assert_eq!(
                label, "REUSE_BLOCKED",
                "{name}: §15 says this family is inherited, and the row says {label:?}"
            );
            reuse_blocked += 1;
        }
        if txt(row, "pattern") == "C_block_context_propagation" {
            assert_eq!(
                label, "SAFE_PROPAGATION_PROVEN_NO_NET_RPC_SAVING",
                "{name}: a §8 Pattern C rejection with no §16 verdict on it"
            );
            propagation += 1;
        }
        assert!(
            label.is_empty() || count(row, "safe_saving") == 0,
            "{name}: an inherited verdict {label} alongside a credited saving"
        );
    }
    assert_eq!(
        reuse_blocked, 2,
        "§15 names the two eth_call legs and no other row"
    );
    assert_eq!(
        propagation, 5,
        "§8's Pattern C has five consumers in the records and all five have to be answered by §16"
    );
    for row in &candidates {
        if row["method"].as_str() != Some("eth_call") {
            continue;
        }
        let id = txt(row, "candidate");
        assert_eq!(
            count(row, "safe_saving"),
            0,
            "{id}: M8.5.1 graded this family REUSE_BLOCKED and §15 forbids re-opening it"
        );
        // §14 forbids reading the Detection→Preflight `eth_call` pair as anything but REJECT, and §17
        // carves out exactly one eth_call-shaped row — the L1-fee oracle — whose missing evidence is a
        // proposition no record can settle, so its band is P1 with the experiment named and unspent.
        // The carve-out is read from the row's own `next_step`, not from its name, so a future row
        // cannot claim P1 by being renamed.
        let experiment = txt(row, "next_step") == "EXPERIMENT_REQUIRED";
        if experiment {
            assert_eq!(
                txt(row, "priority"),
                "P1",
                "{id}: §14's P1 is the band for a strong candidate that needs one controlled experiment"
            );
            assert_eq!(
                row["requires_new_experiment"].as_bool(),
                Some(true),
                "{id}: an EXPERIMENT_REQUIRED row that does not say the experiment is what blocks it"
            );
        } else {
            assert_eq!(
                txt(row, "priority"),
                "REJECT",
                "{id}: §14 names the eth_call family and forbids reading it as anything but REJECT"
            );
        }
    }
    let l1fee: Vec<&Value> = candidates
        .iter()
        .filter(|row| txt(row, "next_step") == "EXPERIMENT_REQUIRED")
        .collect();
    assert_eq!(
        l1fee.len(),
        1,
        "§17 names one hypothesis for a controlled experiment, and no record justifies a second"
    );
    assert_eq!(
        txt(l1fee[0], "candidate"),
        "l1fee.preflight_oracle",
        "§17's Candidate 1 is the L1-fee oracle read"
    );
    assert_eq!(
        published(CANDIDATES)["new_rpc"].as_u64(),
        Some(0),
        "§26: the experiment is named, not run — the census spends no RPC it did not already spend"
    );
}
